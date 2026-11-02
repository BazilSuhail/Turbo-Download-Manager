//! Domain types.
//!
//! Categories keyed by id with an extension mask and a save path, a download
//! record carrying the replayed request headers, and a column set for the
//! main list.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A file-type category: an id, a space-separated extension mask, and a save directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub id: u32,
    pub name: String,
    /// Lowercase extensions without a dot. `r0*` style globs are stored as-is
    /// and matched by `matches`.
    pub mask: Vec<String>,
    pub path: PathBuf,
}

impl Category {
    /// True when `file_name`'s extension matches this category's mask.
    ///
    /// A mask entry may end in `*` to cover numbered archive parts (`r0*`
    /// matches `r00`..`r09`), the usual naming of multi-part RAR volumes.
    pub fn matches(&self, file_name: &str) -> bool {
        let Some(ext) = file_name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()) else {
            return false;
        };
        self.mask.iter().any(|m| {
            let m = m.to_ascii_lowercase();
            match m.strip_suffix('*') {
                Some(stem) => ext.starts_with(stem),
                None => ext == m,
            }
        })
    }

    /// The default set.
    pub fn defaults(root: &Path) -> Vec<Category> {
        let make = |id, name: &str, mask: &str| Category {
            id,
            name: name.to_owned(),
            mask: mask.split_whitespace().map(str::to_owned).collect(),
            path: root.join(name),
        };
        vec![
            make(1, "Programs", "exe msi msp cab bat com dll"),
            make(2, "Music", "mp3 wav wma m4a aac flac ogg opus mid"),
            make(3, "Video", "mp4 mkv avi mov wmv flv webm mpg mpeg m4v 3gp"),
            make(5, "Documents", "pdf doc docx xls xlsx ppt pptx txt rtf odt epub"),
            make(7, "Compressed", "zip rar r0* r1* arj gz sit sitx sea ace bz2 7z xz zst"),
        ]
    }
}

/// Columns of the main download list, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Column {
    FileName,
    Queue,
    Size,
    Status,
    TimeLeft,
    TransferRate,
    LastTryDate,
    Description,
    SaveTo,
    Referer,
    ParentPage,
    DateAdded,
}

impl Column {
    pub const ALL: [Column; 12] = [
        Column::FileName, Column::Queue, Column::Size, Column::Status,
        Column::TimeLeft, Column::TransferRate, Column::LastTryDate,
        Column::Description, Column::SaveTo, Column::Referer,
        Column::ParentPage, Column::DateAdded,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Column::FileName => "File Name",
            Column::Queue => "Q",
            Column::Size => "Size",
            Column::Status => "Status",
            Column::TimeLeft => "Time left",
            Column::TransferRate => "Transfer rate",
            Column::LastTryDate => "Last try date",
            Column::Description => "Description",
            Column::SaveTo => "Save to",
            Column::Referer => "Referer",
            Column::ParentPage => "Parent web page",
            Column::DateAdded => "Date added",
        }
    }

    /// Width in px and whether the column starts visible. SaveTo, Referer,
    /// ParentPage and DateAdded start hidden.
    pub fn default_spec(self) -> (u16, bool) {
        match self {
            Column::FileName => (150, true),
            Column::Queue => (24, true),
            Column::Size => (80, true),
            Column::Status => (100, true),
            Column::TimeLeft => (80, true),
            Column::TransferRate => (90, true),
            Column::LastTryDate => (110, true),
            Column::Description => (120, true),
            Column::SaveTo => (160, false),
            Column::Referer => (160, false),
            Column::ParentPage => (160, false),
            Column::DateAdded => (110, false),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnSpec {
    pub column: Column,
    pub width: u16,
    pub visible: bool,
}

impl ColumnSpec {
    pub fn defaults() -> Vec<ColumnSpec> {
        Column::ALL
            .iter()
            .map(|&column| {
                let (width, visible) = column.default_spec();
                ColumnSpec { column, width, visible }
            })
            .collect()
    }
}

/// Which Options page is open. Persisted the same way `show_categories` is —
/// it is view state the user expects to find where they left it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionsTab {
    #[default]
    General,
    FileTypes,
    SaveTo,
    Downloads,
    Connection,
    Proxy,
    SitesLogins,
    DialUp,
    Sounds,
}

/// Options ▸ General.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    /// We have no network driver; this flags the intent only.
    pub advanced_integration: bool,
    /// Off by default — adding ourselves to the user's logon without being asked is not ours to do.
    pub launch_on_startup: bool,
    pub monitor_clipboard: bool,
    /// One row per browser.
    pub capture_browsers: Vec<(String, bool)>,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            advanced_integration: true,
            launch_on_startup: false,
            monitor_clipboard: false,
            capture_browsers: [
                "Google Chrome",
                "Internet Explorer",
                "Microsoft Edge",
                "Microsoft Edge Legacy",
                "Mozilla Firefox",
                "Opera",
            ]
            .iter()
            .map(|b| ((*b).to_owned(), true))
            .collect(),
        }
    }
}

