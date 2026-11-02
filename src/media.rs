//! Streaming-manifest handling: HLS (`.m3u8`) and DASH (`.mpd`).
//!
//! Detect by URL pattern, MIME type or body magic, then expand the manifest into a **flat**
//! segment list *before* any transfer, so the existing multi-connection engine can
//! take the list as-is. Muxing is out of scope — a job that needs two tracks joined
//! only sets [`MediaJob::needs_mux`].

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use reqwest::Url;
use std::path::{Path, PathBuf};

use crate::engine::{is_cross_site_sensitive, same_site};

/// Safety valve: a manifest claiming more segments than this is treated as hostile.
const MAX_SEGMENTS: usize = 100_000;

/// A manifest is never worth megabytes; a server streaming one forever would
/// otherwise fill memory.
const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("the manifest server answered HTTP {0}")]
    Status(u16),
    #[error("{0} cannot be used as a base for segment URLs")]
    BadUrl(String),
    #[error("{0} does not look like an HLS or DASH manifest")]
    NotAManifest(String),
    #[error("this stream is encrypted; decrypting it is out of scope")]
    Encrypted,
    #[error("the manifest listed no segments we can fetch")]
    Empty,
}

/// Fetch a manifest and flatten it into one job per track.
///
/// Two round trips at most: an HLS master playlist costs one more to read the
/// media playlist it points at. Everything after that is plain segment URLs.
pub async fn resolve(
    client: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
    output: &Path,
) -> Result<Vec<MediaJob>, Error> {
    let origin = Url::parse(url).map_err(|_| Error::BadUrl(url.to_owned()))?;
    let (text, content_type, base) = fetch_text(client, url, headers).await?;
    let kind = detect(url, content_type.as_deref(), text.as_bytes())
        .ok_or_else(|| Error::NotAManifest(url.to_owned()))?;

    let jobs = match kind {
        MediaKind::Hls => {
            // A master playlist lists variants; a media playlist lists
            // segments. Only the master needs the second fetch.
            let playlist = match select_best(&parse_hls_master(&text)) {
                Some(best) => {
                    let media_url = base
                        .join(&best.uri)
                        .map_err(|_| Error::BadUrl(best.uri.clone()))?;
                    // The master may name another host. Cookies, auth and
                    // referer are for the manifest's own site only.
                    let media_headers: Vec<(String, String)> =
                        if same_site(origin.host_str(), media_url.host_str()) {
                            headers.to_vec()
                        } else {
                            headers
                                .iter()
                                .filter(|(name, _)| !is_cross_site_sensitive(name))
                                .cloned()
                                .collect()
                        };
                    let (media_text, _, media_base) =
                        fetch_text(client, media_url.as_str(), &media_headers).await?;
                    parse_hls_media(&media_text, &media_base)
                }
                None => parse_hls_media(&text, &base),
            };
            if playlist.encrypted {
                return Err(Error::Encrypted);
            }
            vec![MediaJob::from_playlist(&playlist, output.to_path_buf())]
        }
        MediaKind::Dash => {
            let reps = expand_dash(&text, &base);
            let (video, audio) = select_best_dash(&reps);
            MediaJob::from_dash(video, audio, output)
        }
    };
    if jobs.iter().all(|j| j.segments.is_empty()) {
        return Err(Error::Empty);
    }
    Ok(jobs)
}

/// Returns the body, its Content-Type, and the URL it was finally served from.
/// After redirects that URL may be on another host, and relative URIs inside
/// the playlist resolve against it, not against the URL we asked for.
async fn fetch_text(
    client: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
) -> Result<(String, Option<String>, Url), Error> {
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let mut response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Status(status.as_u16()));
    }
    let final_url = response.url().clone();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    // Read chunk by chunk and stop at the cap, so a server that streams a
    // manifest forever cannot make us buffer all of it first.
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        buf.extend_from_slice(&chunk);
        if buf.len() >= MAX_MANIFEST_BYTES {
            break;
        }
    }
    buf.truncate(MAX_MANIFEST_BYTES);
    let text = String::from_utf8_lossy(&buf).into_owned();
    Ok((text, content_type, final_url))
}

/// A manifest URL names a playlist, not a file, so saving it as `.m3u8` leaves
/// the user with a text file where they expected a video. HLS segments are
/// MPEG-TS or fragmented MP4 appended end to end; `.ts` plays either in any
/// player that handles HLS at all, and DASH is fragmented MP4.
pub fn container_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let stem = |ext: &str| name[..name.len() - ext.len()].trim_end_matches('.').to_owned();
    if lower.ends_with(".m3u8") || lower.ends_with(".m3u") {
        let base = stem(if lower.ends_with(".m3u8") { ".m3u8" } else { ".m3u" });
        return format!("{}.ts", if base.is_empty() { "stream" } else { &base });
    }
    if lower.ends_with(".mpd") {
        let base = stem(".mpd");
        return format!("{}.mp4", if base.is_empty() { "stream" } else { &base });
    }
    name.to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Hls,
    Dash,
}

