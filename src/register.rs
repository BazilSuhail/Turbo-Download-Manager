//! Installing the native-messaging host registration.
//!
//! A browser finds a native host through a JSON manifest. On Windows the
//! manifest can live anywhere and a per-user registry key names it; on Linux
//! and macOS the manifest must sit in a fixed per-browser directory. Both are
//! per-user, so nothing here needs elevation or affects other accounts.
//!
//! The manifest lists the extensions allowed to connect, so an unrelated
//! extension cannot talk to the host even if it guesses the name. Chromium
//! browsers list origins (`allowed_origins`); Firefox lists add-on ids
//! (`allowed_extensions`), so it gets a manifest of its own.

use std::path::{Path, PathBuf};

pub const HOST_NAME: &str = "com.tdm.host";

/// Chrome/Edge id of the bundled extension, pinned by the `key` in
/// `extension/manifest.json`.
pub const CHROME_EXTENSION_ID: &str = "glkodjlchifmdbokmofigopbgdbabmge";
/// Firefox add-on id, from `browser_specific_settings.gecko.id`.
pub const FIREFOX_EXTENSION_ID: &str = "tdm@tdm.app";

/// Browsers the sidebar lists. Brave, Vivaldi and Opera share the Chromium
/// native-host keys and the same guided steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Chrome,
    Edge,
    Brave,
    Vivaldi,
    Opera,
    Firefox,
}