/// Options ▸ Save to.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SaveToSettings {
    /// Empty means "beside the destination file", which is where our engine
    /// already keeps its `.tdmstate` sidecar.
    pub temp_dir: PathBuf,
    pub remember_last_path: bool,
    /// Stamp the saved file with the server's `Last-Modified`.
    pub use_server_file_date: bool,
}

/// What to do when a URL already in the list is added again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DuplicateAction {
    #[default]
    Ask,
    NumberedName,
    Overwrite,
    CompleteDialog,
}

impl DuplicateAction {
    pub const ALL: [DuplicateAction; 4] = [
        DuplicateAction::Ask,
        DuplicateAction::NumberedName,
        DuplicateAction::Overwrite,
        DuplicateAction::CompleteDialog,
    ];

    pub fn title(self) -> &'static str {
        match self {
            DuplicateAction::Ask => "Show a dialog and ask what to do",
            DuplicateAction::NumberedName => "Add with a numbered file name",
            DuplicateAction::Overwrite => "Add and overwrite the existing file",
            DuplicateAction::CompleteDialog => "If the existing file is complete, show the complete dialog",
        }
    }
}

impl std::fmt::Display for DuplicateAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.title())
    }
}

/// Options ▸ Downloads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadSettings {
    pub show_start_dialog: bool,
    /// Indented under `show_start_dialog`; only meaningful while it is on.
    pub queue_only: bool,
    pub show_complete_dialog: bool,
    pub start_while_showing_info: bool,
    pub queue_panel_on_download_later: bool,
    pub queue_panel_on_batch_close: bool,
    pub ignore_modification_time: bool,
    pub duplicate_action: DuplicateAction,
    /// User-Agent for manually added downloads. Empty means the engine's own.
    pub user_agent: String,
    /// After a download finishes, compare its real type with its name and warn on a mismatch.
    #[serde(default = "default_true")]
    pub check_file_type: bool,
    /// Flash the taskbar button when a download finishes while TDM is in the background.
    #[serde(default = "default_true")]
    pub flash_on_complete: bool,
}

impl Default for DownloadSettings {
    fn default() -> Self {
        Self {
            show_start_dialog: true,
            queue_only: false,
            show_complete_dialog: true,
            start_while_showing_info: true,
            queue_panel_on_download_later: true,
            queue_panel_on_batch_close: true,
            ignore_modification_time: false,
            duplicate_action: DuplicateAction::default(),
            user_agent: String::new(),
            check_file_type: true,
            flash_on_complete: true,
        }
    }
}

