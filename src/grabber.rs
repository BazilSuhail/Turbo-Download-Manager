//! "Grab site": find the files behind a web page so they can be downloaded.
//!
//! The crawl stays on the start page's host, stops after `MAX_PAGES` files and
//! `MAX_DEPTH` levels of pages, obeys `robots.txt`, and pauses between requests.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use reqwest::Url;

use crate::filetype::Category;

/// Most files one grab adds to the list.
pub const MAX_PAGES: usize = 200;
/// How many levels of linked pages the crawl follows from the start page.
pub const MAX_DEPTH: u32 = 2;

/// A file found by a grab, with its extension (lowercase, no dot) and its category.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub url: Url,
    pub ext: String,
    pub category: Category,
}

/// Describe a grabbed URL by the extension of its last path segment.
pub fn candidate(url: Url) -> Candidate {
    let name = url.path().rsplit('/').next().unwrap_or("");
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let category = category_of(&ext);
    Candidate { url, ext, category }
}

fn category_of(ext: &str) -> Category {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" | "svg" | "avif" | "heic" => Category::Image,
        "exe" | "msi" | "dmg" | "deb" | "rpm" | "apk" | "appimage" | "jar" | "wasm" => Category::Executable,
        "zip" | "7z" | "rar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "tar" => Category::Archive,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" | "epub" => Category::Document,
        "mp3" | "aac" | "ogg" | "wav" | "flac" | "m4a" => Category::Audio,
        "mp4" | "mov" | "mkv" | "webm" | "avi" => Category::Video,
        "ttf" | "otf" | "woff" | "woff2" => Category::Font,
        "html" | "htm" | "xhtml" | "php" | "asp" | "aspx" => Category::WebPage,
        "txt" | "md" | "csv" | "json" | "xml" | "log" => Category::Text,
        _ => Category::Other,
    }
}

/// Keep the candidates whose extension is in `exts` (case-insensitive, a leading dot is allowed).
/// An empty `exts` keeps every candidate.
pub fn filter_by_extensions(candidates: &[Candidate], exts: &[&str]) -> Vec<Candidate> {
    candidates
        .iter()
        .filter(|c| exts.is_empty() || exts.iter().any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(&c.ext)))
        .cloned()
        .collect()
}

/// Every link in an HTML document, resolved against `base` or the first `<base href>`.
///
/// Reads `href`, `src`, `data-src`, `data-href` (quoted, single-quoted or unquoted) and every
/// candidate of `srcset`. Comments and the contents of `<script>` and `<style>` are ignored.
pub fn links(html: &str, base: &Url) -> Vec<Url> {
    let lower = html.to_ascii_lowercase();
    let mut raw: Vec<String> = Vec::new();
    let mut base_href: Option<String> = None;
    let mut at = 0;
    while let Some(i) = lower[at..].find('<').map(|k| at + k) {
        if lower[i..].starts_with("<!--") {
            at = lower[i..].find("-->").map_or(lower.len(), |k| i + k + 3);
            continue;
        }
        let name_end = lower[i + 1..].find(|c: char| !c.is_ascii_alphanumeric()).map_or(lower.len(), |k| i + 1 + k);
        let name = &lower[i + 1..name_end];
        if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
            at = i + 1;
            continue;
        }
        let (attrs, end) = attributes(html, &lower, name_end);
        at = end;
        if name == "script" || name == "style" {
            // The tag's own src is a file to fetch; only the text inside the tag is skipped.
            raw.extend(attrs.iter().filter(|(a, _)| *a == "src").map(|(_, v)| decode_entities(v)));
            at = lower[end..].find(&format!("</{name}")).map_or(lower.len(), |k| end + k);
            continue;
        }
        for (attr, value) in attrs {
            let value = decode_entities(value);
            match attr {
                "href" if name == "base" => {
                    if base_href.is_none() {
                        base_href = Some(value);
                    }
                }
                "srcset" => raw.extend(value.split(',').filter_map(|c| c.split_whitespace().next()).map(str::to_owned)),
                "href" | "src" | "data-src" | "data-href" => raw.push(value),
                _ => {}
            }
        }
    }

    let root = base_href.and_then(|h| base.join(h.trim()).ok()).unwrap_or_else(|| base.clone());
    let mut found: Vec<Url> = Vec::new();
    for value in &raw {
        let value = value.trim();
        let lowered = value.to_ascii_lowercase();
        let skip = value.is_empty()
            || value.starts_with('#')
            || lowered.starts_with("data:")
            || lowered.starts_with("javascript:")
            || lowered.starts_with("mailto:");
        if !skip
            && let Ok(url) = root.join(value)
            && matches!(url.scheme(), "http" | "https")
            && !found.contains(&url)
        {
            found.push(url);
        }
    }
    found
}

