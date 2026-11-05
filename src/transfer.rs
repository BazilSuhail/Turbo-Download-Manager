//! Export and import of settings and the download list as one JSON file.
//!
//! Secrets stay out of the file. The proxy password is never in `Settings`
//! (it is `serde(skip)` and lives in the keychain; only `password_ref` is
//! written). Credential headers are removed from download records, and a
//! password inside a URL's userinfo is removed from its URL.
//!
//! Import is treated as untrusted input: the format and version are checked,
//! the record count is capped, URLs must use a known scheme, destinations must
//! be absolute, and runtime status is reset.

use serde_json::map::Entry;
use serde_json::{json, Value};

use crate::model::Settings;
use crate::store::DownloadRecord;

const FORMAT: &str = "tdm-export";
const VERSION: u64 = 1;
/// Most download records one import may carry.
const MAX_DOWNLOADS: usize = 100_000;
const SCHEMES: [&str; 4] = ["http", "https", "ftp", "ftps"];

/// The contents of a validated export file.
pub struct Import {
    /// `None` when the file has no settings (or they were null).
    pub settings: Option<Settings>,
    pub downloads: Vec<DownloadRecord>,
}

/// Serialise settings and downloads to the export JSON, without secrets.
pub fn export_json(settings: &Settings, downloads: &[DownloadRecord]) -> String {
    let downloads: Vec<Value> = downloads
        .iter()
        .filter_map(|r| serde_json::to_value(export_record(r)).ok())
        .collect();
    let doc = json!({
        "format": FORMAT,
        "version": VERSION,
        "settings": serde_json::to_value(settings).unwrap_or(Value::Null),
        "downloads": downloads,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_owned())
}

/// Parse and validate an export file. Invalid download records are dropped
/// one by one; a bad envelope, an unknown format, a newer version or too many
/// records fails the whole import.
pub fn import_json(text: &str) -> Result<Import, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let obj = root
        .as_object()
        .ok_or_else(|| "not a TDM export: the top level must be an object".to_owned())?;

    match obj.get("format").and_then(Value::as_str) {
        Some(FORMAT) => {}
        Some(other) => {
            return Err(format!("not a TDM export: format is \"{other}\", expected \"{FORMAT}\""));
        }
        None => return Err("not a TDM export: the format field is missing".to_owned()),
    }

    let version = obj
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "the export has no valid version number".to_owned())?;
    if version > VERSION {
        return Err(format!(
            "export version {version} is newer than this TDM reads (version {VERSION}); update TDM to import it"
        ));
    }
    if version != VERSION {
        return Err(format!("unsupported export version {version}"));
    }

    let settings = match obj.get("settings") {
        None | Some(Value::Null) => None,
        Some(value) => Some(load_settings(value)?),
    };

    let downloads = match obj.get("downloads") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            if items.len() > MAX_DOWNLOADS {
                return Err(format!(
                    "the export lists {} downloads; the limit is {MAX_DOWNLOADS}",
                    items.len()
                ));
            }
            items.iter().filter_map(import_record).collect()
        }
        Some(_) => return Err("the downloads entry must be a list".to_owned()),
    };

    Ok(Import { settings, downloads })
}

/// A copy of `record` that is safe to write out.
fn export_record(record: &DownloadRecord) -> DownloadRecord {
    let mut out = record.clone();
    out.headers.retain(|(name, _)| !is_secret_name(name));
    out.url = scrub_url(&out.url);
    out.mirrors = out.mirrors.iter().map(|m| scrub_url(m)).collect();
    out
}

/// The URL with its password and any secret-looking query parameters removed.
fn scrub_url(url: &str) -> String {
    strip_secret_query(&strip_password(url))
}

/// One imported record, or `None` when it must be dropped.
fn import_record(value: &Value) -> Option<DownloadRecord> {
    let mut record: DownloadRecord = serde_json::from_value(value.clone()).ok()?;
    if !record.dest.is_absolute() {
        return None;
    }
    record.url = clean_url(&record.url)?;
    record.mirrors = record.mirrors.iter().filter_map(|m| clean_url(m)).collect();
    record.headers.retain(|(name, _)| !is_secret_name(name));
    // Runtime status is never trusted from a file: the download starts as new.
    record.complete = false;
    record.kind = None;
    record.content_type = None;
    record.total = None;
    // Queue membership is restored by file path, and the imported queue may
    // not exist on this machine, so the id from the file is never trusted.
    record.queue_id = None;
    Some(record)
}