/// MIME types servers use for HLS playlists, registered and de-facto.
const HLS_MIMES: &[&str] = &[
    "application/x-mpegurl",
    "application/vnd.apple.mpegurl",
    "application/mpegurl",
    "audio/mpegurl",
    "audio/x-mpegurl",
    "vnd.apple.mpegurl",
];
const DASH_MIMES: &[&str] = &["application/dash+xml", "video/vnd.mpeg.dash.mpd"];

/// Three detection signals: URL pattern `\.(m3u8|mpd)(\?|$)`, the MIME
/// whitelist, and `#EXTM3U` body magic. Any one is enough.
pub fn detect(url: &str, content_type: Option<&str>, head: &[u8]) -> Option<MediaKind> {
    let lower = url.split('#').next().unwrap_or(url).to_ascii_lowercase();
    if ends_with_ext(&lower, ".m3u8") {
        return Some(MediaKind::Hls);
    }
    if ends_with_ext(&lower, ".mpd") {
        return Some(MediaKind::Dash);
    }

    if let Some(ct) = content_type {
        let ct = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        if HLS_MIMES.contains(&ct.as_str()) {
            return Some(MediaKind::Hls);
        }
        if DASH_MIMES.contains(&ct.as_str()) {
            return Some(MediaKind::Dash);
        }
    }

    let probe = &head[..head.len().min(1024)];
    let text = String::from_utf8_lossy(probe);
    let text = text.trim_start().trim_start_matches('\u{feff}').trim_start();
    if text.starts_with("#EXTM3U") {
        return Some(MediaKind::Hls);
    }
    None
}

/// `ext` appears in `url` followed by end-of-string or `?`.
fn ends_with_ext(url: &str, ext: &str) -> bool {
    let mut from = 0;
    while let Some(i) = url[from..].find(ext) {
        let end = from + i + ext.len();
        match url.as_bytes().get(end) {
            None | Some(b'?') => return true,
            _ => from = end,
        }
    }
    false
}

// ---------------------------------------------------------------- segments / jobs

#[derive(Debug, Clone, PartialEq)]
pub struct MediaSegment {
    pub url: Url,
    /// Seconds, when the manifest states it.
    pub duration: Option<f64>,
}

/// One track's full segment list, in playback order.
#[derive(Debug, Clone, Default)]
pub struct Playlist {
    /// `#EXT-X-MAP` / DASH `Initialization`: must be written first.
    pub init: Option<MediaSegment>,
    pub segments: Vec<MediaSegment>,
    pub total_duration: f64,
    /// `#EXT-X-KEY` with `METHOD != NONE`. We do not decrypt; the caller should refuse.
    pub encrypted: bool,
}

/// What the download engine consumes: fetch `segments` in order, append to `output`.
#[derive(Debug, Clone)]
pub struct MediaJob {
    pub segments: Vec<MediaSegment>,
    pub output: PathBuf,
    /// Audio and video arrived as separate tracks; joining them is the caller's call.
    pub needs_mux: bool,
}

impl MediaJob {
    pub fn from_playlist(playlist: &Playlist, output: PathBuf) -> Self {
        Self {
            segments: with_init(playlist.init.as_ref(), &playlist.segments),
            output,
            needs_mux: false,
        }
    }

    /// One job per present track. With both, each output gets a `.video`/`.audio` tag
    /// and `needs_mux` is set on both.
    pub fn from_dash(
        video: Option<&Representation>,
        audio: Option<&Representation>,
        output: &Path,
    ) -> Vec<MediaJob> {
        let needs_mux = video.is_some() && audio.is_some();
        [(video, "video"), (audio, "audio")]
            .into_iter()
            .filter_map(|(rep, tag)| rep.map(|r| (r, tag)))
            .map(|(rep, tag)| MediaJob {
                segments: with_init(rep.init.as_ref(), &rep.segments),
                output: if needs_mux {
                    tagged(output, tag)
                } else {
                    output.to_path_buf()
                },
                needs_mux,
            })
            .collect()
    }
}

fn with_init(init: Option<&MediaSegment>, segments: &[MediaSegment]) -> Vec<MediaSegment> {
    let mut out = Vec::with_capacity(segments.len() + 1);
    out.extend(init.cloned());
    out.extend_from_slice(segments);
    out
}

fn tagged(output: &Path, tag: &str) -> PathBuf {
    let stem = output
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("media");
    let name = match output.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{stem}.{tag}.{ext}"),
        None => format!("{stem}.{tag}"),
    };
    output.with_file_name(name)
}

// ------------------------------------------------------------------------- HLS

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Variant {
    pub uri: String,
    pub bandwidth: Option<u64>,
    pub resolution: Option<(u32, u32)>,
    pub codecs: Option<String>,
    pub audio_group: Option<String>,
}