/// Options ▸ Connection. The max-connections
/// number itself is `Settings::max_segments`, which already existed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionSettings {
    pub limit_enabled: bool,
    /// `Download no more than [n] MBytes`.
    pub limit_mbytes: u32,
    /// `every [n] hours`.
    pub limit_hours: u32,
    pub warn_before_stopping: bool,
    /// Per-segment retry budget and socket
    /// timeout. Applied through `Download::set_limits`.
    pub retries: u32,
    pub timeout_secs: u32,
    /// Retry a download automatically after a transient failure (timeout,
    /// connection error, 408/429/5xx, short read).
    #[serde(default = "default_true")]
    pub auto_retry: bool,
    /// How many automatic retries a download gets in a row. Clamped to
    /// `AUTO_RETRY_MAX` when used.
    #[serde(default = "default_auto_retry_max")]
    pub auto_retry_max: u32,
}

/// Default number of automatic retries per download.
fn default_auto_retry_max() -> u32 {
    3
}

/// Upper bound for `ConnectionSettings::auto_retry_max`.
pub const AUTO_RETRY_MAX: u32 = 10;

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            limit_enabled: false,
            limit_mbytes: 200,
            limit_hours: 5,
            warn_before_stopping: true,
            retries: 5,
            timeout_secs: 30,
            auto_retry: true,
            auto_retry_max: default_auto_retry_max(),
        }
    }
}

/// The four radios of Options ▸ Proxy / Socks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyMode {
    #[default]
    None,
    System,
    Pac,
    Manual,
}

impl ProxyMode {
    pub const ALL: [ProxyMode; 4] =
        [ProxyMode::None, ProxyMode::System, ProxyMode::Pac, ProxyMode::Manual];

    pub fn title(self) -> &'static str {
        match self {
            ProxyMode::None => "No proxy/socks",
            ProxyMode::System => "Use system settings",
            ProxyMode::Pac => "Use automatic configuration script",
            ProxyMode::Manual => "Manual proxy/socks configuration",
        }
    }
}

/// Options ▸ Proxy / Socks. SOCKS is not modelled here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySettings {
    pub mode: ProxyMode,
    /// The PAC URL.
    pub pac_url: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Deliberately **not** persisted: writing a credential to settings.json
    /// in clear is not acceptable.
    /// shortcut: session-only password, revisit when we have a keyring.
    #[serde(skip)]
    pub password: String,
    pub use_http: bool,
    pub use_https: bool,
    pub use_ftp: bool,
    pub ftp_passive: bool,
    /// Reuse the browser's proxy after an intercepted download fails.
    pub use_browser_proxy_on_error: bool,
}

impl ProxySettings {
    /// The proxy the engine should apply, or `None` when none does.
    ///
    /// `Manual` uses the fields on the page. `System` reads the user's Windows
    /// proxy (what the "Get System" button copies in), and
    /// applies it to both protocols — the system setting has no per-protocol
    /// ticks. `Pac` resolves to nothing: evaluating a PAC script needs a
    /// JavaScript engine, so the page says so rather than pretending.
    pub fn resolved(&self) -> Option<crate::engine::ProxyConfig> {
        match self.mode {
            ProxyMode::None | ProxyMode::Pac => None,
            ProxyMode::Manual => {
                if self.host.trim().is_empty() || self.port == 0 {
                    return None;
                }
                // A host already carrying a scheme must not become
                // `http://http://…`.
                let url = if self.host.contains("://") {
                    format!("{}:{}", self.host.trim(), self.port)
                } else {
                    format!("http://{}:{}", self.host.trim(), self.port)
                };
                Some(crate::engine::ProxyConfig {
                    url,
                    http: self.use_http,
                    https: self.use_https,
                    user: self.user.clone(),
                    password: self.password.clone(),
                })
            }
            ProxyMode::System => system_proxy().map(|url| crate::engine::ProxyConfig {
                url,
                http: true,
                https: true,
                user: String::new(),
                password: String::new(),
            }),
        }
    }
}

