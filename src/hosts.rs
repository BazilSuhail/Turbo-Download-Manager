//! Per-host learned connection limits.
//!
//! Some servers refuse connections past a certain count (503 or reset). This
//! module remembers that count per host so the next download starts below it,
//! instead of retrying the same number. No network, no file IO. The clock is
//! read only to stamp and expire learned caps. The store persists the map
//! through `to_map` / `from_map`.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How long a learned cap is trusted. Hosts change their limits, so an old
/// refusal is forgotten and the next download probes the host again.
pub const CAP_TTL_SECS: u64 = 24 * 60 * 60;

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Whether a record learned at `learned_at` has outlived its trust window.
fn expired(learned_at: u64, now: u64) -> bool {
    now > learned_at.saturating_add(CAP_TTL_SECS)
}

/// What is known about one host.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct HostRecord {
    /// Learned maximum connection count, if the host refused a higher one.
    pub cap: Option<u32>,
    /// Connection count at which the host last refused a new connection.
    pub refused_at: Option<u32>,
    /// Highest connection count seen working on this host.
    pub working: u32,
    /// When the cap was learned (unix seconds). Stamped when a refusal lowers
    /// the cap; `cap()` ignores the cap once it is older than `CAP_TTL_SECS`.
    pub learned_at_unix: u64,
}

/// Learned limits for all hosts.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct HostLimits {
    map: BTreeMap<String, HostRecord>,
}

impl HostLimits {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from a stored map.
    pub fn from_map(map: BTreeMap<String, HostRecord>) -> Self {
        Self { map }
    }

    /// Copy out the map, for serialization by the store.
    pub fn to_map(&self) -> BTreeMap<String, HostRecord> {
        self.map.clone()
    }

    /// Connection count to use for `host`.
    ///
    /// Uses the learned cap when there is one, otherwise `requested`. The
    /// result is clamped to `hard_max` and is never below 1. If `hard_max`
    /// is 0, the result is still 1.
    pub fn cap(&self, host: &str, requested: u32, hard_max: u32) -> u32 {
        let now = now_unix();
        let base = self
            .map
            .get(&normalize(host))
            .filter(|r| !expired(r.learned_at_unix, now))
            .and_then(|r| r.cap)
            .unwrap_or(requested);
        base.min(hard_max).max(1)
    }

    /// The host refused a connection while `at_connections` were open.
    /// The learned cap becomes one below that count, never below 1.
    ///
    /// A refusal never raises a cap that is still in force: the cap only goes
    /// down, and only a refusal at a count it would not already allow sets it.
    pub fn on_refused(&mut self, host: &str, at_connections: u32) {
        let now = now_unix();
        let learned = at_connections.saturating_sub(1).max(1);
        let rec = self.map.entry(normalize(host)).or_default();
        let in_force = rec.cap.filter(|_| !expired(rec.learned_at_unix, now));
        match in_force {
            Some(cap) if cap <= learned => {}
            _ => {
                rec.cap = Some(learned);
                rec.refused_at = Some(at_connections);
                rec.learned_at_unix = now;
            }
        }
    }

    /// `connections` worked on `host`.
    ///
    /// Raises `working`. A learned cap is cleared only when a count above the
    /// refusal point works, which shows the host's limit has changed.
    pub fn on_working(&mut self, host: &str, connections: u32) {
        let rec = self.map.entry(normalize(host)).or_default();
        rec.working = rec.working.max(connections);
        if let Some(refused) = rec.refused_at {
            if connections > refused {
                rec.cap = None;
                rec.refused_at = None;
            }
        }
    }

    /// Key for `url`: the lowercased host without port. Empty if the URL
    /// does not parse or has no host.
    pub fn host_key(url: &str) -> String {
        reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
            .unwrap_or_default()
    }
}