/// `#EXT-X-STREAM-INF:` plus the following non-comment line, per RFC 8216.
pub fn parse_hls_master(text: &str) -> Vec<Variant> {
    let mut out = Vec::new();
    let mut pending: Option<Variant> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let mut v = Variant::default();
            for (key, value) in split_attributes(rest) {
                match key.as_str() {
                    "BANDWIDTH" => v.bandwidth = value.parse().ok(),
                    "RESOLUTION" => v.resolution = parse_resolution(&value),
                    "CODECS" => v.codecs = Some(value),
                    "AUDIO" => v.audio_group = Some(value),
                    // VIDEO / SUBTITLES / PROGRAM-ID: nothing downstream reads
                    // them yet.
                    _ => {}
                }
            }
            pending = Some(v);
        } else if line.starts_with('#') {
            continue;
        } else if let Some(mut v) = pending.take() {
            v.uri = line.to_owned();
            out.push(v);
        }
    }
    out
}

/// Quote-aware attribute-list split: a comma inside `"…"` is part of the value
/// (`CODECS="avc1.4d401f,mp4a.40.2"`). Keys are upper-cased, values de-quoted.
fn split_attributes(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in split_unquoted(list, b',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        out.push((
            key.trim().to_ascii_uppercase(),
            value.trim().trim_matches('"').to_owned(),
        ));
    }
    out
}

fn split_unquoted(s: &str, sep: u8) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    // Byte scanning is safe: no UTF-8 continuation byte equals an ASCII delimiter.
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'"' => quoted = !quoted,
            _ if b == sep && !quoted => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn attribute(list: &str, key: &str) -> Option<String> {
    split_attributes(list)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

fn parse_resolution(v: &str) -> Option<(u32, u32)> {
    let (w, h) = v.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Media playlist to a flat list of absolute segment URLs. Relative URIs resolve
/// against `base` — the playlist's own URL, not the page's.
pub fn parse_hls_media(text: &str, base: &Url) -> Playlist {
    let mut playlist = Playlist::default();
    let mut duration = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            duration = rest
                .split(',')
                .next()
                .and_then(|d| d.trim().parse::<f64>().ok())
                .filter(|d| d.is_finite() && *d >= 0.0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            playlist.init = attribute(rest, "URI")
                .and_then(|uri| base.join(&uri).ok())
                .map(|url| MediaSegment {
                    url,
                    duration: None,
                });
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let method = attribute(rest, "METHOD").unwrap_or_default();
            if !method.is_empty() && !method.eq_ignore_ascii_case("NONE") {
                playlist.encrypted = true;
            }
        } else if line.starts_with('#') {
            continue;
        } else if let Ok(url) = base.join(line) {
            if playlist.segments.len() >= MAX_SEGMENTS {
                break;
            }
            let duration = duration.take();
            playlist.total_duration += duration.unwrap_or(0.0);
            playlist.segments.push(MediaSegment { url, duration });
        }
    }
    playlist
}

/// Highest `BANDWIDTH`.
pub fn select_best(variants: &[Variant]) -> Option<&Variant> {
    variants.iter().max_by_key(|v| v.bandwidth.unwrap_or(0))
}

/// Highest-bandwidth video and audio representation, as `(video, audio)`:
/// DASH serves them as separate tracks that are fetched separately.
pub fn select_best_dash(reps: &[Representation]) -> (Option<&Representation>, Option<&Representation>) {
    let best = |want: &str| {
        reps.iter()
            .filter(|r| r.kind() == want && !r.segments.is_empty())
            .max_by_key(|r| r.bandwidth.unwrap_or(0))
    };
    (best("video"), best("audio"))
}

// ------------------------------------------------------------------------ DASH

#[derive(Debug, Clone, Default)]
pub struct Representation {
    pub id: String,
    pub mime_type: Option<String>,
    pub content_type: Option<String>,
    pub codecs: Option<String>,
    pub bandwidth: Option<u64>,
    pub resolution: Option<(u32, u32)>,
    pub init: Option<MediaSegment>,
    pub segments: Vec<MediaSegment>,
}

impl Representation {
    /// `"video"`, `"audio"` or `"unknown"`.
    pub fn kind(&self) -> &'static str {
        for hint in [self.content_type.as_deref(), self.mime_type.as_deref()] {
            let Some(hint) = hint else { continue };
            let hint = hint.to_ascii_lowercase();
            if hint.starts_with("video") {
                return "video";
            }
            if hint.starts_with("audio") {
                return "audio";
            }
        }
        if self.resolution.is_some() { "video" } else { "unknown" }
    }
}

/// Everything inherited down MPD → Period → AdaptationSet → Representation.
#[derive(Debug, Clone)]
struct Scope {
    base: Url,
    media: Option<String>,
    init_template: Option<String>,
    init_url: Option<String>,
    segment_urls: Vec<String>,
    /// `(S@t, S@d, S@r)` straight from the SegmentTimeline.
    timeline: Vec<(Option<u64>, u64, i64)>,
    duration: Option<u64>,
    timescale: Option<u64>,
    start_number: Option<u64>,
    /// Seconds: Period@duration, else MPD@mediaPresentationDuration.
    span: Option<f64>,
    id: Option<String>,
    mime_type: Option<String>,
    content_type: Option<String>,
    codecs: Option<String>,
    bandwidth: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
}