/// The user's Windows proxy, as `http://host:port`.
///
/// Read from the same per-user key Internet Explorer and Chrome use. A
/// per-protocol list (`http=a:1;https=b:2`) is reduced to its `http=` entry,
/// since we apply one server to both.
#[cfg(windows)]
fn system_proxy() -> Option<String> {
    let root = std::env::var_os("SystemRoot").map_or_else(
        || std::path::PathBuf::from(r"C:\Windows"),
        std::path::PathBuf::from,
    );
    let out = std::process::Command::new(root.join("System32").join("reg.exe"))
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyServer",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let value = text
        .lines()
        .find_map(|l| l.split_once("REG_SZ").map(|(_, v)| v.trim()))
        .filter(|v| !v.is_empty())?;
    Some(normalize_proxy_server(value))
}

/// The conventional proxy variables, which desktop environments and shells
/// on Linux export.
#[cfg(not(windows))]
fn system_proxy() -> Option<String> {
    proxy_from_env(|k| std::env::var(k).ok())
}

/// First usable proxy URL among the variables, most specific first. SOCKS and
/// other schemes are skipped: the engine only speaks HTTP proxies.
#[cfg_attr(windows, allow(dead_code))]
fn proxy_from_env(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
        .into_iter()
        .filter_map(get)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .map(|v| normalize_proxy_server(&v))
        .find(|url| url.starts_with("http://") || url.starts_with("https://"))
}

/// Host and port of the user's system proxy, for the Options page's
/// "Get System" button.
pub fn system_proxy_endpoint() -> Option<(String, u16)> {
    let url = system_proxy()?;
    let rest = url.split_once("://").map_or(url.clone(), |(_, r)| r.to_owned());
    let (host, port) = rest.trim_end_matches('/').rsplit_once(':')?;
    Some((host.to_owned(), port.parse().ok()?))
}

/// `host:port`, or `http=host:port;https=…`, to a proxy URL.
fn normalize_proxy_server(value: &str) -> String {
    let one = value
        .split(';')
        .find_map(|part| part.strip_prefix("http="))
        .unwrap_or_else(|| value.split(';').next().unwrap_or(value))
        .trim();
    if one.contains("://") { one.to_owned() } else { format!("http://{one}") }
}

#[cfg(test)]
mod proxy_env_tests {
    use super::proxy_from_env;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn https_proxy_wins_and_a_bare_host_gets_a_scheme() {
        let vars = [("http_proxy", "http://a:1"), ("HTTPS_PROXY", "b:2")];
        assert_eq!(proxy_from_env(env(&vars)).as_deref(), Some("http://b:2"));
    }

    #[test]
    fn socks_and_empty_values_are_skipped() {
        let vars = [("HTTPS_PROXY", " "), ("ALL_PROXY", "socks5://s:1080"), ("http_proxy", "c:3")];
        assert_eq!(proxy_from_env(env(&vars)).as_deref(), Some("http://c:3"));
        assert_eq!(proxy_from_env(env(&[("all_proxy", "socks5h://s:1")])), None);
        assert_eq!(proxy_from_env(env(&[])), None);
    }
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            mode: ProxyMode::None,
            pac_url: String::new(),
            host: String::new(),
            port: 0,
            user: String::new(),
            password: String::new(),
            use_http: false,
            use_https: false,
            use_ftp: false,
            ftp_passive: false,
            use_browser_proxy_on_error: true,
        }
    }
}

/// What TDM does once its queue has no running or waiting downloads left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AfterQueue {
    /// Keep running.
    #[default]
    DoNothing,
    /// Quit TDM.
    CloseApp,
    /// Quit TDM and shut the computer down in 60 seconds (Windows and Linux).
    PowerOff,
}

impl AfterQueue {
    pub const ALL: [AfterQueue; 3] = [AfterQueue::DoNothing, AfterQueue::CloseApp, AfterQueue::PowerOff];

