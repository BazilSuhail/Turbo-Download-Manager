//! Proxy resolution: PAC scripts (evaluated in QuickJS) and proxy URL checks.
//!
//! The PAC evaluator gets no module loader, no file access and no OS helpers.
//! The script text is fetched by the caller; this module only evaluates it.

use std::time::{Duration, Instant};

use rquickjs::{Context, Function, Runtime};

use crate::engine::ProxyConfig;

/// Wall-clock budget for one `FindProxyForURL` call. A script that runs past
/// it is interrupted and reported as `PacError::Timeout`.
pub const PAC_TIME_BUDGET: Duration = Duration::from_millis(250);

/// Heap cap for the PAC runtime.
const PAC_MEMORY_LIMIT: usize = 16 * 1024 * 1024;

/// PAC helpers that need no OS access. Anything else (`dnsResolve`,
/// `myIpAddress`, `isInNet`, ...) is undefined and throws, which surfaces as
/// `PacError::Script`.
const PAC_PRELUDE: &str = r#"
function isPlainHostName(host) { return host.indexOf('.') === -1; }
function dnsDomainIs(host, domain) {
  return host.length >= domain.length &&
    host.substring(host.length - domain.length) === domain;
}
function shExpMatch(text, pattern) {
  var re = '^' + pattern.replace(/[.+^${}()|\[\]\\]/g, '\\$&')
                        .replace(/\*/g, '.*').replace(/\?/g, '.') + '$';
  return new RegExp(re).test(text);
}
"#;

/// What a PAC script decided for one URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PacDecision {
    Direct,
    /// A proxy URL: `http://host:port` or `socks5://host:port`.
    Proxy(String),
}

/// Why a PAC decision could not be made.
#[derive(Debug, thiserror::Error)]
pub enum PacError {
    #[error("download URL is not valid: {0}")]
    BadUrl(String),
    #[error("PAC engine could not start: {0}")]
    Engine(String),
    #[error("PAC script failed: {0}")]
    Script(String),
    #[error("PAC script did not return within {0:?}")]
    Timeout(Duration),
    #[error(transparent)]
    Proxy(#[from] ProxyError),
}

/// Why a proxy URL was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProxyError {
    #[error("proxy URL is not valid: {0}")]
    Malformed(String),
    #[error("unsupported proxy scheme `{0}` (use http, https, socks5 or socks5h)")]
    Scheme(String),
    #[error("proxy URL needs a host and a port")]
    MissingHostOrPort,
    #[error("proxy URL must not carry credentials; set them in the proxy settings")]
    Credentials,
}

/// Plain result for the engine: no proxy, or one proxy URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedProxy {
    None,
    Url(String),
}

impl From<PacDecision> for ResolvedProxy {
    fn from(d: PacDecision) -> Self {
        match d {
            PacDecision::Direct => ResolvedProxy::None,
            PacDecision::Proxy(url) => ResolvedProxy::Url(url),
        }
    }
}

/// Build the engine's proxy record for a PAC-chosen URL. PAC proxies carry no
/// credentials, and both protocols use the proxy.
pub fn pac_proxy_config(url: String) -> ProxyConfig {
    ProxyConfig {
        url,
        http: true,
        https: true,
        user: String::new(),
        password: String::new(),
    }
}

/// Evaluate `FindProxyForURL(url, host)` from `script` and return the first
/// usable directive. Runs with a time budget of [`PAC_TIME_BUDGET`].
pub fn pac_decide(script: &str, url: &str) -> Result<PacDecision, PacError> {
    let parsed = reqwest::Url::parse(url).map_err(|e| PacError::BadUrl(e.to_string()))?;
    let host = parsed
        .host_str()
        .unwrap_or("")
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();

    let deadline = Instant::now() + PAC_TIME_BUDGET;
    let rt = Runtime::new().map_err(|e| PacError::Engine(e.to_string()))?;
    rt.set_memory_limit(PAC_MEMORY_LIMIT);
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let ctx = Context::full(&rt).map_err(|e| PacError::Engine(e.to_string()))?;

    let answer: Result<String, rquickjs::Error> = ctx.with(|ctx| {
        ctx.eval::<(), _>(PAC_PRELUDE)?;
        ctx.eval::<(), _>(script)?;
        let find: Function = ctx.globals().get("FindProxyForURL")?;
        find.call::<_, String>((url.to_string(), host))
    });

    match answer {
        Ok(text) => Ok(parse_pac_result(&text)),
        Err(_) if Instant::now() >= deadline => Err(PacError::Timeout(PAC_TIME_BUDGET)),
        Err(e) => Err(PacError::Script(e.to_string())),
    }
}

/// Pick the first usable directive from a PAC answer such as
/// `"SOCKS5 s:1080; PROXY p:8080; DIRECT"`. Unknown or malformed directives are
/// skipped. When nothing usable remains, the answer is `Direct`.
fn parse_pac_result(answer: &str) -> PacDecision {
    for entry in answer.split(';') {
        let mut parts = entry.split_whitespace();
        let Some(keyword) = parts.next() else { continue };
        let target = parts.next();
        let scheme = match keyword.to_ascii_uppercase().as_str() {
            "DIRECT" => return PacDecision::Direct,
            "PROXY" => "http",
            "SOCKS5" | "SOCKS" => "socks5",
            _ => continue,
        };
        let Some(target) = target else { continue };
        let candidate = format!("{scheme}://{target}");
        if parse_proxy_url(&candidate).is_ok() {
            return PacDecision::Proxy(candidate);
        }
    }
    PacDecision::Direct
}