/// Attributes of the tag whose name ends at `start`, as (lowercase name, raw value), and the
/// index just past the closing `>`. Quoted values end at their matching quote.
fn attributes<'a>(html: &'a str, lower: &'a str, start: usize) -> (Vec<(&'a str, &'a str)>, usize) {
    let b = html.as_bytes();
    let mut out = Vec::new();
    let mut i = start;
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        if i >= b.len() {
            return (out, b.len());
        }
        if b[i] == b'>' {
            return (out, i + 1);
        }
        let name_start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        if i == name_start {
            i += 1;
            continue;
        }
        let name = &lower[name_start..i];
        let mut j = i;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= b.len() || b[j] != b'=' {
            out.push((name, ""));
            i = j;
            continue;
        }
        j += 1;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        let (value, next) = match b.get(j) {
            Some(&q) if q == b'"' || q == b'\'' => {
                let end = html[j + 1..].find(q as char).map_or(b.len(), |k| j + 1 + k);
                (&html[j + 1..end], (end + 1).min(b.len()))
            }
            _ => {
                let end = html[j..].find(|c: char| c.is_ascii_whitespace() || c == '>').map_or(b.len(), |k| j + k);
                (&html[j..end], end)
            }
        };
        out.push((name, value));
        i = next;
    }
}

/// Replace the named entities `&amp; &lt; &gt; &quot; &apos;` and numeric `&#NN;` / `&#xNN;`.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        match after.find(';').filter(|&e| e <= 10).and_then(|e| entity(&after[..e]).map(|c| (c, e))) {
            Some((c, e)) => {
                out.push(c);
                rest = &after[e + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let code = match name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => name.strip_prefix('#')?.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
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

    fn found(html: &str) -> Vec<String> {
        links(html, &base()).iter().map(Url::to_string).collect()
    }

    #[test]
    fn finds_unquoted_attribute_values() {
        let got = found("<a href=next.html>n</a><img src=/img/a.png><img data-src=lazy.jpg alt=x>");
        assert_eq!(
            got,
            vec!["https://example.com/docs/next.html", "https://example.com/img/a.png", "https://example.com/docs/lazy.jpg"]
        );
    }

    #[test]
    fn srcset_yields_every_candidate_without_descriptors() {
        let got = found(r#"<img srcset="small.jpg 1x, large.jpg 2x, huge.jpg 800w"><source srcset='a.webp 320w,b.webp 640w'>"#);
        assert_eq!(
            got,
            vec![
                "https://example.com/docs/small.jpg",
                "https://example.com/docs/large.jpg",
                "https://example.com/docs/huge.jpg",
                "https://example.com/docs/a.webp",
                "https://example.com/docs/b.webp",
            ]
        );
    }

    #[test]
    fn decodes_entities_in_attribute_values() {
        let got = found(r#"<a href="page.php?a=1&amp;b=2">x</a><a href="x&#47;y.pdf">y</a><a href='z&#x2F;w.zip'>z</a>"#);
        assert_eq!(
            got,
            vec![
                "https://example.com/docs/page.php?a=1&b=2",
                "https://example.com/docs/x/y.pdf",
                "https://example.com/docs/z/w.zip",
            ]
        );
    }

    #[test]
    fn base_href_is_used_for_relative_links_and_is_not_a_link() {
        let got = found(r#"<base href="/files/"><a href="doc.pdf">d</a>"#);
        assert_eq!(got, vec!["https://example.com/files/doc.pdf"]);
        let got = found(r#"<link rel="x" href="https://cdn.example.org/a.css"><base href="https://example.com/media/"><img src="p.png">"#);
        assert_eq!(got, vec!["https://cdn.example.org/a.css", "https://example.com/media/p.png"]);
    }

    #[test]
    fn comments_are_ignored() {
        let got = found(r#"<!-- <a href="fake.html"> --><a href="real.html">r</a>"#);
        assert_eq!(got, vec!["https://example.com/docs/real.html"]);
    }

    #[test]
    fn script_and_style_contents_are_ignored() {
        let got = found(r#"<script>document.write('<img src="fake.png">')</script><style>a{background:url("<a href=fake.css>")}</style><a href="real.html">r</a>"#);
        assert_eq!(got, vec!["https://example.com/docs/real.html"]);
    }

    #[test]
    fn candidates_carry_extension_and_filter_by_it() {
        let cands: Vec<Candidate> = ["https://example.com/a/Report.PDF?x=1", "https://example.com/a/photo.png", "https://example.com/a/"]
            .iter()
            .map(|u| candidate(Url::parse(u).unwrap()))
            .collect();
        assert_eq!(cands[0].ext, "pdf");
        assert_eq!(cands[0].category, Category::Document);
        assert_eq!(cands[1].category, Category::Image);
        assert_eq!(cands[2].ext, "");
        let pdfs = filter_by_extensions(&cands, &["pdf"]);
        assert_eq!(pdfs.len(), 1);
        assert_eq!(pdfs[0].url.path(), "/a/Report.PDF");
        assert_eq!(filter_by_extensions(&cands, &[".PNG", "pdf"]).len(), 2);
        assert_eq!(filter_by_extensions(&cands, &[]).len(), 3);
    }

    #[test]
    fn robots_disallow_applies_to_the_star_group_only() {
        let robots = "User-agent: Googlebot\nDisallow: /\n\nUser-agent: *\nDisallow: /private\nDisallow:\n";
        assert!(disallowed(robots, "/private/file.pdf"));
        assert!(!disallowed(robots, "/public/file.pdf"));
        assert!(!disallowed("", "/anything"));
    }
}