    pub fn title(self) -> &'static str {
        match self {
            AfterQueue::DoNothing => "Keep TDM open",
            AfterQueue::CloseApp => "Close TDM",
            AfterQueue::PowerOff => "Close TDM and shut down the PC",
        }
    }
}

impl std::fmt::Display for AfterQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.title())
    }
}

/// A time-of-day bandwidth cap. While the clock is inside `start..end` the
/// window's `limit` replaces the global one. A window whose `start` is later
/// than its `end` wraps past midnight, so 22:00–07:00 covers the night.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedWindow {
    pub start: crate::scheduler::TimeOfDay,
    pub end: crate::scheduler::TimeOfDay,
    /// Bytes per second inside the window; `None` means unlimited there.
    pub limit: Option<u64>,
}

impl SpeedWindow {
    /// Whether `now` falls inside the window. `start == end` is an empty window.
    pub fn contains(&self, now: crate::scheduler::TimeOfDay) -> bool {
        let start = self.start.minutes_since_midnight();
        let end = self.end.minutes_since_midnight();
        let t = now.minutes_since_midnight();
        if start <= end {
            start <= t && t < end
        } else {
            t >= start || t < end
        }
    }
}

/// The cap in force at `now`: the first window that contains the time, or
/// the global limit when none does.
pub fn effective_limit(
    global: Option<u64>,
    windows: &[SpeedWindow],
    now: crate::scheduler::TimeOfDay,
) -> Option<u64> {
    windows.iter().find(|w| w.contains(now)).map_or(global, |w| w.limit)
}

/// Application settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub download_dir: PathBuf,
    /// File types taken over from a browser.
    pub capture_extensions: Vec<String>,
    /// Hosts never taken over.
    pub exception_hosts: Vec<String>,
    /// Max simultaneous downloads.
    pub max_active: usize,
    /// Max segments per download.
    pub max_segments: u32,
    /// Global cap in bytes/second; `None` means unlimited.
    pub speed_limit: Option<u64>,
    /// Time-of-day caps that override `speed_limit` while they are open.
    #[serde(default)]
    pub speed_windows: Vec<SpeedWindow>,
    /// What to do when the queue runs out of work.
    #[serde(default)]
    pub after_queue: AfterQueue,
    pub categories: Vec<Category>,
    pub columns: Vec<ColumnSpec>,
    /// Whether the category panel is expanded in the main window.
    pub show_categories: bool,
    /// The Options page last looked at.
    #[serde(default)]
    pub options_tab: OptionsTab,
    #[serde(default)]
    pub general: GeneralSettings,
    #[serde(default)]
    pub save_to: SaveToSettings,
    #[serde(default)]
    pub downloads: DownloadSettings,
    #[serde(default)]
    pub connection: ConnectionSettings,
    #[serde(default)]
    pub proxy: ProxySettings,
    /// When the queue is allowed to run. Off by default, so a fresh install
    /// behaves as if there were no scheduler at all.
    #[serde(default)]
    pub schedule: crate::scheduler::Schedule,
    /// Name and extensions half-typed into the new-category row. UI state, so
    /// it is not persisted.
    #[serde(skip)]
    pub draft_category: (String, String),
    /// TDM Light instead of the default TDM Dark.
    #[serde(default)]
    pub light_theme: bool,
    /// Raise and focus the window when a browser hands over a download.
    #[serde(default = "default_true")]
    pub bring_to_front_on_capture: bool,
    /// Take over every file download from the browser, not only the listed types.
    #[serde(default = "default_true")]
    pub capture_all: bool,
    /// Command-line virus scanner run on each finished download; empty turns it off.
    #[serde(default = "default_scanner")]
    pub scanner: String,
    /// Main window size and position, restored at startup. `None` until the
    /// window has been resized or moved once.
    #[serde(default)]
    pub window: Option<WindowGeometry>,
}

/// The main window's last size and, when known, its screen position.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
}

fn default_true() -> bool {
    true
}

