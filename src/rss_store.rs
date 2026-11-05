//! Per-feed polling glue: when a feed is due, where its seen set lives, and a
//! single async call that fetches, filters and saves the seen state.

use std::path::PathBuf;

use crate::model::RssFeed;
use crate::rss::{self, Found, Rule, Seen};

/// True when `feed` should be polled now.
///
/// A disabled feed is never due. A feed never polled before is due. Otherwise
/// the feed is due once `clamped_interval()` minutes have passed since
/// `last_poll_unix`. If the clock went backwards (`now < last`), the feed is
/// due, so a clock change cannot stop polling for a long time.
pub fn due(feed: &RssFeed, last_poll_unix: Option<u64>, now_unix: u64) -> bool {
    if !feed.enabled {
        return false;
    }
    let Some(last) = last_poll_unix else {
        return true;
    };
    if now_unix < last {
        return true;
    }
    let interval_secs = u64::from(feed.clamped_interval()) * 60;
    now_unix - last >= interval_secs
}

/// Where the seen set for one feed is stored: `<app dir>/rss/seen-<id>.json`.
pub fn seen_path(feed_id: u64) -> PathBuf {
    crate::store::app_dir()
        .join("rss")
        .join(format!("seen-{feed_id}.json"))
}

/// Deletes the seen set of a removed feed. A new feed reuses the lowest free id
/// (max + 1), so without this it would inherit the old feed's seen links and
/// skip them. A seen file that is already gone is not an error.
pub fn purge_seen(feed_id: u64) -> std::io::Result<()> {
    match std::fs::remove_file(seen_path(feed_id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Rejects anything that is not an http or https URL.
fn check_feed_url(raw: &str) -> Result<(), String> {
    let ok = reqwest::Url::parse(raw.trim())
        .map(|u| matches!(u.scheme(), "http" | "https"))
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(format!("feed URL is not http or https: {raw}"))
    }
}

/// Polls one feed. Loads its seen set, returns the new matching items, and
/// saves the seen set before returning.
pub async fn poll_feed(feed: &RssFeed) -> Result<Vec<Found>, String> {
    check_feed_url(&feed.url)?;
    let path = seen_path(feed.id);
    let mut seen = Seen::load(&path).map_err(|e| e.to_string())?;
    let rules: Vec<Rule> = feed
        .rules
        .iter()
        .map(|text| Rule {
            contains: text.clone(),
            episode_min: None,
        })
        .collect();
    let result = rss::poll(&feed.url, &rules, &mut seen).await;
    seen.save(&path).map_err(|e| e.to_string())?;
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(enabled: bool, interval_minutes: u32) -> RssFeed {
        RssFeed {
            id: 7,
            url: "https://example.com/feed.xml".into(),
            rules: vec!["big show".into()],
            dir: PathBuf::from("downloads"),
            enabled,
            interval_minutes,
        }
    }

    #[test]
    fn never_polled_is_due() {
        assert!(due(&feed(true, 30), None, 1_000));
    }

    #[test]
    fn not_yet_due_before_interval() {
        // 30 minutes = 1800 seconds.
        assert!(!due(&feed(true, 30), Some(1_000), 1_000 + 1_799));
    }

    #[test]
    fn due_exactly_at_interval() {
        assert!(due(&feed(true, 30), Some(1_000), 1_000 + 1_800));
    }

    #[test]
    fn clock_went_backwards_is_due() {
        assert!(due(&feed(true, 30), Some(5_000), 4_000));
    }

    #[test]
    fn disabled_feed_is_never_due() {
        assert!(!due(&feed(false, 30), None, 1_000));
        assert!(!due(&feed(false, 30), Some(1_000), 1_000 + 86_400));
    }

    #[test]
    fn interval_is_clamped_to_five_minutes() {
        // interval_minutes 1 clamps to 5 minutes = 300 seconds.
        assert!(!due(&feed(true, 1), Some(1_000), 1_000 + 299));
        assert!(due(&feed(true, 1), Some(1_000), 1_000 + 300));
    }

    #[test]
    fn seen_path_shape() {
        let path = seen_path(7);
        assert_eq!(path.file_name().unwrap(), "seen-7.json");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "rss");
    }

    #[test]
    fn non_http_urls_are_refused() {
        assert!(check_feed_url("magnet:?xt=urn:btih:abc").is_err());
        assert!(check_feed_url("file:///C:/feed.xml").is_err());
        assert!(check_feed_url("ftp://example.com/feed.xml").is_err());
        assert!(check_feed_url("not a url").is_err());
        assert!(check_feed_url("http://example.com/feed.xml").is_ok());
        assert!(check_feed_url("  https://example.com/feed.xml ").is_ok());
    }
}
