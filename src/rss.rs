//! RSS 2.0 and Atom polling for HTTP(S) enclosure links. Magnet and torrent
//! links are ignored. The caller owns the [`Seen`] set and saves it with
//! [`Seen::save`] when it wants the state kept across runs.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use serde::{Deserialize, Serialize};
use tdm_store::{LoadOutcome, SyncMode};

const SEEN_MAX: usize = 5000;
const MAX_BODY: usize = 5 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
const SEEN_FILE: &str = "rss-seen.json";

#[derive(Debug, thiserror::Error)]
pub enum RssError {
    #[error("feed URL is not http or https: {0}")]
    Scheme(String),
    #[error("feed is larger than 5 MiB")]
    TooLarge,
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("XML error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("state error: {0}")]
    Store(#[from] tdm_store::StoreError),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// A feed item that matched a rule and is new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub title: String,
    pub url: String,
}

/// A download rule. `contains` words must all appear in the title
/// (case-insensitive substring). With `episode_min`, the title must also name
/// an episode number at or above it (`S01E05`, `E05`, or `Episode 5`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub contains: String,
    pub episode_min: Option<u32>,
}

/// URLs already returned by [`poll`], oldest first, bounded to 5000 entries.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(from = "Vec<String>", into = "Vec<String>")]
pub struct Seen {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl Seen {
    /// Adds `url`. Returns false when it was already present.
    pub fn insert(&mut self, url: &str) -> bool {
        if self.set.contains(url) {
            return false;
        }
        self.set.insert(url.to_owned());
        self.order.push_back(url.to_owned());
        while self.order.len() > SEEN_MAX {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }

    pub fn contains(&self, url: &str) -> bool {
        self.set.contains(url)
    }

    /// Loads the seen set from `path`. A missing file means an empty set.
    pub fn load(path: &Path) -> Result<Self, RssError> {
        match tdm_store::read_json_or_backup::<Seen>(path)? {
            LoadOutcome::Main(s) | LoadOutcome::Backup(s) => Ok(s),
            LoadOutcome::Missing => Ok(Seen::default()),
        }
    }

    /// Writes the seen set to `path` durably, creating the parent directory.
    pub fn save(&self, path: &Path) -> Result<(), RssError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        tdm_store::write_json_durable(path, self, SyncMode::Lifecycle)?;
        Ok(())
    }
}

impl From<Vec<String>> for Seen {
    fn from(urls: Vec<String>) -> Self {
        let mut seen = Seen::default();
        for url in urls {
            seen.insert(&url);
        }
        seen
    }
}

impl From<Seen> for Vec<String> {
    fn from(seen: Seen) -> Self {
        seen.order.into()
    }
}

/// Where the seen set lives: `rss-seen.json` in the app data directory.
pub fn default_seen_path() -> PathBuf {
    crate::store::app_dir().join(SEEN_FILE)
}

/// Fetches `feed_url`, returns the new items that match `rules`, and adds
/// their URLs to `seen`. The caller saves `seen` if it wants to keep it.
pub async fn poll(feed_url: &str, rules: &[Rule], seen: &mut Seen) -> Result<Vec<Found>, RssError> {
    let url = reqwest::Url::parse(feed_url.trim())
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or_else(|| RssError::Scheme(feed_url.to_owned()))?;
    if rules.is_empty() {
        return Ok(Vec::new());
    }
    let client = reqwest::Client::builder().timeout(TIMEOUT).build()?;
    let mut resp = client.get(url).send().await?.error_for_status()?;
    if resp.content_length().is_some_and(|n| n > MAX_BODY as u64) {
        return Err(RssError::TooLarge);
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(RssError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    let xml = String::from_utf8_lossy(&body);
    let items = parse_feed(&xml)?;
    Ok(select(items, rules, seen))
}

/// Keeps the items that match a rule and are not yet seen, marking them seen.
fn select(items: Vec<Found>, rules: &[Rule], seen: &mut Seen) -> Vec<Found> {
    items
        .into_iter()
        .filter(|f| rules.iter().any(|r| rule_matches(r, &f.title)))
        .filter(|f| seen.insert(&f.url))
        .collect()
}

fn rule_matches(rule: &Rule, title: &str) -> bool {
    let title = title.to_lowercase();
    let words: Vec<String> = rule.contains.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() || !words.iter().all(|w| title.contains(w.as_str())) {
        return false;
    }
    match rule.episode_min {
        None => true,
        Some(min) => episode_number(&title).is_some_and(|n| n >= min),
    }
}

/// Finds the episode number in a lowercased title: `episode 5`, `e05`, or the
/// `e05` part of `s01e05`.
fn episode_number(lower: &str) -> Option<u32> {
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    for (i, &tok) in tokens.iter().enumerate() {
        if tok == "episode"
            && let Some(n) = tokens.get(i + 1).and_then(|t| t.parse::<u32>().ok())
        {
            return Some(n);
        }
        if let Some(pos) = tok.rfind('e') {
            let head = &tok[..pos];
            let digits = &tok[pos + 1..];
            let head_ok = head.is_empty()
                || (head.len() > 1
                    && head.starts_with('s')
                    && head[1..].bytes().all(|b| b.is_ascii_digit()));
            if head_ok
                && !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && let Ok(n) = digits.parse::<u32>()
            {
                return Some(n);
            }
        }
    }
    None
}

/// Returns the URL when it is http or https. Anything else is dropped.
fn http_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let url = reqwest::Url::parse(trimmed).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| trimmed.to_owned())
}

#[derive(Default)]
struct Entry {
    title: String,
    enclosure: Option<String>,
    link: Option<String>,
}

fn attr(e: &BytesStart<'_>, name: &str) -> Result<Option<String>, RssError> {
    match e.try_get_attribute(name).map_err(quick_xml::Error::from)? {
        Some(a) => Ok(Some(a.normalized_value(XmlVersion::Implicit1_0)?.into_owned())),
        None => Ok(None),
    }
}

/// RSS `<enclosure url>` and Atom `<link rel="enclosure" href>` are preferred.
/// Any other Atom `<link href>` is the fallback.
fn take_link(e: &BytesStart<'_>, entry: &mut Entry) -> Result<(), RssError> {
    let name = e.name();
    match name.as_ref() {
        "enclosure" => {
            if entry.enclosure.is_none() {
                entry.enclosure = attr(e, "url")?.as_deref().and_then(http_url);
            }
        }
        "link" => {
            let href = attr(e, "href")?.as_deref().and_then(http_url);
            if attr(e, "rel")?.as_deref() == Some("enclosure") {
                if entry.enclosure.is_none() {
                    entry.enclosure = href;
                }
            } else if entry.link.is_none() {
                entry.link = href;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Parses an RSS 2.0 or Atom document into its items. Items without an http or
/// https enclosure or link are dropped. Rules and the seen set are not applied.
fn parse_feed(xml: &str) -> Result<Vec<Found>, RssError> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut cur: Option<Entry> = None;
    let mut in_title = false;
    loop {
        match reader.read_event()? {
            Event::Eof => break,
            Event::Start(e) => {
                match e.name().as_ref() {
                    "item" | "entry" => cur = Some(Entry::default()),
                    "title" => in_title = true,
                    _ => {}
                }
                if let Some(entry) = cur.as_mut() {
                    take_link(&e, entry)?;
                }
            }
            Event::Empty(e) => {
                if let Some(entry) = cur.as_mut() {
                    take_link(&e, entry)?;
                }
            }
            Event::End(e) => match e.name().as_ref() {
                "item" | "entry" => {
                    if let Some(entry) = cur.take()
                        && let Some(url) = entry.enclosure.or(entry.link)
                    {
                        let title = if entry.title.trim().is_empty() {
                            url.clone()
                        } else {
                            entry.title.trim().to_owned()
                        };
                        out.push(Found { title, url });
                    }
                }
                "title" => in_title = false,
                _ => {}
            },
            Event::Text(t) => {
                if in_title
                    && let Some(entry) = cur.as_mut()
                {
                    let raw = t.html_content();
                    let text = quick_xml::escape::unescape(&raw).map_err(quick_xml::Error::from)?;
                    entry.title.push_str(&text);
                }
            }
            // quick-xml reports `&amp;`, `&#38;` and friends as separate entity events.
            Event::GeneralRef(r) => {
                if in_title
                    && let Some(entry) = cur.as_mut()
                {
                    let text = match r.resolve_char_ref() {
                        Ok(Some(c)) => c.to_string(),
                        _ => match r.html_content().as_ref() {
                            "amp" => "&".to_owned(),
                            "lt" => "<".to_owned(),
                            "gt" => ">".to_owned(),
                            "quot" => "\"".to_owned(),
                            "apos" => "'".to_owned(),
                            _ => String::new(),
                        },
                    };
                    entry.title.push_str(&text);
                }
            }
            Event::CData(c) => {
                if in_title
                    && let Some(entry) = cur.as_mut()
                {
                    entry.title.push_str(&c.html_content());
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Feed</title>
<item><title>Big Show S01E05 1080p</title><enclosure url="https://example.com/a.mkv" type="video/x-matroska"/></item>
<item><title><![CDATA[Other Show E02]]></title><enclosure url="magnet:?xt=urn:btih:abc"/></item>
<item><title>Big Show S01E02</title><enclosure url="https://example.com/b.mkv" type="video/mp4"/></item>
</channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>Feed</title>
<entry><title>Tom &amp; Jerry E07</title><link rel="alternate" href="https://example.com/page"/><link rel="enclosure" type="video/mp4" href="https://example.com/c.mp4"/></entry>
<entry><title>Other E01</title><link href="https://example.com/d.mp4"/></entry>
</feed>"#;

    fn big_show_rules() -> Vec<Rule> {
        vec![Rule { contains: "big show".into(), episode_min: None }]
    }

    #[test]
    fn parses_rss_and_ignores_magnet() {
        let items = parse_feed(RSS).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Big Show S01E05 1080p");
        assert_eq!(items[0].url, "https://example.com/a.mkv");
        assert_eq!(items[1].url, "https://example.com/b.mkv");
    }

    #[test]
    fn parses_atom_enclosure_and_link_fallback() {
        let items = parse_feed(ATOM).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Tom & Jerry E07");
        assert_eq!(items[0].url, "https://example.com/c.mp4");
        assert_eq!(items[1].url, "https://example.com/d.mp4");
    }

    #[test]
    fn rules_match_case_insensitively_with_episode_floor() {
        let rule = Rule { contains: "big SHOW".into(), episode_min: Some(3) };
        assert!(rule_matches(&rule, "Big Show S01E05 1080p"));
        assert!(!rule_matches(&rule, "Big Show S01E02"));
        assert!(!rule_matches(&rule, "Other Show S01E05"));
        let any = Rule { contains: "show".into(), episode_min: None };
        assert!(rule_matches(&any, "BIG SHOW"));
        let empty = Rule { contains: String::new(), episode_min: None };
        assert!(!rule_matches(&empty, "anything"));
    }

    #[test]
    fn empty_rules_match_nothing() {
        let mut seen = Seen::default();
        assert!(select(parse_feed(RSS).unwrap(), &[], &mut seen).is_empty());
        assert!(seen.order.is_empty());
    }

    #[test]
    fn magnet_links_are_not_http() {
        assert!(http_url("magnet:?xt=urn:btih:abc").is_none());
        assert!(http_url("https://example.com/x.mkv").is_some());
    }

    #[test]
    fn seen_entries_are_skipped_on_second_parse() {
        let rules = big_show_rules();
        let mut seen = Seen::default();
        let first = select(parse_feed(RSS).unwrap(), &rules, &mut seen);
        assert_eq!(first.len(), 2);
        let second = select(parse_feed(RSS).unwrap(), &rules, &mut seen);
        assert!(second.is_empty());
    }

    #[test]
    fn seen_drops_oldest_beyond_bound() {
        let mut seen = Seen::default();
        for i in 0..=SEEN_MAX {
            assert!(seen.insert(&format!("https://x/{i}")));
        }
        assert_eq!(seen.order.len(), SEEN_MAX);
        assert!(!seen.contains("https://x/0"));
        assert!(seen.contains("https://x/1"));
        assert!(seen.contains(&format!("https://x/{SEEN_MAX}")));
    }

    #[test]
    fn seen_round_trips_through_json() {
        let mut seen = Seen::default();
        seen.insert("https://x/a");
        seen.insert("https://x/b");
        let json = serde_json::to_string(&seen).unwrap();
        let back: Seen = serde_json::from_str(&json).unwrap();
        assert!(back.contains("https://x/a") && back.contains("https://x/b"));
        assert_eq!(back.order.len(), 2);
    }
}