/// Windows Defender's command-line scanner when it is installed. Empty means no scan.
fn default_scanner() -> String {
    if cfg!(windows) {
        let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
        let exe = program_files.join(r"Windows Defender\MpCmdRun.exe");
        if exe.is_file() {
            return exe.display().to_string();
        }
    }
    String::new()
}

/// Accepted ranges for the numeric settings. Anything outside is clamped
/// rather than rejected, so a hand-edited file or a held-down stepper cannot
/// ask the engine for 9000 connections.
pub const CONNECTIONS: std::ops::RangeInclusive<u32> = 1..=32;
pub const RETRIES: std::ops::RangeInclusive<u32> = 0..=99;
pub const TIMEOUT_SECS: std::ops::RangeInclusive<u32> = 5..=600;
pub const LIMIT_MBYTES: std::ops::RangeInclusive<u32> = 1..=1_000_000;
pub const LIMIT_HOURS: std::ops::RangeInclusive<u32> = 1..=720;

impl Default for Settings {
    fn default() -> Self {
        let download_dir = dirs_download();
        Self {
            categories: Category::defaults(&download_dir),
            download_dir,
            capture_extensions: "3GP 7Z AAC ACE AVI BZ2 DOC DOCX EXE GZ ISO MKV MP3 MP4 MPEG \
                                 MSI OGG PDF PPT RAR SIT TAR WAV WMA WMV XLS XLSX ZIP"
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            exception_hosts: Vec::new(),
            max_active: 3,
            max_segments: 8,
            speed_limit: None,
            speed_windows: Vec::new(),
            after_queue: AfterQueue::default(),
            columns: ColumnSpec::defaults(),
            show_categories: true,
            options_tab: OptionsTab::default(),
            general: GeneralSettings::default(),
            save_to: SaveToSettings::default(),
            downloads: DownloadSettings::default(),
            connection: ConnectionSettings::default(),
            proxy: ProxySettings::default(),
            schedule: crate::scheduler::Schedule::default(),
            draft_category: (String::new(), String::new()),
            light_theme: false,
            bring_to_front_on_capture: true,
            capture_all: true,
            scanner: default_scanner(),
            window: None,
        }
    }
}

impl Settings {
    /// The category whose mask claims this file name, if any.
    pub fn category_for(&self, file_name: &str) -> Option<&Category> {
        self.categories.iter().find(|c| c.matches(file_name))
    }

    /// What the browser extension is told to capture: every download when
    /// `capture_all` is on, otherwise the listed types.
    pub fn capture_list(&self) -> Vec<String> {
        if self.capture_all { vec!["*".to_owned()] } else { self.capture_extensions.clone() }
    }

    /// Where a file should be saved: its category's directory, else the root.
    pub fn destination_for(&self, file_name: &str) -> PathBuf {
        self.category_for(file_name)
            .map(|c| c.path.clone())
            .unwrap_or_else(|| self.download_dir.clone())
            .join(file_name)
    }
}