impl Scope {
    fn new(base: Url) -> Self {
        Self {
            base,
            media: None,
            init_template: None,
            init_url: None,
            segment_urls: Vec::new(),
            timeline: Vec::new(),
            duration: None,
            timescale: None,
            start_number: None,
            span: None,
            id: None,
            mime_type: None,
            content_type: None,
            codecs: None,
            bandwidth: None,
            width: None,
            height: None,
        }
    }

    fn take_common(&mut self, attrs: &[(String, String)]) {
        for (k, v) in attrs {
            match k.as_str() {
                "id" => self.id = Some(v.clone()),
                "mimeType" => self.mime_type = Some(v.clone()),
                "contentType" => self.content_type = Some(v.clone()),
                "codecs" => self.codecs = Some(v.clone()),
                "bandwidth" => self.bandwidth = v.parse().ok(),
                "width" => self.width = v.parse().ok(),
                "height" => self.height = v.parse().ok(),
                "duration" => self.span = parse_xs_duration(v).or(self.span),
                _ => {}
            }
        }
    }

    fn take_segment_attrs(&mut self, attrs: &[(String, String)]) {
        for (k, v) in attrs {
            match k.as_str() {
                "media" => self.media = Some(v.clone()),
                "initialization" => self.init_template = Some(v.clone()),
                "duration" => self.duration = v.parse().ok(),
                "timescale" => self.timescale = v.parse().ok(),
                "startNumber" => self.start_number = v.parse().ok(),
                _ => {}
            }
        }
    }
}

/// Expand an MPD into one [`Representation`] per track, each with its segment list
/// already materialised. A malformed manifest yields
/// whatever was parsed before the break — possibly nothing. Never panics.
pub fn expand_dash(mpd_xml: &str, base: &Url) -> Vec<Representation> {
    let mut reader = Reader::from_str(mpd_xml);
    let mut out = Vec::new();

    let mut mpd = Scope::new(base.clone());
    let mut period: Option<Scope> = None;
    let mut aset: Option<Scope> = None;
    let mut rep: Option<Scope> = None;
    let mut in_base_url = false;

    // A read error ends the document: a half-parsed manifest is not usable.
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Eof => break,
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                let name = e.local_name();
                let attrs = attributes(e);
                match name.as_ref() {
                    "MPD" => mpd.take_common(&mpd_attrs(&attrs)),
                    "Period" => {
                        let mut s = mpd.clone();
                        s.take_common(&attrs);
                        period = Some(s);
                    }
                    "AdaptationSet" => {
                        let mut s = period.clone().unwrap_or_else(|| mpd.clone());
                        s.take_common(&attrs);
                        aset = Some(s);
                    }
                    "Representation" => {
                        let mut s = aset
                            .clone()
                            .or_else(|| period.clone())
                            .unwrap_or_else(|| mpd.clone());
                        s.take_common(&attrs);
                        if empty {
                            out.push(finish(&s));
                        } else {
                            rep = Some(s);
                        }
                    }
                    "SegmentTemplate" | "SegmentList" => {
                        current(&mut mpd, &mut period, &mut aset, &mut rep)
                            .take_segment_attrs(&attrs)
                    }
                    "SegmentURL" => {
                        if let Some(media) = pick(&attrs, "media") {
                            let scope = current(&mut mpd, &mut period, &mut aset, &mut rep);
                            if scope.segment_urls.len() < MAX_SEGMENTS {
                                scope.segment_urls.push(media);
                            }
                        }
                    }
                    "Initialization" | "Initialisation" => {
                        if let Some(src) = pick(&attrs, "sourceURL") {
                            current(&mut mpd, &mut period, &mut aset, &mut rep).init_url = Some(src);
                        }
                    }
                    "S" => {
                        let scope = current(&mut mpd, &mut period, &mut aset, &mut rep);
                        if scope.timeline.len() < MAX_SEGMENTS {
                            scope.timeline.push((
                                pick(&attrs, "t").and_then(|v| v.parse().ok()),
                                pick(&attrs, "d").and_then(|v| v.parse().ok()).unwrap_or(0),
                                pick(&attrs, "r").and_then(|v| v.parse().ok()).unwrap_or(0),
                            ));
                        }
                    }
                    "BaseURL" => in_base_url = !empty,
                    _ => {}
                }
            }
            Event::Text(ref t) if in_base_url => {
                let raw = quick_xml::escape::unescape(t)
                    .unwrap_or(std::borrow::Cow::Borrowed(t.as_ref()));
                let raw = raw.trim();
                if !raw.is_empty() {
                    let scope = current(&mut mpd, &mut period, &mut aset, &mut rep);
                    if let Ok(joined) = scope.base.join(raw) {
                        scope.base = joined;
                    }
                }
            }
            Event::End(ref e) => match e.local_name().as_ref() {
                "BaseURL" => in_base_url = false,
                "Representation" => {
                    if let Some(s) = rep.take() {
                        out.push(finish(&s));
                    }
                }
                "AdaptationSet" => aset = None,
                "Period" => period = None,
                _ => {}
            },
            _ => {}
        }
    }
    out
}

