//! Pure policy checks for torrent input and the shared bandwidth budget.
//! No network, no DNS, no librqbit calls.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const MAGNET_PREFIX: &str = "magnet:?";
const MAGNET_MAX_LEN: usize = 8192;
const TORRENT_FILE_MAX: u64 = 10 * 1024 * 1024;
const OVERSHOOT_WINDOW: usize = 10;

/// Returns true when a tracker URL is safe to contact.
///
/// Only `http`, `https` and `udp` are accepted. The caller must resolve the
/// URL host itself and pass every resolved address in `resolved`; this
/// function performs no DNS. If the host is an IP literal, the literal is
/// checked directly and `resolved` is ignored. Private, loopback, link-local,
/// multicast, unspecified and unique-local addresses are rejected, as are the
/// host names `localhost` and anything ending in `.local`.
pub fn tracker_allowed(url: &str, resolved: &[IpAddr]) -> bool {
    let (scheme, rest) = match url.split_once("://") {
        Some(parts) => parts,
        None => return false,
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https" | "udp") {
        return false;
    }

    let authority = rest
        .split(|c: char| matches!(c, '/' | '?' | '#'))
        .next()
        .unwrap_or("");
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(inner) = hostport.strip_prefix('[') {
        match inner.split_once(']') {
            Some((h, _)) => h,
            None => return false,
        }
    } else {
        hostport.rsplit_once(':').map_or(hostport, |(h, _)| h)
    };
    if host.is_empty() {
        return false;
    }

    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".local") {
        return false;
    }

    match host.parse::<IpAddr>() {
        Ok(ip) => ip_is_public(&ip),
        Err(_) => !resolved.is_empty() && resolved.iter().all(ip_is_public),
    }
}

fn ip_is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_is_public(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4_is_public(&v4),
            None => v6_is_public(v6),
        },
    }
}

fn v4_is_public(ip: &Ipv4Addr) -> bool {
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_broadcast())
}

fn v6_is_public(ip: &Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    let link_local = (first & 0xffc0) == 0xfe80;
    let unique_local = (first & 0xfe00) == 0xfc00;
    !(ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || link_local || unique_local)
}

/// Returns true for a magnet URI that starts with `magnet:?`, carries a
/// non-empty `xt=urn:btih:` parameter, and is at most 8192 bytes long.
pub fn magnet_ok(s: &str) -> bool {
    if s.len() > MAGNET_MAX_LEN {
        return false;
    }
    match s.strip_prefix(MAGNET_PREFIX) {
        Some(query) => query.split('&').any(|param| {
            param
                .strip_prefix("xt=urn:btih:")
                .is_some_and(|hash| !hash.is_empty())
        }),
        None => false,
    }
}

/// Returns true for a `.torrent` file size between 1 byte and 10 MiB inclusive.
pub fn torrent_file_ok(len: u64) -> bool {
    (1..=TORRENT_FILE_MAX).contains(&len)
}

/// Bandwidth budget left for torrents after HTTP traffic.
///
/// Returns `None` when there is no total cap. Otherwise the budget is the cap
/// minus the smoothed HTTP rate, never below 10% of the cap and never above
/// the cap.
pub fn torrent_budget(total_cap_bps: Option<u64>, http_bps_smoothed: u64) -> Option<u64> {
    let cap = total_cap_bps?;
    let floor = cap / 10;
    let remaining = cap.saturating_sub(http_bps_smoothed);
    Some(remaining.max(floor).min(cap))
}

/// Returns true when a new budget should be pushed to the workers.
///
/// Sends when the two values differ by at least 5% of `cap_bps`, or when
/// exactly one of them is `None`.
pub fn should_send_budget(previous: Option<u64>, next: Option<u64>, cap_bps: u64) -> bool {
    match (previous, next) {
        (None, None) => false,
        (Some(a), Some(b)) => {
            (a.abs_diff(b) as u128) * 100 >= (cap_bps as u128) * 5
        }
        _ => true,
    }
}