fn dirs_download() -> PathBuf {
    // `USERPROFILE` alone is Windows-only: elsewhere it fell through to a
    // path relative to whatever directory the GUI was launched from.
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("downloads"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            download_dir: PathBuf::from("D:\\dl"),
            categories: Category::defaults(&PathBuf::from("D:\\dl")),
            ..Settings::default()
        }
    }

    #[test]
    fn category_mask_matches_plain_extension() {
        let s = settings();
        assert_eq!(s.category_for("song.mp3").map(|c| c.id), Some(2));
        assert_eq!(s.category_for("clip.MKV").map(|c| c.id), Some(3));
        assert_eq!(s.category_for("setup.exe").map(|c| c.id), Some(1));
    }

    #[test]
    fn starred_mask_covers_numbered_archive_parts() {
        let s = settings();
        // r0* must cover r00..r09.
        assert_eq!(s.category_for("movie.r00").map(|c| c.id), Some(7));
        assert_eq!(s.category_for("movie.r09").map(|c| c.id), Some(7));
        assert_eq!(s.category_for("movie.r15").map(|c| c.id), Some(7));
    }

    #[test]
    fn unknown_extension_falls_back_to_the_root_directory() {
        let s = settings();
        assert!(s.category_for("notes.xyz").is_none());
        assert_eq!(s.destination_for("notes.xyz"), PathBuf::from("D:\\dl\\notes.xyz"));
    }

    #[test]
    fn a_file_with_no_extension_matches_nothing() {
        let s = settings();
        assert!(s.category_for("README").is_none());
    }

    #[test]
    fn categorised_file_lands_in_its_category_directory() {
        let s = settings();
        assert_eq!(
            s.destination_for("album.flac"),
            PathBuf::from("D:\\dl\\Music\\album.flac")
        );
    }

    #[test]
    fn settings_round_trip_through_serde() {
        let mut s = settings();
        s.connection.retries = 9;
        s.connection.timeout_secs = 120;
        s.downloads.duplicate_action = DuplicateAction::Overwrite;
        s.downloads.user_agent = "agent/1".into();
        s.proxy.mode = ProxyMode::Manual;
        s.proxy.host = "proxy.invalid".into();
        s.proxy.port = 8080;
        s.general.capture_browsers[1].1 = false;
        s.options_tab = OptionsTab::Proxy;

        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        // Everything but the password, which is session-only by design.
        assert_eq!(back, s);
    }

    #[test]
    fn a_proxy_password_is_never_written_to_disk() {
        let mut s = Settings::default();
        s.proxy.password = "hunter2".into();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("hunter2"), "the password reached the settings file");
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert!(back.proxy.password.is_empty());
    }

    #[test]
    fn a_settings_file_from_before_these_fields_still_loads() {
        // Exactly the shape the previous build wrote.
        let old = r#"{
            "download_dir": "D:\\dl",
            "capture_extensions": ["ZIP"],
            "exception_hosts": [],
            "max_active": 4,
            "max_segments": 16,
            "speed_limit": null,
            "categories": [],
            "columns": [],
            "show_categories": false
        }"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.max_active, 4);
        assert_eq!(s.max_segments, 16);
        assert!(!s.show_categories);
        // Everything new falls back to its default.
        assert_eq!(s.options_tab, OptionsTab::General);
        assert_eq!(s.connection, ConnectionSettings::default());
        assert_eq!(s.proxy.mode, ProxyMode::None);
        assert_eq!(s.general.capture_browsers.len(), 6);
        assert!(s.downloads.show_start_dialog);
    }

    /// Defaults must not carry anyone's machine-specific paths or servers.
    #[test]
    fn defaults_carry_no_machine_specific_data() {
        let mut s = Settings::default();
        // download_dir and the category paths are derived from the environment
        // at run time, so they legitimately contain the user's name.
        let root = PathBuf::from("X:\\dl");
        s.categories = Category::defaults(&root);
        s.download_dir = root;

        let json = serde_json::to_string(&s).unwrap().to_ascii_lowercase();
        for leaked in [
            "appdata",
            "roaming",
            "windows nt",
            "127.0.0.1",
        ] {
            assert!(!json.contains(leaked), "Settings::default() leaks {leaked:?}");
        }
        assert!(s.proxy.host.is_empty());
        assert!(s.proxy.user.is_empty());
        assert!(s.save_to.temp_dir.as_os_str().is_empty());
        assert!(s.downloads.user_agent.is_empty());
    }

    #[test]
    fn four_columns_start_hidden() {
        let hidden: Vec<_> = ColumnSpec::defaults()
            .into_iter()
            .filter(|c| !c.visible)
            .map(|c| c.column)
            .collect();
        assert_eq!(
            hidden,
            vec![Column::SaveTo, Column::Referer, Column::ParentPage, Column::DateAdded]
        );
    }

    #[test]
    fn a_manual_proxy_becomes_a_url_the_engine_can_use() {
        let mut p = ProxySettings {
            mode: ProxyMode::Manual,
            host: "proxy.lan".into(),
            port: 3128,
            use_http: true,
            ..ProxySettings::default()
        };
        let c = p.resolved().expect("a manual proxy resolves");
        assert_eq!(c.url, "http://proxy.lan:3128");
        assert!(c.http && !c.https);

        // A host that already carries a scheme must not be prefixed twice.
        p.host = "http://proxy.lan".into();
        assert_eq!(p.resolved().unwrap().url, "http://proxy.lan:3128");
    }

    #[test]
    fn an_incomplete_or_disabled_proxy_resolves_to_nothing() {
        // No port: applying `http://host:0` would fail every request.
        let p = ProxySettings {
            mode: ProxyMode::Manual,
            host: "proxy.lan".into(),
            ..ProxySettings::default()
        };
        assert!(p.resolved().is_none());
        assert!(ProxySettings { mode: ProxyMode::Manual, ..ProxySettings::default() }
            .resolved()
            .is_none());
        assert!(ProxySettings::default().resolved().is_none());
        // PAC needs a script engine we do not have; it must not silently
        // behave as a direct connection to some other host.
        let pac = ProxySettings {
            mode: ProxyMode::Pac,
            pac_url: "http://example.test/p.pac".into(),
            ..ProxySettings::default()
        };
        assert!(pac.resolved().is_none());
    }

    #[test]
    fn credentials_travel_with_the_proxy_rather_than_in_its_url() {
        let p = ProxySettings {
            mode: ProxyMode::Manual,
            host: "proxy.lan".into(),
            port: 8080,
            user: "u@example".into(),
            password: "p:w/d".into(),
            use_https: true,
            ..ProxySettings::default()
        };
        let c = p.resolved().unwrap();
        // Characters that would need escaping in a URL are carried as fields.
        assert_eq!(c.url, "http://proxy.lan:8080");
        assert_eq!(c.user, "u@example");
        assert_eq!(c.password, "p:w/d");
    }

    #[test]
    fn a_per_protocol_system_proxy_list_reduces_to_its_http_entry() {
        assert_eq!(
            normalize_proxy_server("http=a.lan:1;https=b.lan:2"),
            "http://a.lan:1"
        );
        assert_eq!(normalize_proxy_server("plain.lan:8080"), "http://plain.lan:8080");
        assert_eq!(normalize_proxy_server("http://already.lan:3"), "http://already.lan:3");
    }

    #[test]
    fn old_settings_without_retry_flash_or_window_fields_load_with_defaults() {
        // Serialize today's defaults, then strip the fields added after
        // settings files were first written. What is left is an old file.
        let mut v = serde_json::to_value(Settings::default()).unwrap();
        v.as_object_mut().unwrap().remove("window");
        v["downloads"].as_object_mut().unwrap().remove("flash_on_complete");
        v["connection"].as_object_mut().unwrap().remove("auto_retry");
        v["connection"].as_object_mut().unwrap().remove("auto_retry_max");

        let s: Settings = serde_json::from_value(v).expect("old settings must load");
        assert!(s.connection.auto_retry);
        assert_eq!(s.connection.auto_retry_max, 3);
        assert!(s.downloads.flash_on_complete);
        assert_eq!(s.window, None);
    }

    #[test]
    fn window_geometry_position_is_optional() {
        let g: WindowGeometry = serde_json::from_str(r#"{"width": 900.0, "height": 600.0}"#).unwrap();
        assert_eq!((g.x, g.y), (None, None));
        let full = WindowGeometry { width: 900.0, height: 600.0, x: Some(10.0), y: Some(20.0) };
        let back: WindowGeometry = serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
        assert_eq!(back, full);
    }
}