/// The innermost open scope — where a `SegmentTemplate`/`BaseURL` child belongs.
fn current<'a>(
    mpd: &'a mut Scope,
    period: &'a mut Option<Scope>,
    aset: &'a mut Option<Scope>,
    rep: &'a mut Option<Scope>,
) -> &'a mut Scope {
    match (rep, aset, period) {
        (Some(s), _, _) => s,
        (_, Some(s), _) => s,
        (_, _, Some(s)) => s,
        _ => mpd,
    }
}

fn attributes(e: &BytesStart) -> Vec<(String, String)> {
    e.attributes()
        .filter_map(|a| a.ok())
        .filter_map(|a| {
            let key = a.key.local_name().as_ref().to_owned();
            let value = a
                .normalized_value(XmlVersion::Implicit1_0)
                .ok()?
                .into_owned();
            Some((key, value))
        })
        .collect()
}

fn pick(attrs: &[(String, String)], key: &str) -> Option<String> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

/// MPD@mediaPresentationDuration stands in for Period@duration on single-period MPDs.
fn mpd_attrs(attrs: &[(String, String)]) -> Vec<(String, String)> {
    attrs
        .iter()
        .map(|(k, v)| {
            let k = if k == "mediaPresentationDuration" {
                "duration"
            } else {
                k.as_str()
            };
            (k.to_owned(), v.clone())
        })
        .collect()
}

/// A representation with an unresolvable segment count still comes back, with an
/// empty list — the caller decides what to do about it.
fn finish(scope: &Scope) -> Representation {
    let id = scope.id.clone().unwrap_or_default();
    let bandwidth = scope.bandwidth;
    let timescale = scope.timescale.filter(|t| *t > 0).unwrap_or(1);
    let bw = bandwidth.unwrap_or(0);
    let resolve = |uri: &str| scope.base.join(uri).ok();

    let init = scope
        .init_url
        .clone()
        .or_else(|| {
            scope
                .init_template
                .as_deref()
                .map(|t| expand_template(t, &id, bw, 0, 0))
        })
        .and_then(|uri| resolve(&uri))
        .map(|url| MediaSegment {
            url,
            duration: None,
        });

    let segments = dash_segments(scope, timescale, &id, bw).unwrap_or_default();

    Representation {
        id,
        mime_type: scope.mime_type.clone(),
        content_type: scope.content_type.clone(),
        codecs: scope.codecs.clone(),
        bandwidth,
        resolution: match (scope.width, scope.height) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        },
        init,
        segments,
    }
}

fn dash_segments(
    scope: &Scope,
    timescale: u64,
    id: &str,
    bw: u64,
) -> Option<Vec<MediaSegment>> {
    let resolve = |uri: &str| scope.base.join(uri).ok();
    let mut segments = Vec::new();
    if !scope.segment_urls.is_empty() {
        // SegmentList: URLs are literal.
        let each = scope
            .duration
            .map(|d| d as f64 / timescale as f64)
            .filter(|d| *d > 0.0);
        for uri in &scope.segment_urls {
            if let Some(url) = resolve(uri) {
                segments.push(MediaSegment {
                    url,
                    duration: each,
                });
            }
        }
    } else if let Some(media) = scope.media.as_deref() {
        let mut number = scope.start_number.unwrap_or(1);
        if !scope.timeline.is_empty() {
            let end = scope
                .span
                .filter(|s| s.is_finite() && *s > 0.0)
                .map(|s| (s * timescale as f64) as u64);
            let mut time: u64 = 0;
            for (t, d, r) in &scope.timeline {
                if let Some(t) = t {
                    time = *t;
                }
                if *d == 0 {
                    continue;
                }
                let repeats = if *r < 0 {
                    // r="-1": repeat to the end of the period.
                    match end {
                        Some(end) if end > time => (end - time).div_ceil(*d),
                        _ => 1,
                    }
                } else {
                    (*r as u64).saturating_add(1)
                };
                for _ in 0..repeats.min(MAX_SEGMENTS as u64) {
                    if segments.len() >= MAX_SEGMENTS {
                        break;
                    }
                    if let Some(url) = resolve(&expand_template(media, id, bw, number, time)) {
                        segments.push(MediaSegment {
                            url,
                            duration: Some(*d as f64 / timescale as f64),
                        });
                    }
                    number = number.saturating_add(1);
                    time = time.saturating_add(*d);
                }
            }
        } else if media.contains("$Number") || media.contains("$Time") {
            // SegmentTemplate@duration: the count comes from the period length.
            let seg = scope.duration.filter(|d| *d > 0)?;
            let span = scope.span.filter(|s| s.is_finite() && *s > 0.0)?;
            let total = (span * timescale as f64) as u64;
            let count = total.div_ceil(seg).min(MAX_SEGMENTS as u64);
            let mut time: u64 = 0;
            for _ in 0..count {
                if let Some(url) = resolve(&expand_template(media, id, bw, number, time)) {
                    segments.push(MediaSegment {
                        url,
                        duration: Some(seg as f64 / timescale as f64),
                    });
                }
                number = number.saturating_add(1);
                time = time.saturating_add(seg);
            }
        } else if let Some(url) = resolve(media) {
            segments.push(MediaSegment {
                url,
                duration: scope.span,
            });
        }
    }
    Some(segments)
}