/// Settings from the file, laid over the defaults so a partial object loads.
fn load_settings(value: &Value) -> Result<Settings, String> {
    if !value.is_object() {
        return Err("settings must be an object".to_owned());
    }
    let mut merged = serde_json::to_value(Settings::default()).map_err(|e| e.to_string())?;
    merge(&mut merged, value);
    let mut settings: Settings =
        serde_json::from_value(merged).map_err(|e| format!("settings are invalid: {e}"))?;
    if !settings.download_dir.is_absolute() {
        settings.download_dir = Settings::default().download_dir;
    }
    Ok(settings)
}

/// Recursively overlays `overlay` onto `base`. Non-object values replace.
fn merge(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(b), Value::Object(o)) => {
            for (key, value) in o {
                match b.entry(key.clone()) {
                    Entry::Occupied(mut slot) => merge(slot.get_mut(), value),
                    Entry::Vacant(slot) => {
                        slot.insert(value.clone());
                    }
                }
            }
        }
        (slot, value) => *slot = value.clone(),
    }
}

/// True for header names that carry a secret (cookies, authorization, tokens, passwords).
fn is_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "cookie",
        "authorization",
        "auth",
        "session",
        "x-auth",
        "x-session",
        "password",
        "passwd",
        "token",
        "secret",
        "api-key",
        "apikey",
    ]
    .iter()
    .any(|s| lower.contains(*s))
}

/// True for query parameter names that carry a credential. Matched exactly
/// (ignoring case), so a name such as `design` is not caught by `sig`.
fn is_secret_query(name: &str) -> bool {
    const NAMES: [&str; 10] = [
        "token",
        "auth",
        "key",
        "sig",
        "signature",
        "session",
        "password",
        "passwd",
        "secret",
        "access_token",
    ];
    let lower = name.to_ascii_lowercase();
    NAMES.contains(&lower.as_str())
}

/// The URL with its secret-looking query parameters removed. Other URLs are returned unchanged.
fn strip_secret_query(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut parsed) if parsed.query_pairs().any(|(name, _)| is_secret_query(&name)) => {
            let kept: Vec<(String, String)> = parsed
                .query_pairs()
                .filter(|(name, _)| !is_secret_query(name))
                .map(|(name, value)| (name.into_owned(), value.into_owned()))
                .collect();
            if kept.is_empty() {
                parsed.set_query(None);
            } else {
                parsed.query_pairs_mut().clear().extend_pairs(kept);
            }
            parsed.to_string()
        }
        _ => url.to_owned(),
    }
}

/// The URL with its password removed. Other URLs are returned unchanged.
fn strip_password(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut parsed) if parsed.password().is_some() => {
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        _ => url.to_owned(),
    }
}