impl Browser {
    pub const ALL: [Browser; 6] = [
        Browser::Chrome,
        Browser::Edge,
        Browser::Brave,
        Browser::Vivaldi,
        Browser::Opera,
        Browser::Firefox,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Browser::Chrome => "Chrome",
            Browser::Edge => "Edge",
            Browser::Brave => "Brave",
            Browser::Vivaldi => "Vivaldi",
            Browser::Opera => "Opera",
            Browser::Firefox => "Firefox",
        }
    }

    /// The extensions page: where the user loads TDM.
    pub fn extensions_url(self) -> &'static str {
        match self {
            Browser::Chrome => "chrome://extensions",
            Browser::Edge => "edge://extensions",
            Browser::Brave => "brave://extensions",
            Browser::Vivaldi => "vivaldi://extensions",
            Browser::Opera => "opera://extensions",
            Browser::Firefox => "about:debugging#/runtime/this-firefox",
        }
    }

    /// Open the browser on its extensions page.
    pub fn open_extensions_page(self) -> Result<(), Error> {
        let url = self.extensions_url();
        if cfg!(windows) {
            // Per-user install first, then the all-users one.
            let exe = self
                .windows_exes()
                .into_iter()
                .find(|p| p.is_file())
                .ok_or(Error::NotInstalled(self.label()))?;
            std::process::Command::new(exe).arg(url).spawn()?;
        } else if cfg!(target_os = "macos") {
            let app = match self {
                Browser::Chrome => "Google Chrome",
                Browser::Edge => "Microsoft Edge",
                Browser::Brave => "Brave Browser",
                Browser::Vivaldi => "Vivaldi",
                Browser::Opera => "Opera",
                Browser::Firefox => "Firefox",
            };
            std::process::Command::new("open").args(["-a", app, url]).spawn()?;
        } else {
            let command = match self {
                Browser::Chrome => "google-chrome",
                Browser::Edge => "microsoft-edge",
                Browser::Brave => "brave-browser",
                Browser::Vivaldi => "vivaldi",
                Browser::Opera => "opera",
                Browser::Firefox => "firefox",
            };
            std::process::Command::new(command).arg(url).spawn()?;
        }
        Ok(())
    }

    /// Where the browser's executable lives on Windows, per-user install first.
    fn windows_exes(self) -> Vec<PathBuf> {
        let var = |name: &str, fallback: &str| std::env::var_os(name).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(fallback));
        let pf = var("ProgramFiles", r"C:\Program Files");
        let pf86 = var("ProgramFiles(x86)", r"C:\Program Files (x86)");
        let local = dirs::data_local_dir().unwrap_or_default();
        match self {
            Browser::Chrome => vec![
                local.join(r"Google\Chrome\Application\chrome.exe"),
                pf.join(r"Google\Chrome\Application\chrome.exe"),
            ],
            Browser::Edge => vec![pf86.join(r"Microsoft\Edge\Application\msedge.exe"), pf.join(r"Microsoft\Edge\Application\msedge.exe")],
            Browser::Brave => vec![
                local.join(r"BraveSoftware\Brave-Browser\Applicationrave.exe"),
                pf.join(r"BraveSoftware\Brave-Browser\Applicationrave.exe"),
            ],
            Browser::Vivaldi => vec![local.join(r"Vivaldi\Applicationivaldi.exe")],
            Browser::Opera => vec![local.join(r"Programs\Opera\opera.exe")],
            Browser::Firefox => vec![pf.join(r"Mozilla Firefoxirefox.exe"), pf86.join(r"Mozilla Firefoxirefox.exe")],
        }
    }

    /// The folder to load unpacked: the copy the installer put beside the app,
    /// or the build output when running from source.
    pub fn extension_dir(self) -> PathBuf {
        let sub = if self == Browser::Firefox { "firefox" } else { "chrome" };
        let beside_app = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("extension").join(sub)));
        match beside_app {
            Some(dir) if dir.join("manifest.json").is_file() => dir,
            _ => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extension").join("dist").join(sub),
        }
    }

    /// The per-user folder that proves the browser is installed.
    fn profile(self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        if cfg!(windows) {
            let local = dirs::data_local_dir()?;
            let roaming = dirs::data_dir()?;
            return Some(match self {
                Browser::Chrome => local.join("Google").join("Chrome"),
                Browser::Edge => local.join("Microsoft").join("Edge"),
                Browser::Brave => local.join("BraveSoftware").join("Brave-Browser"),
                Browser::Vivaldi => local.join("Vivaldi"),
                Browser::Opera => roaming.join("Opera Software"),
                Browser::Firefox => roaming.join("Mozilla").join("Firefox"),
            });
        }
        let mac = cfg!(target_os = "macos");
        let support = if mac { home.join("Library").join("Application Support") } else { home.join(".config") };
        Some(match self {
            Browser::Chrome => support.join(if mac { "Google/Chrome" } else { "google-chrome" }),
            Browser::Edge => support.join(if mac { "Microsoft Edge" } else { "microsoft-edge" }),
            Browser::Brave => support.join("BraveSoftware/Brave-Browser"),
            Browser::Vivaldi => support.join(if mac { "Vivaldi" } else { "vivaldi" }),
            Browser::Opera => support.join(if mac { "com.operasoftware.Opera" } else { "opera" }),
            Browser::Firefox => {
                if mac {
                    support.join("Firefox")
                } else {
                    home.join(".mozilla")
                }
            }
        })
    }

    pub fn installed(self) -> bool {
        self.profile().is_some_and(|p| p.is_dir())
    }

    /// Every listed browser installed for this user, in sidebar order.
    pub fn detected() -> Vec<Browser> {
        Browser::ALL.into_iter().filter(|b| b.installed()).collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encoding the manifest: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0} is not a plausible Chrome extension id (expect 32 lowercase letters a-p)")]
    BadExtensionId(String),
    #[error("registry: {0}")]
    Registry(String),
    #[error("no supported browser profile found under {0}")]
    NoBrowser(PathBuf),
    #[error("{0} is not installed")]
    NotInstalled(&'static str),
}

/// Chrome ids are 32 characters drawn from `a`–`p`. Rejecting anything else
/// early gives a clear error instead of a host that silently never connects.
fn valid_extension_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// The Chromium-format manifest's home on Windows, where the registry points.
pub fn manifest_path() -> PathBuf {
    crate::store::app_dir().join(format!("{HOST_NAME}.json"))
}

/// Build the manifest Chromium-based browsers expect.
pub fn manifest_json(exe: &Path, extension_ids: &[String]) -> Result<String, Error> {
    for id in extension_ids {
        if !valid_extension_id(id) {
            return Err(Error::BadExtensionId(id.clone()));
        }
    }
    let origins: Vec<String> =
        extension_ids.iter().map(|id| format!("chrome-extension://{id}/")).collect();
    let value = serde_json::json!({
        "name": HOST_NAME,
        "description": "Turbo Download Manager native messaging host",
        "path": exe.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": origins,
    });
    Ok(serde_json::to_string_pretty(&value)?)
}