/// `$Var$` substitution, including the `%0Nd` width form (`$Number%05d$`).
/// `$$` is a literal `$`; an unknown identifier is left untouched.
fn expand_template(template: &str, id: &str, bandwidth: u64, number: u64, time: u64) -> String {
    let mut out = String::with_capacity(template.len() + 8);
    let mut rest = template;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        let Some(j) = rest.find('$') else {
            out.push('$');
            out.push_str(rest);
            return out;
        };
        let token = &rest[..j];
        rest = &rest[j + 1..];
        if token.is_empty() {
            out.push('$');
            continue;
        }
        let (name, format) = match token.find('%') {
            Some(p) => (&token[..p], Some(&token[p..])),
            None => (token, None),
        };
        match name {
            "RepresentationID" => out.push_str(id),
            "Number" => out.push_str(&pad(number, format)),
            "Bandwidth" => out.push_str(&pad(bandwidth, format)),
            "Time" => out.push_str(&pad(time, format)),
            _ => {
                out.push('$');
                out.push_str(token);
                out.push('$');
            }
        }
    }
    out.push_str(rest);
    out
}

fn pad(value: u64, format: Option<&str>) -> String {
    let width = format
        .and_then(|f| {
            f.trim_start_matches('%')
                .trim_end_matches(['d', 'D', 'u', 'i'])
                .parse::<usize>()
                .ok()
        })
        .unwrap_or(0)
        .min(20);
    format!("{value:0width$}")
}

