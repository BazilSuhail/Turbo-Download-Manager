//! "Grab site": find the files behind a web page so they can be downloaded.
//!
//! The crawl stays on the start page's host, stops after `MAX_PAGES` files and
//! `MAX_DEPTH` levels of pages, obeys `robots.txt`, and pauses between requests.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use reqwest::Url;

/// Most files one grab adds to the list.
pub const MAX_PAGES: usize = 200;
/// How many levels of linked pages the crawl follows from the start page.
pub const MAX_DEPTH: u32 = 2;

/// Every `href` and `src` in an HTML document, resolved against `base`.
///
/// shortcut: a plain attribute scan, so it misses unquoted attributes and
/// `srcset`; upgrade to an HTML parser crate if sites need those.
pub fn links(html: &str, base: &Url) -> Vec<Url> {
    let lower = html.to_ascii_lowercase();
    let mut found: Vec<Url> = Vec::new();
    for attr in ["href=", "src="] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(attr).map(|i| from + i + attr.len()) {
            from = at;
            let Some(quote) = html[at..].chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
            let start = at + 1;
            let Some(len) = html[start..].find(quote) else { break };
            let value = html[start..start + len].trim();
            let skip = value.is_empty() || value.starts_with('#') || value.starts_with("data:") || value.starts_with("javascript:") || value.starts_with("mailto:");
            if !skip
                && let Ok(url) = base.join(value)
                && matches!(url.scheme(), "http" | "https")
                && !found.contains(&url)
            {
                found.push(url);
            }
        }
    }
    found
}

/// Whether `robots.txt` forbids `path` for every robot (the `*` group).
pub fn disallowed(robots: &str, path: &str) -> bool {
    let mut applies = false;
    for line in robots.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "user-agent" => applies = value == "*",
            "disallow" if applies && !value.is_empty() && path.starts_with(value) => return true,
            _ => {}
        }
    }
    false
}

/// Crawl from `start` and return every file URL to download, the start page included.
pub async fn crawl(start: Url) -> Result<Vec<Url>, String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("TDM/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let host = start.host_str().ok_or("the address has no host name")?.to_owned();
    let robots = match start.join("/robots.txt") {
        Ok(url) => match client.get(url).send().await {
            Ok(r) if r.status().is_success() => r.text().await.unwrap_or_default(),
            _ => String::new(),
        },
        Err(_) => String::new(),
    };

    let mut seen = HashSet::from([start.to_string()]);
    let mut queue = VecDeque::from([(start, 0u32)]);
    let mut found = Vec::new();
    while let Some((page, depth)) = queue.pop_front() {
        if found.len() >= MAX_PAGES {
            break;
        }
        if disallowed(&robots, page.path()) {
            continue;
        }
        found.push(page.clone());
        let Ok(response) = client.get(page.clone()).send().await else { continue };
        let is_html = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.contains("text/html"));
        if !is_html || depth >= MAX_DEPTH {
            continue;
        }
        let Ok(body) = response.text().await else { continue };
        for link in links(&body, &page) {
            if link.host_str() == Some(host.as_str()) && seen.insert(link.to_string()) {
                queue.push_back((link, depth + 1));
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://example.com/docs/index.html").unwrap()
    }

    #[test]
    fn finds_hrefs_and_srcs_resolved_against_the_page() {
        let html = r##"<a href="next.html">n</a><img src='/img/a.png'><a href="#top">t</a>
                      <a href="mailto:x@y.z">m</a><script src="https://cdn.example.org/x.js"></script>"##;
        let got: Vec<String> = links(html, &base()).iter().map(Url::to_string).collect();
        assert_eq!(
            got,
            vec![
                "https://example.com/docs/next.html",
                "https://example.com/img/a.png",
                "https://cdn.example.org/x.js",
            ]
        );
    }

    #[test]
    fn robots_disallow_applies_to_the_star_group_only() {
        let robots = "User-agent: Googlebot\nDisallow: /\n\nUser-agent: *\nDisallow: /private\nDisallow:\n";
        assert!(disallowed(robots, "/private/file.pdf"));
        assert!(!disallowed(robots, "/public/file.pdf"));
        assert!(!disallowed("", "/anything"));
    }
}