/// Build the manifest Firefox expects: add-on ids instead of origins.
pub fn firefox_manifest_json(exe: &Path, addon_ids: &[&str]) -> Result<String, Error> {
    let value = serde_json::json!({
        "name": HOST_NAME,
        "description": "Turbo Download Manager native messaging host",
        "path": exe.to_string_lossy(),
        "type": "stdio",
        "allowed_extensions": addon_ids,
    });
    Ok(serde_json::to_string_pretty(&value)?)
}

/// Register for the bundled extension in every supported browser. Idempotent
/// and cheap when nothing changed; the application calls it at every startup
/// so no manual step is needed.
pub fn install_default() -> Result<PathBuf, Error> {
    install_with(&default_ids(), false)
}

/// The bundled extension plus any ids added with `--register-host <ids>`,
/// which every start must keep.
fn default_ids() -> Vec<String> {
    let mut ids = platform::registered_ids();
    if !ids.iter().any(|id| id == CHROME_EXTENSION_ID) {
        ids.push(CHROME_EXTENSION_ID.to_owned());
    }
    ids
}

/// Re-run the native-host registration: the sidebar's "Set up" action.
pub fn set_up() -> Result<(), Error> {
    write_manifests(&default_ids(), true).map(|_| ())
}

/// The extension ids an existing Chromium manifest allows, in its order.
fn ids_in_manifest(json: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    value["allowed_origins"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o.as_str()?.strip_prefix("chrome-extension://")?.strip_suffix('/'))
        .filter(|id| valid_extension_id(id))
        .map(str::to_owned)
        .collect()
}

/// Register for these Chromium extension ids (the bundled one when empty),
/// plus the bundled Firefox add-on. Always rewrites the registry keys, so it
/// repairs a registration that was removed by hand.
///
/// Returns the Chromium manifest path (on Linux/macOS, the first one written).
pub fn install(extension_ids: &[String]) -> Result<PathBuf, Error> {
    if extension_ids.is_empty() {
        return install_with(&[CHROME_EXTENSION_ID.to_owned()], true);
    }
    install_with(extension_ids, true)
}

fn install_with(extension_ids: &[String], force: bool) -> Result<PathBuf, Error> {
    write_manifests(extension_ids, force)
}

fn write_manifests(extension_ids: &[String], force: bool) -> Result<PathBuf, Error> {
    let exe = std::env::current_exe()?;
    let chromium = manifest_json(&exe, extension_ids)?;
    let firefox = firefox_manifest_json(&exe, &[FIREFOX_EXTENSION_ID])?;
    platform::install(&chromium, &firefox, force)
}

/// Start (or stop starting) the application when the user signs in.
pub fn set_launch_at_login(on: bool) -> Result<(), Error> {
    platform::set_autostart(&std::env::current_exe()?, on)
}

/// Remove every manifest, registry key and policy entry TDM added. Missing
/// ones are not an error: the uninstaller runs this whatever state it finds.
pub fn uninstall() -> Result<(), Error> {
    platform::uninstall()
}