/// Overshoot check over per-second samples.
///
/// The sum of any 10 consecutive samples must be at most 1.05 times
/// `cap_bps`. If there are fewer than 10 samples, the whole series is checked
/// as one window.
pub fn within_cap(samples_bps: &[u64], cap_bps: u64) -> bool {
    let n = samples_bps.len().min(OVERSHOOT_WINDOW);
    if n == 0 {
        return true;
    }
    samples_bps.windows(n).all(|w| {
        let sum: u128 = w.iter().map(|&s| s as u128).sum();
        // The allowance scales with the window: n seconds at 105% of the cap.
        sum * 100 <= (cap_bps as u128) * (n as u128) * 105
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[test]
    fn tracker_public_ip_literal_allowed() {
        assert!(tracker_allowed("http://8.8.8.8:80/announce", &[]));
    }

    #[test]
    fn tracker_loopback_rejected() {
        assert!(!tracker_allowed("http://127.0.0.1:80/announce", &[]));
    }

    #[test]
    fn tracker_private_rejected() {
        assert!(!tracker_allowed("http://10.0.0.5/announce", &[]));
    }

    #[test]
    fn tracker_ipv6_link_local_rejected() {
        assert!(!tracker_allowed("http://[fe80::1]:6969/announce", &[]));
    }

    #[test]
    fn tracker_ipv6_unique_local_rejected() {
        assert!(!tracker_allowed("http://[fd00::1]:6969/announce", &[]));
    }

    #[test]
    fn tracker_ipv4_mapped_loopback_rejected() {
        assert!(!tracker_allowed("http://[::ffff:127.0.0.1]/announce", &[]));
    }

    #[test]
    fn tracker_localhost_rejected() {
        assert!(!tracker_allowed(
            "http://localhost:80/announce",
            &[v4(8, 8, 8, 8)]
        ));
    }

    #[test]
    fn tracker_dot_local_rejected() {
        assert!(!tracker_allowed(
            "https://tracker.local/announce",
            &[v4(8, 8, 8, 8)]
        ));
    }

    #[test]
    fn tracker_udp_public_resolved_allowed() {
        assert!(tracker_allowed(
            "udp://tracker.example:6969",
            &[v4(8, 8, 4, 4)]
        ));
    }

    #[test]
    fn tracker_domain_resolving_private_rejected() {
        assert!(!tracker_allowed(
            "https://tracker.example/announce",
            &[v4(8, 8, 4, 4), v4(10, 1, 1, 1)]
        ));
    }

    #[test]
    fn tracker_domain_without_resolution_rejected() {
        assert!(!tracker_allowed("https://tracker.example/announce", &[]));
    }

    #[test]
    fn tracker_ftp_scheme_rejected() {
        assert!(!tracker_allowed(
            "ftp://tracker.example/announce",
            &[v4(8, 8, 4, 4)]
        ));
    }

    #[test]
    fn magnet_valid() {
        assert!(magnet_ok("magnet:?xt=urn:btih:abcdef0123&dn=name"));
    }

    #[test]
    fn magnet_missing_btih_rejected() {
        assert!(!magnet_ok("magnet:?dn=name"));
        assert!(!magnet_ok("magnet:?xt=urn:btmh:abcdef"));
    }

    #[test]
    fn magnet_empty_hash_rejected() {
        assert!(!magnet_ok("magnet:?xt=urn:btih:"));
    }

    #[test]
    fn magnet_wrong_prefix_rejected() {
        assert!(!magnet_ok("http://example.com/?xt=urn:btih:abc"));
    }

    #[test]
    fn magnet_length_limit() {
        let at_limit = format!("magnet:?xt=urn:btih:{}", "a".repeat(8192 - 20));
        assert_eq!(at_limit.len(), 8192);
        assert!(magnet_ok(&at_limit));
        let over = format!("{at_limit}a");
        assert!(!magnet_ok(&over));
    }

    #[test]
    fn torrent_file_size_bounds() {
        assert!(!torrent_file_ok(0));
        assert!(torrent_file_ok(1));
        assert!(torrent_file_ok(10 * 1024 * 1024));
        assert!(!torrent_file_ok(10 * 1024 * 1024 + 1));
    }

    #[test]
    fn budget_no_cap_is_none() {
        assert_eq!(torrent_budget(None, 12345), None);
    }

    #[test]
    fn budget_http_uses_whole_cap_keeps_ten_percent() {
        assert_eq!(torrent_budget(Some(1000), 1000), Some(100));
        assert_eq!(torrent_budget(Some(1000), 5000), Some(100));
    }

    #[test]
    fn budget_never_exceeds_cap() {
        assert_eq!(torrent_budget(Some(1000), 0), Some(1000));
    }

    #[test]
    fn budget_subtracts_http_rate() {
        assert_eq!(torrent_budget(Some(1000), 300), Some(700));
    }

    #[test]
    fn send_small_change_suppressed() {
        // 200 bps change on a 10000 bps cap is 2%.
        assert!(!should_send_budget(Some(5000), Some(5200), 10000));
    }

    #[test]
    fn send_six_percent_change_sent() {
        // 600 bps change on a 10000 bps cap is 6%.
        assert!(should_send_budget(Some(5000), Some(5600), 10000));
    }

    #[test]
    fn send_on_none_transition() {
        assert!(should_send_budget(None, Some(1), 10000));
        assert!(should_send_budget(Some(1), None, 10000));
        assert!(!should_send_budget(None, None, 10000));
    }

    #[test]
    fn steady_cap_within_cap() {
        assert!(within_cap(&vec![1000u64; 30], 1000));
    }

    #[test]
    fn exactly_five_percent_over_is_within_cap() {
        assert!(within_cap(&vec![1050u64; 10], 1000));
        assert!(!within_cap(&vec![1051u64; 10], 1000));
    }

    #[test]
    fn two_x_burst_for_three_seconds_fails() {
        let samples: Vec<u64> = [vec![1000u64; 10], vec![2000u64; 3], vec![1000u64; 7]].concat();
        assert!(!within_cap(&samples, 1000));
    }

    #[test]
    fn short_series_is_checked_as_one_window() {
        assert!(within_cap(&[1000, 1000, 1000], 1000));
        assert!(!within_cap(&[2000, 2000, 2000], 1000));
    }
}