/// `xs:duration`, the subset manifests use: `PT1H2M3.5S`, `P1DT4H`.
fn parse_xs_duration(text: &str) -> Option<f64> {
    let text = text.trim();
    let body = text.strip_prefix('P').or_else(|| text.strip_prefix("-P"))?;
    let (date, time) = match body.split_once('T') {
        Some((d, t)) => (d, t),
        None => (body, ""),
    };
    let mut seconds = 0.0f64;
    let mut number = String::new();
    for (part, units) in [(date, ['Y', 'M', 'D']), (time, ['H', 'M', 'S'])] {
        let is_time = units[0] == 'H';
        for c in part.chars() {
            if c.is_ascii_digit() || c == '.' {
                number.push(c);
                continue;
            }
            let value: f64 = number.parse().ok()?;
            number.clear();
            seconds += value
                * match (c, is_time) {
                    ('Y', _) => 31_536_000.0,
                    ('M', false) => 2_592_000.0,
                    ('D', _) | ('W', _) => 86_400.0,
                    ('H', _) => 3600.0,
                    ('M', true) => 60.0,
                    ('S', _) => 1.0,
                    _ => return None,
                };
        }
        if !number.is_empty() {
            return None; // trailing digits with no unit
        }
    }
    seconds.is_finite().then_some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("test URL is a literal")
    }

    /// A master that names a media playlist on another host string must not
    /// receive the Cookie header; the master's own fetch still gets it.
    #[tokio::test]
    async fn credentials_do_not_follow_the_media_playlist_to_another_host() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen: Arc<std::sync::Mutex<Vec<(String, bool)>>> = Arc::default();
        let master = format!(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nhttp://localhost:{port}/media.m3u8\n"
        );
        let media = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nseg0.ts\n#EXT-X-ENDLIST\n";

        let server_seen = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let seen = Arc::clone(&server_seen);
                let master = master.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        if sock.read_exact(&mut byte).await.is_err() {
                            return;
                        }
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let path = head
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_owned();
                    let has_cookie =
                        head.lines().any(|l| l.to_ascii_lowercase().starts_with("cookie:"));
                    seen.lock().expect("seen").push((path.clone(), has_cookie));
                    let body = if path == "/media.m3u8" { media.to_owned() } else { master };
                    let out = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(out.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });

        let headers = vec![("Cookie".to_owned(), "session=secret".to_owned())];
        let client = reqwest::Client::new();
        let dir = std::env::temp_dir().join(format!("tdm-media-unit-{}", std::process::id()));
        let jobs = resolve(
            &client,
            &format!("http://127.0.0.1:{port}/master.m3u8"),
            &headers,
            &dir.join("out.ts"),
        )
        .await
        .expect("resolves");
        assert_eq!(jobs.len(), 1);

        let seen = seen.lock().expect("seen").clone();
        assert!(
            seen.contains(&("/master.m3u8".to_owned(), true)),
            "master should carry the cookie: {seen:?}"
        );
        assert!(
            seen.contains(&("/media.m3u8".to_owned(), false)),
            "media playlist on another host must not carry the cookie: {seen:?}"
        );
    }

    const MASTER: &str = "\
#EXTM3U
#EXT-X-VERSION:4
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",URI=\"audio/en.m3u8\"
#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.42c01e,mp4a.40.2\",AUDIO=\"aud\"
low/index.m3u8
#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=2400000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"aud\",SUBTITLES=\"subs\"
high/index.m3u8
";

    #[test]
    fn master_keeps_commas_inside_quoted_codecs() {
        let variants = parse_hls_master(MASTER);
        assert_eq!(variants.len(), 2);
        let best = select_best(&variants).expect("two variants");
        assert_eq!(best.uri, "high/index.m3u8");
        assert_eq!(best.bandwidth, Some(2_400_000));
        assert_eq!(best.resolution, Some((1280, 720)));
        assert_eq!(best.codecs.as_deref(), Some("avc1.4d401f,mp4a.40.2"));
        assert_eq!(best.audio_group.as_deref(), Some("aud"));
        assert_eq!(variants[0].codecs.as_deref(), Some("avc1.42c01e,mp4a.40.2"));
    }

    #[test]
    fn media_playlist_resolves_relative_uris_against_the_playlist() {
        let text = "\
#EXTM3U
#EXT-X-TARGETDURATION:6
#EXT-X-MAP:URI=\"init.mp4\"
#EXTINF:6.0,
seg-1.m4s
#EXTINF:5.5,
../other/seg-2.m4s
#EXT-X-ENDLIST
";
        let base = url("https://cdn.example.com/v/720p/index.m3u8");
        let playlist = parse_hls_media(text, &base);
        assert_eq!(
            playlist.init.as_ref().map(|s| s.url.as_str()),
            Some("https://cdn.example.com/v/720p/init.mp4")
        );
        assert_eq!(playlist.segments.len(), 2);
        assert_eq!(
            playlist.segments[0].url.as_str(),
            "https://cdn.example.com/v/720p/seg-1.m4s"
        );
        assert_eq!(
            playlist.segments[1].url.as_str(),
            "https://cdn.example.com/v/other/seg-2.m4s"
        );
        assert!((playlist.total_duration - 11.5).abs() < 1e-9);
        assert!(!playlist.encrypted);

        let job = MediaJob::from_playlist(&playlist, PathBuf::from("out.mp4"));
        assert_eq!(job.segments.len(), 3); // init first
        assert!(!job.needs_mux);
    }

    #[test]
    fn encrypted_playlist_is_flagged() {
        let text = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:4,\na.ts\n";
        assert!(parse_hls_media(text, &url("https://e.test/p.m3u8")).encrypted);
    }

    #[test]
    fn detection_by_url_mime_and_body_magic_independently() {
        // URL only.
        assert_eq!(
            detect("https://e.test/a/master.m3u8?token=1", None, b""),
            Some(MediaKind::Hls)
        );
        assert_eq!(
            detect("https://e.test/a/manifest.mpd", None, b""),
            Some(MediaKind::Dash)
        );
        // MIME only.
        assert_eq!(
            detect(
                "https://e.test/stream?id=7",
                Some("application/vnd.apple.mpegurl; charset=utf-8"),
                b""
            ),
            Some(MediaKind::Hls)
        );
        assert_eq!(
            detect("https://e.test/stream?id=7", Some("application/dash+xml"), b""),
            Some(MediaKind::Dash)
        );
        // Body magic only.
        assert_eq!(
            detect("https://e.test/stream", None, b"\xef\xbb\xbf#EXTM3U\n#EXTINF:4,\n"),
            Some(MediaKind::Hls)
        );
        // Nothing.
        assert_eq!(detect("https://e.test/a.mp4", Some("video/mp4"), b"\0\0\0 ftyp"), None);
        // `.m3u8` must be followed by `?` or end.
        assert_eq!(detect("https://e.test/a.m3u8x/b", None, b""), None);
    }

    const MPD_TEMPLATE: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT0H0M30.0S">
  <BaseURL>https://cdn.example.com/dash/</BaseURL>
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate timescale="1000" duration="4000" startNumber="1"
        initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/seg-$Number%05d$.m4s"/>
      <Representation id="v720" bandwidth="2400000" width="1280" height="720" codecs="avc1.4d401f"/>
      <Representation id="v360" bandwidth="800000" width="640" height="360" codecs="avc1.42c01e"/>
    </AdaptationSet>
    <AdaptationSet contentType="audio" mimeType="audio/mp4">
      <SegmentList timescale="1000" duration="4000">
        <Initialization sourceURL="a/init.mp4"/>
        <SegmentURL media="a/seg-1.m4s"/>
        <SegmentURL media="a/seg-2.m4s"/>
      </SegmentList>
      <Representation id="a128" bandwidth="128000" codecs="mp4a.40.2"/>
    </AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn dash_segment_template_with_number_width() {
        let reps = expand_dash(MPD_TEMPLATE, &url("https://origin.test/x/manifest.mpd"));
        assert_eq!(reps.len(), 3);
        let (video, audio) = select_best_dash(&reps);
        let video = video.expect("a video representation");
        let audio = audio.expect("an audio representation");

        assert_eq!(video.id, "v720");
        assert_eq!(video.resolution, Some((1280, 720)));
        assert_eq!(video.segments.len(), 8); // 30s / 4s, rounded up
        assert_eq!(
            video.init.as_ref().map(|s| s.url.as_str()),
            Some("https://cdn.example.com/dash/v720/init.mp4")
        );
        assert_eq!(
            video.segments[0].url.as_str(),
            "https://cdn.example.com/dash/v720/seg-00001.m4s"
        );
        assert_eq!(
            video.segments[7].url.as_str(),
            "https://cdn.example.com/dash/v720/seg-00008.m4s"
        );

        assert_eq!(audio.segments.len(), 2);
        assert_eq!(
            audio.segments[1].url.as_str(),
            "https://cdn.example.com/dash/a/seg-2.m4s"
        );

        let jobs = MediaJob::from_dash(Some(video), Some(audio), Path::new("show.mp4"));
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().all(|j| j.needs_mux));
        assert_eq!(jobs[0].output, PathBuf::from("show.video.mp4"));
        assert_eq!(jobs[1].output, PathBuf::from("show.audio.mp4"));
        assert_eq!(jobs[0].segments.len(), 9); // init + 8

        let single = MediaJob::from_dash(Some(video), None, Path::new("show.mp4"));
        assert_eq!(single.len(), 1);
        assert!(!single[0].needs_mux);
        assert_eq!(single[0].output, PathBuf::from("show.mp4"));
    }

    #[test]
    fn dash_segment_timeline_with_repeat_minus_one() {
        let mpd = r#"<MPD mediaPresentationDuration="PT20S">
  <Period duration="PT20S">
    <AdaptationSet mimeType="video/mp4">
      <Representation id="r0" bandwidth="1000000" width="1920" height="1080">
        <SegmentTemplate timescale="90000" media="r0/$Time$.m4s" initialization="r0/i.mp4">
          <SegmentTimeline>
            <S t="0" d="450000" r="1"/>
            <S d="180000" r="-1"/>
          </SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;
        let reps = expand_dash(mpd, &url("https://cdn.test/live/manifest.mpd"));
        assert_eq!(reps.len(), 1);
        let rep = &reps[0];
        // 2 x 5s from the first S (10s), then 2s segments filling the rest of 20s = 5.
        assert_eq!(rep.segments.len(), 7);
        assert_eq!(
            rep.segments[0].url.as_str(),
            "https://cdn.test/live/r0/0.m4s"
        );
        assert_eq!(
            rep.segments[1].url.as_str(),
            "https://cdn.test/live/r0/450000.m4s"
        );
        assert_eq!(
            rep.segments[2].url.as_str(),
            "https://cdn.test/live/r0/900000.m4s"
        );
        assert_eq!(
            rep.segments[6].url.as_str(),
            "https://cdn.test/live/r0/1620000.m4s"
        );
        assert_eq!(rep.segments[3].duration, Some(2.0));
        assert_eq!(rep.kind(), "video");
        assert_eq!(
            rep.init.as_ref().map(|s| s.url.as_str()),
            Some("https://cdn.test/live/r0/i.mp4")
        );
    }

    #[test]
    fn template_variables_and_padding() {
        assert_eq!(
            expand_template("$RepresentationID$/$Bandwidth$/$Number%05d$-$Time$", "v1", 64000, 7, 90),
            "v1/64000/00007-90"
        );
        assert_eq!(expand_template("a$$b$Number$", "", 0, 3, 0), "a$b3");
        assert_eq!(expand_template("$Unknown$/x", "", 0, 0, 0), "$Unknown$/x");
        assert_eq!(expand_template("$Number", "", 0, 1, 0), "$Number");
    }

    #[test]
    fn malformed_manifests_do_not_panic() {
        let base = url("https://e.test/m.mpd");
        assert!(expand_dash("", &base).is_empty());
        assert!(expand_dash("not xml at all <<<>>", &base).is_empty());
        assert!(expand_dash("<MPD><Period><AdaptationSet", &base).is_empty());
        // Well-formed shape, nonsense values.
        let junk = r#"<MPD mediaPresentationDuration="banana"><Period><AdaptationSet>
            <SegmentTemplate timescale="0" duration="-1" media="$Number%zzd$.m4s"/>
            <Representation id="x" bandwidth="not-a-number"/>
            </AdaptationSet></Period></MPD>"#;
        let reps = expand_dash(junk, &base);
        assert_eq!(reps.len(), 1);
        assert!(reps[0].segments.is_empty());
        assert_eq!(reps[0].bandwidth, None);

        // HLS side.
        assert!(parse_hls_master("#EXT-X-STREAM-INF:BANDWIDTH=\"unclosed,CODECS=x").is_empty());
        assert!(parse_hls_master("").is_empty());
        // An unparsable EXTINF loses the duration but keeps the segment; a bare
        // EXT-X-MAP with no URI yields no init segment.
        let pl = parse_hls_media("#EXTM3U\n#EXTINF:abc,\nseg.ts\n#EXT-X-MAP:\n", &base);
        assert_eq!(pl.segments.len(), 1);
        assert_eq!(pl.segments[0].duration, None);
        assert_eq!(pl.total_duration, 0.0);
        assert!(pl.init.is_none());
        assert_eq!(select_best(&[]), None);
        assert_eq!(parse_resolution("1920"), None);
        assert_eq!(parse_xs_duration("PT"), Some(0.0));
        assert_eq!(parse_xs_duration("12S"), None);
    }
}