/// Write `content` unless the file already holds exactly that. Returns
/// whether it wrote, so callers can skip follow-up work on the common path.
fn write_if_changed(path: &Path, content: &str) -> std::io::Result<bool> {
    if std::fs::read(path).is_ok_and(|old| old == content.as_bytes()) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)?;
    Ok(true)
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    /// Registry homes of `NativeMessagingHosts`, under `HKCU\Software`. Opera
    /// reads Chrome's key, so it is covered without one of its own.
    const CHROMIUM_VENDORS: [&str; 5] = [
        "Google\\Chrome",
        "Microsoft\\Edge",
        "Chromium",
        "BraveSoftware\\Brave-Browser",
        "Vivaldi",
    ];

    fn key(vendor: &str) -> String {
        format!("HKCU\\Software\\{vendor}\\NativeMessagingHosts\\{HOST_NAME}")
    }

    fn firefox_path() -> PathBuf {
        crate::store::app_dir().join(format!("{HOST_NAME}.firefox.json"))
    }

    pub fn install(chromium: &str, firefox: &str, force: bool) -> Result<PathBuf, Error> {
        let path = manifest_path();
        let ff_path = firefox_path();
        let changed = write_if_changed(&path, chromium)? | write_if_changed(&ff_path, firefox)?;
        // shortcut: at startup the keys are only rewritten when a manifest
        // changed (i.e. the exe moved), so a key deleted by hand stays gone
        // until `--register-host` runs; read the keys back via RegGetValueW if
        // that ever matters, without paying six reg.exe spawns per launch.
        if changed || force {
            for vendor in CHROMIUM_VENDORS {
                write_default_value(&key(vendor), &path.to_string_lossy())?;
            }
            write_default_value(&key("Mozilla"), &ff_path.to_string_lossy())?;
        }
        Ok(path)
    }

    pub fn registered_ids() -> Vec<String> {
        std::fs::read_to_string(manifest_path()).map(|j| ids_in_manifest(&j)).unwrap_or_default()
    }

    const RUN: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_NAME: &str = "Turbo Download Manager";

    pub fn uninstall() -> Result<(), Error> {
        for vendor in CHROMIUM_VENDORS.iter().copied().chain(["Mozilla"]) {
            // Fails when the key is already gone, which is the goal anyway.
            let _ = reg(&["delete", &key(vendor), "/f"]);
        }
        // Fails when the value is already gone, which is the goal anyway.
        let _ = reg(&["delete", RUN, "/v", RUN_NAME, "/f"]);
        remove_if_present(&manifest_path())?;
        remove_if_present(&firefox_path())?;
        Ok(())
    }

    /// The installer writes the same value, so either side can undo the other.
    pub fn set_autostart(exe: &Path, on: bool) -> Result<(), Error> {
        let quoted = format!("\"{}\"", exe.display());
        let args: Vec<&str> = if on {
            vec!["add", RUN, "/v", RUN_NAME, "/t", "REG_SZ", "/d", &quoted, "/f"]
        } else {
            vec!["delete", RUN, "/v", RUN_NAME, "/f"]
        };
        let output = reg(&args)?;
        // Deleting a value that is not there fails; that is already the goal.
        if on && !output.status.success() {
            return Err(Error::Registry(String::from_utf8_lossy(&output.stderr).trim().to_owned()));
        }
        Ok(())
    }

    /// Set a key's default value with `reg.exe`.
    ///
    /// Shelling out avoids pulling in a registry crate for a few lines of work,
    /// and `reg.exe` is present on every Windows install.
    fn write_default_value(key: &str, value: &str) -> Result<(), Error> {
        let output = reg(&["add", key, "/ve", "/t", "REG_SZ", "/d", value, "/f"])?;
        if !output.status.success() {
            return Err(Error::Registry(format!(
                "{key}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(())
    }

    fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
        use std::os::windows::process::CommandExt as _;
        // Absolute path, never a bare name: `Command` resolves a bare name
        // against the current directory on Windows, so a `reg.exe` dropped in
        // the working directory would run instead of the system one.
        let reg = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join("reg.exe");
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // Without this a console flashes up on every GUI start.
        std::process::Command::new(&reg).args(args).creation_flags(CREATE_NO_WINDOW).output()
    }
}

/// One browser's manifest directory, written only when `profile` exists so we
/// never create config trees for browsers the user does not have.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, PartialEq)]
struct Target {
    profile: PathBuf,
    dir: PathBuf,
    firefox: bool,
}

/// Every per-user manifest directory on Linux (`macos == false`) or macOS.
///
/// shortcut: Snap/Flatpak browsers keep their config inside the sandbox
/// (`~/snap/...`, `~/.var/app/...`) and are not covered; add their paths when
/// someone asks, they also need the sandbox's native-messaging portal.
#[cfg_attr(windows, allow(dead_code))]
fn unix_targets(home: &Path, macos: bool) -> Vec<Target> {
    let chromium = |profile: PathBuf| Target {
        dir: profile.join("NativeMessagingHosts"),
        profile,
        firefox: false,
    };
    if macos {
        let support = home.join("Library").join("Application Support");
        vec![
            chromium(support.join("Google").join("Chrome")),
            chromium(support.join("Chromium")),
            chromium(support.join("Microsoft Edge")),
            chromium(support.join("BraveSoftware").join("Brave-Browser")),
            chromium(support.join("Vivaldi")),
            // Firefox keeps profiles under `Firefox` but reads hosts from `Mozilla`.
            Target {
                profile: support.join("Firefox"),
                dir: support.join("Mozilla").join("NativeMessagingHosts"),
                firefox: true,
            },
        ]
    } else {
        let config = home.join(".config");
        vec![
            chromium(config.join("google-chrome")),
            chromium(config.join("chromium")),
            chromium(config.join("microsoft-edge")),
            chromium(config.join("BraveSoftware").join("Brave-Browser")),
            chromium(config.join("vivaldi")),
            Target {
                profile: home.join(".mozilla"),
                dir: home.join(".mozilla").join("native-messaging-hosts"),
                firefox: true,
            },
        ]
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    fn targets() -> Result<Vec<Target>, Error> {
        let home = dirs::home_dir()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory"))?;
        Ok(unix_targets(&home, cfg!(target_os = "macos")))
    }

    pub fn install(chromium: &str, firefox: &str, _force: bool) -> Result<PathBuf, Error> {
        let mut first = None;
        for t in targets()? {
            if !t.profile.is_dir() {
                continue;
            }
            let path = t.dir.join(format!("{HOST_NAME}.json"));
            write_if_changed(&path, if t.firefox { firefox } else { chromium })?;
            first.get_or_insert(path);
        }
        first.ok_or_else(|| Error::NoBrowser(dirs::home_dir().unwrap_or_default()))
    }

    pub fn registered_ids() -> Vec<String> {
        targets()
            .unwrap_or_default()
            .into_iter()
            .filter(|t| !t.firefox)
            .find_map(|t| std::fs::read_to_string(t.dir.join(format!("{HOST_NAME}.json"))).ok())
            .map(|j| ids_in_manifest(&j))
            .unwrap_or_default()
    }

    /// A login item: an XDG autostart entry on Linux, a LaunchAgent on macOS.
    fn autostart_path(home: &Path) -> PathBuf {
        if cfg!(target_os = "macos") {
            home.join("Library/LaunchAgents/app.tdm.plist")
        } else {
            home.join(".config/autostart/tdm.desktop")
        }
    }

    pub fn set_autostart(exe: &Path, on: bool) -> Result<(), Error> {
        let Some(home) = dirs::home_dir() else { return Ok(()) };
        let (path, body) = if cfg!(target_os = "macos") {
            (
                autostart_path(&home),
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\"><dict><key>Label</key><string>app.tdm</string><key>ProgramArguments</key><array><string>{}</string></array><key>RunAtLoad</key><true/></dict></plist>
",
                    exe.display()
                ),
            )
        } else {
            (
                autostart_path(&home),
                format!(
                    "[Desktop Entry]
Type=Application
Name=Turbo Download Manager
Exec=\"{}\"
X-GNOME-Autostart-enabled=true
",
                    exe.display()
                ),
            )
        };
        if !on {
            return Ok(remove_if_present(&path)?);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_if_changed(&path, &body)?;
        Ok(())
    }

    pub fn uninstall() -> Result<(), Error> {
        for t in targets()? {
            remove_if_present(&t.dir.join(format!("{HOST_NAME}.json")))?;
        }
        if let Some(home) = dirs::home_dir() {
            remove_if_present(&autostart_path(&home))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_real_looking_extension_id() {
        assert!(valid_extension_id("abcdefghijklmnopabcdefghijklmnop"));
        assert!(valid_extension_id(CHROME_EXTENSION_ID));
    }

    #[test]
    fn rejects_ids_that_would_silently_never_connect() {
        // Too short, too long, and containing letters outside a-p.
        assert!(!valid_extension_id("abc"));
        assert!(!valid_extension_id("abcdefghijklmnopabcdefghijklmnopq"));
        assert!(!valid_extension_id("zbcdefghijklmnopabcdefghijklmnop"));
        assert!(!valid_extension_id("ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP"));
        assert!(!valid_extension_id(""));
    }

    #[test]
    fn manifest_has_the_fields_chrome_requires() {
        let json = manifest_json(
            Path::new("C:\\apps\\tdm.exe"),
            &["abcdefghijklmnopabcdefghijklmnop".to_owned()],
        )
        .expect("manifest");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["name"], HOST_NAME);
        assert_eq!(v["type"], "stdio");
        assert_eq!(v["path"], "C:\\apps\\tdm.exe");
        assert_eq!(
            v["allowed_origins"][0],
            "chrome-extension://abcdefghijklmnopabcdefghijklmnop/"
        );
        assert!(v.get("allowed_extensions").is_none());
    }

    #[test]
    fn firefox_manifest_lists_addon_ids_not_origins() {
        let json =
            firefox_manifest_json(Path::new("/usr/bin/tdm"), &[FIREFOX_EXTENSION_ID]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["name"], HOST_NAME);
        assert_eq!(v["type"], "stdio");
        assert_eq!(v["path"], "/usr/bin/tdm");
        assert_eq!(v["allowed_extensions"], serde_json::json!(["tdm@tdm.app"]));
        // Firefox rejects a manifest carrying Chrome's key.
        assert!(v.get("allowed_origins").is_none());
    }

    #[test]
    fn ids_read_back_from_a_written_manifest() {
        let ids = vec![
            "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            CHROME_EXTENSION_ID.to_owned(),
        ];
        let json = manifest_json(Path::new("x.exe"), &ids).unwrap();
        assert_eq!(ids_in_manifest(&json), ids);
        assert!(ids_in_manifest("not json").is_empty());
    }

    #[test]
    fn a_bad_id_fails_before_anything_is_written() {
        let err = manifest_json(Path::new("x.exe"), &["nope".to_owned()]).unwrap_err();
        assert!(matches!(err, Error::BadExtensionId(_)));
    }

    #[test]
    fn every_allowed_origin_is_listed() {
        let ids = vec![
            "abcdefghijklmnopabcdefghijklmnop".to_owned(),
            "ponmlkjihgfedcbaponmlkjihgfedcba".to_owned(),
        ];
        let json = manifest_json(Path::new("x.exe"), &ids).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["allowed_origins"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn linux_targets_cover_each_browser_under_the_home_dir() {
        let home = Path::new("/home/u");
        let t = unix_targets(home, false);
        for want in [
            "/home/u/.config/google-chrome/NativeMessagingHosts",
            "/home/u/.config/chromium/NativeMessagingHosts",
            "/home/u/.config/microsoft-edge/NativeMessagingHosts",
            "/home/u/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts",
            "/home/u/.config/vivaldi/NativeMessagingHosts",
            "/home/u/.mozilla/native-messaging-hosts",
        ] {
            // Path equality compares components, so `/` vs `\` does not matter.
            assert!(t.iter().any(|t| t.dir == Path::new(want)), "missing {want}");
        }
        // Each manifest dir sits inside the profile dir it is gated on, and
        // only the Mozilla one uses the Firefox format.
        for t in &t {
            assert!(t.dir.starts_with(&t.profile), "{:?}", t);
            assert_eq!(t.firefox, t.profile.ends_with(".mozilla"));
        }
    }

    #[test]
    fn macos_targets_use_application_support() {
        let home = Path::new("/Users/u");
        let t = unix_targets(home, true);
        let support = Path::new("/Users/u/Library/Application Support");
        assert!(t.iter().all(|t| t.dir.starts_with(support)));
        assert!(t.iter().any(|t| t.dir == support.join("Google/Chrome/NativeMessagingHosts")));
        assert!(t.iter().any(|t| t.dir == support.join("Microsoft Edge/NativeMessagingHosts")));
        let ff: Vec<_> = t.iter().filter(|t| t.firefox).collect();
        assert_eq!(ff.len(), 1);
        assert_eq!(ff[0].dir, support.join("Mozilla/NativeMessagingHosts"));
        assert_eq!(ff[0].profile, support.join("Firefox"));
    }

    #[test]
    fn every_browser_has_an_extensions_page() {
        for b in Browser::ALL {
            assert!(b.extensions_url().contains("://") || b.extensions_url().starts_with("about:"), "{b:?}");
        }
    }

    #[test]
    fn chromium_browsers_load_the_chromium_copy_and_firefox_its_own() {
        assert!(Browser::Chrome.extension_dir().ends_with("chrome"));
        assert!(Browser::Brave.extension_dir().ends_with("chrome"));
        assert!(Browser::Firefox.extension_dir().ends_with("firefox"));
    }

    #[test]
    fn unchanged_content_is_not_rewritten() {
        let dir = std::env::temp_dir().join(format!("tdm-reg-{}", std::process::id()));
        let path = dir.join("sub").join("m.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(write_if_changed(&path, "a").unwrap(), "first write creates the file");
        assert!(!write_if_changed(&path, "a").unwrap(), "identical content is skipped");
        assert!(write_if_changed(&path, "b").unwrap(), "new content is written");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b");
        remove_if_present(&path).unwrap();
        remove_if_present(&path).expect("removing twice is fine");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