/// The URL without a password, or `None` when it is not one of the accepted schemes.
fn clean_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    SCHEMES.contains(&parsed.scheme()).then(|| strip_password(url.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path that is valid on this platform.
    fn abs(name: &str) -> String {
        std::env::temp_dir().join(name).display().to_string()
    }

    fn record(url: &str, dest: &str, headers: Value) -> DownloadRecord {
        serde_json::from_value(json!({ "url": url, "dest": dest, "headers": headers })).unwrap()
    }

    fn import_downloads(list: Value) -> Vec<DownloadRecord> {
        let text = json!({ "format": FORMAT, "version": 1, "downloads": list }).to_string();
        import_json(&text).expect("import").downloads
    }

    #[test]
    fn round_trip_keeps_records() {
        let records = vec![
            record("https://example.test/a.zip", &abs("a.zip"), json!([["Referer", "https://example.test/"]])),
            record("ftp://files.test/b.iso", &abs("b.iso"), json!([])),
        ];
        let text = export_json(&Settings::default(), &records);
        assert!(text.contains("\"format\": \"tdm-export\""));
        assert!(text.contains("\"version\": 1"));

        let back = import_json(&text).expect("round trip");
        assert_eq!(back.downloads.len(), 2);
        assert_eq!(back.downloads[0].url, "https://example.test/a.zip");
        assert_eq!(back.downloads[0].dest, records[0].dest);
        assert_eq!(back.downloads[1].url, "ftp://files.test/b.iso");
        assert_eq!(back.settings, Some(Settings::default()));
    }

    #[test]
    fn cookies_and_authorization_are_stripped() {
        let rec = record(
            "https://example.test/private.zip",
            &abs("private.zip"),
            json!([
                ["Referer", "https://example.test/"],
                ["Cookie", "session=cookie-secret"],
                ["authorization", "Bearer token-secret"],
                ["Proxy-Authorization", "Basic cHJveHktc2VjcmV0"],
                ["X-Api-Token", "api-secret"]
            ]),
        );
        let text = export_json(&Settings::default(), &[rec]);
        assert!(!text.contains("cookie-secret"));
        assert!(!text.contains("token-secret"));
        assert!(!text.contains("cHJveHktc2VjcmV0"));
        assert!(!text.contains("api-secret"));

        let back = import_json(&text).unwrap();
        assert_eq!(
            back.downloads[0].headers,
            vec![("Referer".to_owned(), "https://example.test/".to_owned())]
        );
    }

    #[test]
    fn secret_query_parameters_and_session_headers_are_not_exported() {
        let rec = record(
            "https://example.test/a.zip?token=tok-secret&page=2&Sig=sig-secret&design=keep",
            &abs("a.zip"),
            json!([["X-Session", "sess-secret"], ["X-Auth", "auth-secret"], ["Accept", "*/*"]]),
        );
        let text = export_json(&Settings::default(), &[rec]);
        assert!(!text.contains("tok-secret"));
        assert!(!text.contains("sig-secret"));
        assert!(!text.contains("sess-secret"));
        assert!(!text.contains("auth-secret"));

        let back = import_json(&text).unwrap();
        assert_eq!(back.downloads[0].url, "https://example.test/a.zip?page=2&design=keep");
        assert_eq!(back.downloads[0].headers, vec![("Accept".to_owned(), "*/*".to_owned())]);
    }

    #[test]
    fn imported_queue_id_is_dropped() {
        let downloads = import_downloads(json!([
            { "url": "https://ok.test/a.zip", "dest": abs("a.zip"), "queue_id": 4 }
        ]));
        assert_eq!(downloads[0].queue_id, None);
    }

    #[test]
    fn url_passwords_and_proxy_password_are_not_exported() {
        let mut settings = Settings::default();
        settings.proxy.password = "hunter2".into();
        let rec = record("https://user:pw-secret@example.test/a.zip", &abs("a.zip"), json!([]));
        let text = export_json(&settings, &[rec]);
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("pw-secret"));
    }

    #[test]
    fn bad_scheme_is_dropped() {
        let downloads = import_downloads(json!([
            { "url": "file:///etc/passwd", "dest": abs("x1") },
            { "url": "javascript:alert(1)", "dest": abs("x2") },
            { "url": "https://ok.test/a.zip", "dest": abs("a.zip") },
        ]));
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].url, "https://ok.test/a.zip");
    }

    #[test]
    fn relative_path_is_dropped() {
        let downloads = import_downloads(json!([
            { "url": "https://ok.test/rel.zip", "dest": "relative/rel.zip" },
            { "url": "https://ok.test/abs.zip", "dest": abs("abs.zip") },
        ]));
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].url, "https://ok.test/abs.zip");
    }

    #[test]
    fn imported_records_start_with_empty_runtime_status() {
        let downloads = import_downloads(json!([
            { "url": "https://ok.test/a.zip", "dest": abs("a.zip"),
              "complete": true, "kind": "zip", "content_type": "application/zip", "total": 10 }
        ]));
        assert!(!downloads[0].complete);
        assert_eq!(downloads[0].kind, None);
        assert_eq!(downloads[0].content_type, None);
        assert_eq!(downloads[0].total, None);
    }

    #[test]
    fn wrong_format_is_refused() {
        let text = json!({ "format": "something-else", "version": 1, "downloads": [] }).to_string();
        let err = import_json(&text).err().expect("must refuse");
        assert!(err.contains("format"), "unclear error: {err}");
        assert!(import_json("[]").is_err());
        assert!(import_json("not json").is_err());
    }

    #[test]
    fn newer_version_is_refused() {
        let text = json!({ "format": FORMAT, "version": 2, "downloads": [] }).to_string();
        let err = import_json(&text).err().expect("must refuse");
        assert!(err.contains("newer"), "unclear error: {err}");
    }

    #[test]
    fn oversize_count_is_refused() {
        let text = json!({
            "format": FORMAT,
            "version": 1,
            "downloads": vec![Value::Null; MAX_DOWNLOADS + 1],
        })
        .to_string();
        let err = import_json(&text).err().expect("must refuse");
        assert!(err.contains("limit"), "unclear error: {err}");
    }

    #[test]
    fn partial_settings_load_over_defaults() {
        let text = json!({
            "format": FORMAT,
            "version": 1,
            "settings": { "max_active": 7, "connection": { "retries": 2 } },
        })
        .to_string();
        let settings = import_json(&text).unwrap().settings.expect("settings present");
        assert_eq!(settings.max_active, 7);
        assert_eq!(settings.connection.retries, 2);
        assert_eq!(settings.max_segments, Settings::default().max_segments);
        assert!(!settings.categories.is_empty());
    }
}