/// Keys are lowercased so callers may pass a raw host or a `host_key` result.
fn normalize(host: &str) -> String {
    host.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_is_clamped_to_hard_max() {
        let mut l = HostLimits::new();
        assert_eq!(l.cap("a.example", 32, 8), 8);
        l.on_refused("a.example", 50);
        assert_eq!(l.cap("a.example", 32, 8), 8);
        // No learned cap for this host: the requested count is used, clamped.
        assert_eq!(l.cap("b.example", 4, 8), 4);
        assert_eq!(l.cap("b.example", 32, 8), 8);
    }

    #[test]
    fn refusal_at_five_lowers_cap_to_four() {
        let mut l = HostLimits::new();
        l.on_refused("h.example", 5);
        assert_eq!(l.cap("h.example", 8, 16), 4);
        assert_eq!(l.map["h.example"].refused_at, Some(5));
    }

    #[test]
    fn success_at_four_keeps_cap_after_refusal_at_five() {
        let mut l = HostLimits::new();
        l.on_refused("h.example", 5);
        l.on_working("h.example", 4);
        assert_eq!(l.cap("h.example", 8, 16), 4);
        assert_eq!(l.map["h.example"].working, 4);
    }

    #[test]
    fn higher_success_after_refusal_clears_cap() {
        let mut l = HostLimits::new();
        l.on_refused("h.example", 5);
        l.on_working("h.example", 6);
        assert_eq!(l.cap("h.example", 8, 16), 8);
        assert_eq!(l.map["h.example"].cap, None);
    }

    #[test]
    fn a_refusal_cannot_raise_a_cap() {
        let mut l = HostLimits::new();
        l.on_refused("h.example", 5);
        l.on_refused("h.example", 50);
        assert_eq!(l.cap("h.example", 32, 32), 4);
        assert_eq!(l.map["h.example"].refused_at, Some(5));
    }

    #[test]
    fn a_lower_refusal_lowers_the_cap() {
        let mut l = HostLimits::new();
        l.on_refused("h.example", 9);
        l.on_refused("h.example", 4);
        assert_eq!(l.cap("h.example", 32, 32), 3);
    }

    #[test]
    fn an_expired_cap_is_ignored() {
        let mut l = HostLimits::new();
        l.on_refused("h.example", 5);
        l.map.get_mut("h.example").expect("record").learned_at_unix =
            now_unix() - CAP_TTL_SECS - 1;
        assert_eq!(l.cap("h.example", 8, 16), 8);
    }

    #[test]
    fn a_refusal_after_expiry_sets_a_fresh_cap() {
        let mut l = HostLimits::new();
        l.on_refused("h.example", 5);
        l.map.get_mut("h.example").expect("record").learned_at_unix =
            now_unix() - CAP_TTL_SECS - 1;
        l.on_refused("h.example", 9);
        assert_eq!(l.cap("h.example", 16, 16), 8);
    }

    #[test]
    fn invalid_url_gives_empty_key() {
        assert_eq!(HostLimits::host_key("not a url"), "");
        assert_eq!(HostLimits::host_key(""), "");
        assert_eq!(HostLimits::host_key("mailto:someone"), "");
    }

    #[test]
    fn host_key_drops_port_and_lowercases() {
        assert_eq!(
            HostLimits::host_key("https://Files.Example.COM:8443/a/b.zip?x=1"),
            "files.example.com"
        );
    }

    #[test]
    fn cap_is_never_zero() {
        let mut l = HostLimits::new();
        l.on_refused("h.example", 1);
        assert_eq!(l.cap("h.example", 8, 16), 1);
        l.on_refused("h.example", 0);
        assert_eq!(l.cap("h.example", 8, 16), 1);
        assert_eq!(l.cap("h.example", 0, 0), 1);
    }

    #[test]
    fn map_round_trip() {
        let mut l = HostLimits::new();
        l.on_refused("h.example", 3);
        let back = HostLimits::from_map(l.to_map());
        assert_eq!(back, l);
    }
}