/// Check a proxy URL: scheme must be http, https, socks5 or socks5h, a host and
/// an explicit port must be present, and no credentials may be embedded.
pub fn parse_proxy_url(s: &str) -> Result<reqwest::Url, ProxyError> {
    let url = reqwest::Url::parse(s).map_err(|e| ProxyError::Malformed(e.to_string()))?;
    match url.scheme() {
        "http" | "https" | "socks5" | "socks5h" => {}
        other => return Err(ProxyError::Scheme(other.to_string())),
    }
    if url.host_str().is_none_or(str::is_empty) || !has_explicit_port(s) {
        return Err(ProxyError::MissingHostOrPort);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ProxyError::Credentials);
    }
    Ok(url)
}

/// Whether the authority of `s` names a port. `Url` drops a default port such
/// as `:80` from its own form, so the text itself is checked.
fn has_explicit_port(s: &str) -> bool {
    let rest = s.split_once("://").map_or(s, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    let after_host = host_port.rsplit_once(']').map_or(host_port, |(_, t)| t);
    after_host.rsplit_once(':').is_some_and(|(_, port)| !port.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL_DIRECT_SCRIPT: &str = r#"
        function FindProxyForURL(url, host) {
            if (host === "localhost" || isPlainHostName(host)) return "DIRECT";
            return "PROXY proxy.test:8080";
        }"#;

    #[test]
    fn pac_is_direct_for_localhost_and_proxied_elsewhere() {
        assert_eq!(
            pac_decide(LOCAL_DIRECT_SCRIPT, "http://localhost:8000/file.zip").unwrap(),
            PacDecision::Direct
        );
        assert_eq!(
            pac_decide(LOCAL_DIRECT_SCRIPT, "https://example.com/file.zip").unwrap(),
            PacDecision::Proxy("http://proxy.test:8080".into())
        );
        assert_eq!(
            ResolvedProxy::from(pac_decide(LOCAL_DIRECT_SCRIPT, "http://intranet/x").unwrap()),
            ResolvedProxy::None
        );
    }

    #[test]
    fn socks_directives_map_to_socks5_urls() {
        let socks = r#"function FindProxyForURL(u, h) { return "SOCKS5 s.test:1080; DIRECT"; }"#;
        assert_eq!(
            pac_decide(socks, "https://example.com/").unwrap(),
            PacDecision::Proxy("socks5://s.test:1080".into())
        );
        let legacy = r#"function FindProxyForURL(u, h) { return "SOCKS s.test:1081"; }"#;
        assert_eq!(
            pac_decide(legacy, "https://example.com/").unwrap(),
            PacDecision::Proxy("socks5://s.test:1081".into())
        );
    }

    #[test]
    fn unknown_and_malformed_directives_are_skipped() {
        let s = r#"function FindProxyForURL(u, h) { return "HTTPX a:1; PROXY ; PROXY p:3128"; }"#;
        assert_eq!(
            pac_decide(s, "https://example.com/").unwrap(),
            PacDecision::Proxy("http://p:3128".into())
        );
        let empty = r#"function FindProxyForURL(u, h) { return ""; }"#;
        assert_eq!(pac_decide(empty, "https://example.com/").unwrap(), PacDecision::Direct);
    }

    #[test]
    fn infinite_loop_is_stopped_within_the_budget() {
        let looping = "function FindProxyForURL(u, h) { while (true) {} }";
        let started = Instant::now();
        let err = pac_decide(looping, "https://example.com/").unwrap_err();
        assert!(matches!(err, PacError::Timeout(_)), "got {err:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn broken_or_missing_function_is_a_script_error() {
        let broken = "function FindProxyForURL(u, h) {";
        assert!(matches!(
            pac_decide(broken, "https://example.com/"),
            Err(PacError::Script(_))
        ));
        let missing = "var x = 1;";
        assert!(matches!(
            pac_decide(missing, "https://example.com/"),
            Err(PacError::Script(_))
        ));
    }

    #[test]
    fn malformed_proxy_urls_are_rejected() {
        assert!(matches!(parse_proxy_url("not a url"), Err(ProxyError::Malformed(_))));
        assert!(matches!(
            parse_proxy_url("ftp://h:21"),
            Err(ProxyError::Scheme(_))
        ));
        assert_eq!(
            parse_proxy_url("socks5://s.test").unwrap_err(),
            ProxyError::MissingHostOrPort
        );
        assert_eq!(
            parse_proxy_url("http://user:pw@h:3128").unwrap_err(),
            ProxyError::Credentials
        );
        assert!(parse_proxy_url("http://h:3128").is_ok());
        assert!(parse_proxy_url("socks5h://s.test:1080").is_ok());
    }

    #[test]
    fn a_proxy_without_an_explicit_port_is_refused_not_defaulted() {
        assert_eq!(parse_proxy_url("http://h").unwrap_err(), ProxyError::MissingHostOrPort);
        // Written out, the default port is still a port the user named.
        assert!(parse_proxy_url("http://h:80").is_ok());
        let no_port = r#"function FindProxyForURL(u, h) { return "PROXY h"; }"#;
        assert_eq!(pac_decide(no_port, "https://example.com/").unwrap(), PacDecision::Direct);
    }
}
