// Release builds open no console window; the browser still talks to the native host over the pipes it passes in.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Application shell.
//!
//! The familiar download-manager layout — menu bar, toolbar, link bar,
//! category sidebar on the left, download list on the right, status bar —
//! in the TDM look.
//!
//! Downloads run on Tokio tasks; the UI only reads snapshots on a timer, so a
//! slow or stalled transfer can never block the interface.
//!
//! Animation is timestamp-driven: `now` advances on window frames, and the
//! frame subscription exists only while something is moving, so an idle
//! window costs no CPU.

mod batch;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use iced::animation::{Animation, Easing};
use iced::keyboard::{self, key::Named, Key};
use iced::widget::{
    button, checkbox, column, container, float, image, mouse_area, opaque, progress_bar, row,
    scrollable, stack, svg, text, text_editor, text_input, tooltip, Space,
};
use iced::{window, Alignment, Color, Element, Length, Radians, Subscription, Task, Theme, Vector};

use tdm::engine::Limiter;
use tdm::filetype::{self, Kind, Verdict};
use tdm::ipc::{Advert, Incoming, Inbox};
use tdm::model::{AUTO_RETRY_MAX, OptionsTab, Settings, WindowGeometry};
use tdm::store::{DownloadList, DownloadRecord, Store};
use tdm::ui::assets::{self, Icon};
use tdm::ui::fit::middle_ellipsis;
use tdm::ui::menu::{Cmd, Item, MENU_BAR};
use tdm::ui::theme::{self, alpha, blend, Palette};
use tdm::{Download, Snapshot, Status, next_to_start};

const TICK: Duration = Duration::from_millis(500);
/// Running downloads only need the progress shimmer, which reads fine at a
/// fraction of the display rate.
const SHIMMER_TICK: Duration = Duration::from_millis(100);
const TOAST_LIFE: Duration = Duration::from_millis(4500);
/// A toast that offers an action stays long enough to be acted on.
const TOAST_ACTION_LIFE: Duration = Duration::from_secs(9);
const TOAST_FADE: Duration = Duration::from_millis(400);
/// A toast slides in over this long; only then, and while fading, does it need frames.
const TOAST_ENTER: Duration = Duration::from_millis(250);
const SPARK_LEN: usize = 48;
/// A Hello this recent means the browser's extension is talking to TDM.
const CONNECTED_WITHIN: Duration = Duration::from_secs(10 * 60);
const URL_INPUT: &str = "url";
const SEARCH_INPUT: &str = "search";
/// The "type N to confirm" box in the delete dialog.
const CONFIRM_INPUT: &str = "confirm-delete";
/// Select mode fades and the check boxes grow over this long.
const SELECT_FADE: Duration = Duration::from_millis(180);
/// A freshly ticked row settles into place over this long.
const MARK_FLASH: Duration = Duration::from_millis(180);
/// Width of the check box column in select mode, at full size.
const CHECK_SLOT: f32 = 28.0;
/// Height of the bulk action bar when fully open. It unrolls to this.
const BAR_HEIGHT: f32 = 44.0;

/// Left-hand tree selection: everything, one category, or a status folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    All,
    Category(u32),
    Unfinished,
    Finished,
}

/// Why a download stopped: the error text, and whether a retry might succeed.
#[derive(Debug, Clone)]
struct Failure {
    text: String,
    transient: bool,
}

#[derive(Debug, Clone)]
enum Message {
    UrlChanged(String),
    AddUrl,
    SearchChanged(String),
    /// Enter in the search box. Deliberately does nothing, so the list keeps its selection.
    SearchSubmit,
    Tick,
    /// A display frame (or a shimmer tick): advance animations.
    Frame(Instant),
    MenuOpen(Option<usize>),
    Command(Cmd),
    Select(u64),
    SelectFilter(Filter),
    TogglePause(u64),
    Remove(u64),
    /// Enter or leave select mode. Leaving clears every mark.
    ToggleSelectMode(bool),
    /// Tick or untick one download (entering select mode if needed).
    Mark(u64),
    /// Tick or untick every visible download.
    MarkAllVisible(bool),
    /// Drop every mark, keeping select mode on.
    ClearMarks,
    /// Open the confirmation for every marked download.
    DeleteMarked,
    /// A click on a row, with Ctrl or not, and in select mode or not.
    RowPress(u64),
    /// The bar's "Also delete files from disk" tick.
    BulkDeleteFiles(bool),
    /// Show or hide one row's action strip.
    RowMenu(Option<u64>),
    OpenFile(u64),
    ShowInFolder(u64),
    CopyLink(u64),
    Verify(u64),
    VerifyExpected(String),
    VerifyDone(u64, Result<tdm::checksum::Digests, String>),
    BatchMirrors(text_editor::Action),
    /// Drop the always-on-top pin set by a browser hand-off.
    ReleaseLevel(window::Id),
    /// The window was resized to this size, in logical pixels.
    Resized(iced::Size),
    /// The window was moved to this position.
    WindowMoved(iced::Point),
    /// The window gained (true) or lost (false) keyboard focus.
    WindowFocus(bool),
    /// Copy the command that builds the browser extension folders.
    CopyBuildCommand,
    /// Open the folder picker to change the default download folder.
    ChangeDownloadFolder,
    /// The folder picker finished; `None` if the user cancelled.
    DownloadFolderPicked(Option<std::path::PathBuf>),
    CloseVerify,
    BatchEdit(text_editor::Action),
    BatchAdd,
    BatchClose,
    SettingsOpen(bool),
    ToggleCapture(bool),
    Finished(u64, Result<(), Failure>),
    /// The typed confirmation changed.
    ConfirmTyped(String),
    /// Files that could not be deleted, as (download id, reason).
    DiscardedMany(Vec<(u64, String)>),
    ConfirmDelete,
    ConfirmDeleteToggleFile(bool),
    CancelDelete,
    /// A change made on one of the Preferences pages.
    Options(tdm::ui::options::Message),
    /// Window close or Quit: stop workers, persist, then quit.
    Shutdown,
    /// What the clipboard watcher read.
    Clipboard(Option<String>),
    PasteUrl(Option<String>),
    /// The sidebar's "Set up" button for one browser, and its outcome.
    OpenBrowserGuide,
    CloseBrowserGuide,
    Scanned(u64, tdm::scan::Verdict),
    GrabOpen,
    GrabUrl(String),
    GrabStart,
    GrabDone(Result<Vec<String>, String>),
    GrabClose,
    CaptureStart,
    CaptureQueue,
    CaptureCancel,
    /// The file-name field of a media prompt changed: the new stem.
    CaptureName(String),
    /// Open the folder picker for the front media prompt's save folder.
    CaptureFolder,
    /// The folder picker finished for the prompt with this sequence number.
    CaptureFolderPicked(u64, Option<PathBuf>),
    AddToBrowser(tdm::register::Browser),
    /// The native-host registration behind "Add to <browser>" finished.
    SetUpDone(tdm::register::Browser, Result<(), String>),
    /// The launch-at-login change finished.
    LaunchAtLoginDone(Result<(), String>),
    /// Copy the folder to load unpacked to the clipboard.
    CopyExtensionPath(tdm::register::Browser),
    /// Open the folder to load unpacked in the file manager.
    OpenExtensionFolder(tdm::register::Browser),
    ToastAction(u64),
    DismissToast(u64),
    Key(keyboard::Event),
    /// Rename the finished file to the extension its content has.
    TypeRename(u64),
    /// The rename finished: the name it reserved up front, and the new path or
    /// why it did not happen.
    Renamed(u64, PathBuf, Result<PathBuf, String>),
    /// Open the download's own address in the browser, not the saved file.
    TypeOpenPage(u64),
    /// Close the type dialog and keep the file as it is.
    TypeKeep,
}

/// The type warning shown after a finished download, with what it needs to draw itself.
struct TypePrompt {
    id: u64,
    kind: Kind,
    content_type: Option<String>,
    verdict: Verdict,
}

/// A pending delete, awaiting confirmation.
struct ConfirmDelete {
    /// The downloads to remove, snapshotted when the dialog opened.
    ids: Vec<u64>,
    /// Opt-in: the list entry goes either way, the bytes on disk only if this
    /// is ticked.
    also_delete_file: bool,
    /// What the user has typed in the "Type N to confirm" box.
    typed: String,
    /// How many of `ids` are running, kept current by the tick.
    running: usize,
}

/// How hard a delete must be confirmed. The stronger the consequence, the
/// more deliberate the gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    /// One download: a plain yes or no.
    Simple,
    /// A few idle downloads: a count and a list of names, no typing.
    Counted,
    /// Many downloads, running ones, or files on disk: the user types the count.
    Typed,
}

fn confirm_tier(n: usize, running: usize, files: bool) -> Tier {
    if n <= 1 {
        Tier::Simple
    } else if n >= 10 || running > 0 || files {
        Tier::Typed
    } else {
        Tier::Counted
    }
}

/// Whether the typed confirmation names the right count.
fn typed_ok(n: usize, typed: &str) -> bool {
    typed.trim() == n.to_string()
}

/// Drop marks for downloads that no longer exist.
fn prune_marks(marked: &mut BTreeSet<u64>, live: &HashMap<u64, Status>) {
    marked.retain(|id| live.contains_key(id));
}

/// What a click on a row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowAction {
    /// Select mode: tick or untick the row, as the check box does.
    Mark,
    /// Ctrl held outside select mode: tick or untick the row, and stay out of select mode.
    Toggle,
    /// A plain click: select the row.
    Select,
}

fn row_press(select_mode: bool, ctrl: bool) -> RowAction {
    if select_mode {
        RowAction::Mark
    } else if ctrl {
        RowAction::Toggle
    } else {
        RowAction::Select
    }
}

struct Grab {
    url: String,
    working: bool,
    error: Option<String>,
}

struct Batch {
    content: text_editor::Content,
    /// Optional mirror URLs, one per line, given to every link in the batch.
    mirrors: text_editor::Content,
    /// The expansion of `content`, kept current on every edit so the view
    /// does not re-expand ten thousand links per frame.
    links: Result<Vec<String>, batch::Error>,
}

struct Verify {
    id: u64,
    name: String,
    expected: String,
    /// `None` while hashing.
    digest: Option<Result<tdm::checksum::Digests, String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToastKind {
    Info,
    Success,
    /// Something to look at, not a failure: the file is kept either way.
    Warning,
    Error,
}

struct Toast {
    id: u64,
    kind: ToastKind,
    text: String,
    born: Instant,
    /// A link the toast offers to download.
    offer: Option<String>,
}

impl Toast {
    fn life(&self) -> Duration {
        if self.offer.is_some() { TOAST_ACTION_LIFE } else { TOAST_LIFE }
    }
}

struct Entry {
    id: u64,
    /// Result of the virus scan, once the download has finished and been scanned.
    scan: Option<tdm::scan::Verdict>,
    /// A virus scan was started this session and has not reported yet.
    scanning: bool,
    name: String,
    handle: Download,
    snapshot: Snapshot,
    category_id: Option<u32>,
    /// Per-download request headers (Referer, Cookie, User-Agent). Kept here
    /// so they survive a restart; the engine holds its own copy internally.
    headers: Vec<(String, String)>,
    /// Whether this entry's handle has been run in this session. Restored
    /// entries start `false` so their status is not overwritten by an unrun
    /// handle's default snapshot.
    started: bool,
    /// Paused by the schedule's stop time rather than by the user, so the
    /// next window may resume it. A download the user paused stays paused.
    paused_by_schedule: bool,
    last_done: u64,
    last_tick: Instant,
    speed: f64,
    /// Automatic retries used in a row. Session-only, reset by a success.
    retries: u32,
    /// When a Failed download is due for its automatic retry. Session-only.
    retry_at: Option<Instant>,
}

struct App {
    url: String,
    search: String,
    downloads: Vec<Entry>,
    next_id: u64,
    /// The keyboard cursor: the row Space and Delete act on.
    selected: Option<u64>,
    /// Downloads ticked in select mode. Separate from `selected`.
    marked: BTreeSet<u64>,
    /// Whether rows show check boxes and clicks tick rather than select.
    select_mode: bool,
    /// Drives the fade-in of select mode and the growing check column.
    select_anim: Animation<bool>,
    /// Drives the bulk action bar's slide: shown while anything is ticked or select mode is on.
    bar_anim: Animation<bool>,
    /// Whether Ctrl is held, from ModifiersChanged. A row click with Ctrl ticks instead of opening.
    ctrl_down: bool,
    /// The bar's "Also delete files from disk" tick, used when Delete N opens the dialog.
    bulk_delete_files: bool,
    /// Downloads whose rename is in progress. They are not deleted until it lands.
    renaming: BTreeSet<u64>,
    /// The row ticked most recently, and when, for its settling animation.
    mark_flash: Option<(u64, Instant)>,
    row_menu: Option<u64>,
    filter: Filter,
    /// Browsers installed when the app started; listed in the sidebar.
    browsers: Vec<tdm::register::Browser>,
    /// The add-TDM-to-your-browser page.
    browser_guide: bool,
    /// The "Grab site" dialog, while it is open.
    grab: Option<Grab>,
    /// Browser downloads waiting for Download now or Add to queue.
    prompts: VecDeque<Incoming>,
    /// The stem typed into the front media prompt's name field; `None` until
    /// the field is edited. The extension is never part of it.
    prompt_name: Option<String>,
    /// The folder picked for the front media prompt; `None` for the default.
    prompt_dir: Option<PathBuf>,
    /// Bumped each time the front prompt is removed, so a folder pick that
    /// arrives late cannot land on the prompt after it.
    prompt_seq: u64,
    /// Set once the queue starts a download, cleared when the after-queue
    /// action runs, so that action fires once per run of the queue.
    queue_busy: bool,
    /// Current window width, so the sidebar can grow with the window.
    window_width: f32,
    open_menu: Option<usize>,
    settings_open: bool,
    settings: Settings,
    store: Store,
    /// One limiter shared by every download, so a speed cap bounds their
    /// combined rate rather than each one independently.
    limiter: Arc<Limiter>,
    /// Downloads handed over by a browser, drained on the next tick.
    inbox: Inbox,
    /// Capture rules published to a connecting extension.
    advert: Arc<Mutex<Advert>>,
    /// Whether the local endpoint came up; shown in the status bar so a
    /// failure here is visible rather than silent.
    capture_ready: bool,
    confirm_delete: Option<ConfirmDelete>,
    /// The file-type warning, while it is open.
    type_prompt: Option<TypePrompt>,
    batch: Option<Batch>,
    verify: Option<Verify>,
    toasts: Vec<Toast>,
    next_toast: u64,
    /// The last link the clipboard watcher saw, so one copy offers once.
    last_clipboard: Option<String>,
    /// Combined speed per tick, newest last.
    spark: VecDeque<f64>,
    /// Animation clock.
    now: Instant,
    epoch: Instant,
    theme_anim: Animation<bool>,
    overlay_anim: Animation<bool>,
    /// Whether the main window has keyboard focus; drives the taskbar flash.
    focused: bool,
    /// Names a rename has claimed but not yet moved to, so a new download
    /// does not pick the same path while the rename is pending.
    pending_names: BTreeSet<PathBuf>,
    /// The window's last known size and position, saved on exit.
    window_geometry: Option<WindowGeometry>,
}

/// Sends `tracing` output to `<app dir>/logs/tdm.log`, appended. Filter comes
/// from `TDM_LOG` (default `info`). Falls back to stderr if the file cannot be
/// opened. A second init is ignored rather than panicking.
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("TDM_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let logs = tdm::store::app_dir().join("logs");
    let file = std::fs::create_dir_all(&logs).and_then(|()| {
        std::fs::OpenOptions::new().create(true).append(true).open(logs.join("tdm.log"))
    });
    let sink: Box<dyn std::io::Write + Send> = match file {
        Ok(file) => Box::new(file),
        Err(_) => Box::new(std::io::stderr()),
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(Mutex::new(sink))
        .try_init();
}

fn main() -> iced::Result {
    init_logging();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
        println!(
            "tdm                          run the download manager\n\
             tdm --register-host [ids]    let browser extensions talk to it\n\
             tdm --unregister-host        undo --register-host"
        );
        return Ok(());
    }
    // Native-host mode, registration, and handing a second launch over to
    // the running window all end here without opening one.
    if let Some(code) = tdm::ipc::run_cli(&args) {
        std::process::exit(code);
    }

    // Settings are read again in `boot`, which reports any problem; here a
    // failure just means the default window.
    let saved = Store::default_location().load_settings().ok().and_then(|s| s.window);
    let (size, position) = startup_window(saved);

    iced::application(App::boot, App::update, App::view)
        .title("Turbo Download Manager")
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window::Settings {
            size,
            position,
            min_size: Some(iced::Size::new(560.0, 320.0)),
            icon: window::icon::from_rgba(assets::app_icon(64).to_vec(), 64, 64).ok(),
            ..Default::default()
        })
        .run()
}

/// Cached SVG handles: a handle hashes its bytes on creation, which is
/// wasted work to repeat for every icon on every frame.
fn handle(icon: Icon) -> svg::Handle {
    static CACHE: OnceLock<Vec<svg::Handle>> = OnceLock::new();
    CACHE.get_or_init(|| Icon::ALL.iter().map(|i| svg::Handle::from_memory(i.svg())).collect())
        [icon as usize]
        .clone()
}

/// An icon tinted `colour`. The tint's alpha goes through `opacity`, since
/// the SVG colour filter only takes the hue.
fn glyph<'a>(icon: Icon, size: f32, colour: Color) -> svg::Svg<'a> {
    svg(handle(icon))
        .width(size)
        .height(size)
        .opacity(colour.a)
        .style(move |_, _| svg::Style { color: Some(Color { a: 1.0, ..colour }) })
}

fn space(w: Length, h: Length) -> Space {
    Space::new().width(w).height(h)
}

/// `dest`, else `name (2).ext`, `name (3).ext`, … — the first one not `taken`.
fn unique_dest(dest: PathBuf, taken: impl Fn(&std::path::Path) -> bool) -> PathBuf {
    if !taken(&dest) {
        return dest;
    }
    (2u64..)
        .map(|n| numbered(&dest, n))
        .find(|p| !taken(p))
        .expect("some number is free")
}

/// `dest` with ` (n)` before its extension: `photo.png` gives `photo (2).png`.
fn numbered(dest: &std::path::Path, n: u64) -> PathBuf {
    let stem = dest.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = dest.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    dest.with_file_name(format!("{stem} ({n}){ext}"))
}

/// Whether a destination is already in use on disk, by a state sidecar, or by
/// one of the downloads in `others` (the list's own paths and pending renames).
fn is_taken(p: &std::path::Path, others: &[PathBuf]) -> bool {
    let mut sidecar = p.as_os_str().to_owned();
    sidecar.push(".tdmstate");
    p.exists() || std::path::Path::new(&sidecar).exists() || others.iter().any(|o| same_path(o, p))
}

/// Whether two destinations name the same file. Windows file names are
/// case-insensitive, so `A.zip` and `a.zip` must count as one.
fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    if cfg!(windows) {
        a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// The last path component as text.
fn file_name_of(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Rename a finished file to `to`, claiming the name first. `std::fs::rename`
/// replaces an existing target on Windows, so an empty placeholder is created
/// with `create_new` (which fails if anything already has the name) and the
/// move then replaces only that placeholder. A sharing violation (a scanner or
/// indexer holding the file for a moment) is retried a few times.
fn rename_finished(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<PathBuf> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(to)?;
    let mut retries = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(to.to_path_buf()),
            Err(e) if cfg!(windows) && e.raw_os_error() == Some(32) && retries < 3 => {
                retries += 1;
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(e) => {
                let _ = std::fs::remove_file(to);
                return Err(e);
            }
        }
    }
}

/// Rename `from` to `want`, or to the first name after it that is free, trying
/// at most 100 names. Runs off the UI thread, since every check touches disk.
fn rename_to_free_name(from: &std::path::Path, want: &std::path::Path, others: &[PathBuf]) -> Result<PathBuf, String> {
    let names = std::iter::once(want.to_path_buf()).chain((2u64..).map(|n| numbered(want, n)));
    for to in names.take(100) {
        if is_taken(&to, others) {
            continue;
        }
        match rename_finished(from, &to) {
            Ok(done) => return Ok(done),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(format!("no free name near {}", file_name_of(want)))
}

/// Copy a state file that failed to parse aside, before the next save
/// replaces it with this session's defaults.
fn keep_aside(path: &std::path::Path) {
    let mut bad = path.as_os_str().to_owned();
    bad.push(".corrupt");
    if let Err(e) = std::fs::copy(path, &bad) {
        tracing::warn!("could not keep a copy of {}: {e}", path.display());
    }
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let store = Store::default_location();
        // A corrupt settings file must not silently reset the user's
        // categories, so report it and fall back for this run only.
        let settings = match store.load_settings() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("settings could not be loaded ({e}); using defaults for this session");
                // Every exit saves settings, so keep the unreadable file now.
                keep_aside(&store.settings_path());
                Settings::default()
            }
        };
        let limiter = Arc::new(Limiter::new(settings.speed_limit));
        let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
        let advert = Arc::new(Mutex::new(Advert {
            capture_extensions: settings.capture_list(),
            exception_hosts: settings.exception_hosts.clone(),
            enabled: true,
        }));
        // If this fails the application still works; only browser capture is
        // unavailable, and the status bar says so.
        let capture_ready = match tdm::ipc::serve(Arc::clone(&inbox), Arc::clone(&advert)) {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!("browser capture unavailable: {e}");
                false
            }
        };
        let now = Instant::now();
        let mut app = Self {
            url: String::new(),
            search: String::new(),
            downloads: Vec::new(),
            next_id: 1,
            selected: None,
            marked: BTreeSet::new(),
            select_mode: false,
            select_anim: Animation::new(false).duration(SELECT_FADE).easing(Easing::EaseOut),
            bar_anim: Animation::new(false).duration(SELECT_FADE).easing(Easing::EaseOut),
            ctrl_down: false,
            bulk_delete_files: false,
            renaming: BTreeSet::new(),
            mark_flash: None,
            row_menu: None,
            filter: Filter::All,
            browsers: tdm::register::Browser::detected(),
            browser_guide: false,
            grab: None,
            prompts: VecDeque::new(),
            prompt_name: None,
            prompt_dir: None,
            prompt_seq: 0,
            queue_busy: false,
            window_width: 900.0,
            open_menu: None,
            settings_open: false,
            theme_anim: Animation::new(settings.light_theme)
                .duration(Duration::from_millis(350))
                .easing(Easing::EaseInOut),
            overlay_anim: Animation::new(true),
            store,
            limiter,
            inbox,
            advert,
            capture_ready,
            confirm_delete: None,
            type_prompt: None,
            batch: None,
            verify: None,
            toasts: Vec::new(),
            next_toast: 1,
            last_clipboard: None,
            spark: VecDeque::with_capacity(SPARK_LEN),
            now,
            epoch: now,
            focused: true,
            pending_names: BTreeSet::new(),
            window_geometry: settings.window,
            settings,
        };
        match app.store.load_list() {
            Ok(list) => {
                for rec in list.downloads {
                    app.push(rec, Status::Paused);
                }
            }
            // The next save would otherwise replace the whole list with an empty one.
            Err(e) => {
                tracing::warn!("the download list could not be loaded ({e})");
                keep_aside(&app.store.list_path());
            }
        }
        // Idempotent, and slow enough (registry, files) to keep off the UI
        // thread. Failure only means the browser cannot reach us yet.
        std::thread::spawn(|| {
            if let Err(e) = tdm::register::install_default() {
                tracing::warn!("browser registration failed: {e}");
            }
        });
        (app, Task::none())
    }

    fn push(&mut self, rec: DownloadRecord, status: Status) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let name = rec
            .dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "download".to_owned());
        let mut handle = Download::new(rec.url, rec.dest, self.settings.max_segments);
        for (k, v) in &rec.headers {
            handle.set_header(k.clone(), v.clone());
        }
        // A User-Agent the browser gave us for this download wins; the
        // Preferences value is the fallback for everything else.
        let has_ua = rec.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("user-agent"));
        if !has_ua && !self.settings.downloads.user_agent.trim().is_empty() {
            handle.set_header("User-Agent", self.settings.downloads.user_agent.clone());
        }
        handle.set_limiter(Arc::clone(&self.limiter));
        handle.set_proxy(self.settings.proxy.resolved());
        handle.set_limits(self.settings.connection.retries, self.settings.connection.timeout_secs);
        handle.set_mirrors(rec.mirrors.clone());
        let mut snapshot = handle.snapshot();
        snapshot.status = if rec.complete { Status::Finished } else { status };
        snapshot.total = rec.total;
        snapshot.kind = rec.kind.as_deref().and_then(Kind::from_id);
        snapshot.content_type = rec.content_type.clone();
        self.downloads.push(Entry {
            id,
            name,
            handle,
            snapshot,
            category_id: rec.category_id,
            headers: rec.headers,
            started: false,
            paused_by_schedule: false,
            last_done: 0,
            last_tick: Instant::now(),
            speed: 0.0,
            scan: None,
            scanning: false,
            retries: 0,
            retry_at: None,
        });
        id
    }

    /// Add a link typed, pasted, expanded from a batch or offered by a toast.
    fn add_link(&mut self, url: String) {
        self.add_link_mirrored(url, Vec::new());
    }

    /// Queue `url` with optional mirror URLs for the same file.
    fn add_link_mirrored(&mut self, url: String, mirrors: Vec<String>) {
        // A manifest URL saved under its own name would leave the user with a
        // .m3u8 text file instead of a video.
        let name = tdm::media::container_name(&tdm::native_host::filename_from_url(&url));
        let dest = self.free_dest(self.settings.destination_for(&name));
        let category_id = self.settings.category_for(&name).map(|c| c.id);
        let status = self.added_status();
        self.push(
            DownloadRecord {
                url,
                dest,
                category_id,
                headers: Vec::new(),
                mirrors,
                total: None,
                complete: false,
                kind: None,
                content_type: None,
            },
            status,
        );
    }

    /// `dest`, or a numbered sibling when a file, a resume sidecar or another
    /// download already claims it. Writing into a claimed path would
    /// overwrite the user's file, or have two downloads share one.
    /// Queue one file from a grabbed site, kept under a folder named after the host.
    fn add_site_file(&mut self, url: &str) {
        let Ok(parsed) = reqwest::Url::parse(url) else { return };
        let mut dest = self.settings.download_dir.join(parsed.host_str().unwrap_or("site"));
        let mut segments: Vec<String> = parsed
            .path_segments()
            .map(|s| s.filter(|x| !x.is_empty()).map(str::to_owned).collect())
            .unwrap_or_default();
        if segments.is_empty() || parsed.path().ends_with('/') {
            segments.push("index.html".to_owned());
        }
        for segment in segments {
            dest.push(tdm::native_host::sanitize_filename(&segment));
        }
        let dest = self.free_dest(dest);
        let record = DownloadRecord {
            url: url.to_owned(),
            dest,
            category_id: None,
            headers: Vec::new(),
            mirrors: Vec::new(),
            total: None,
            complete: false,
            kind: None,
            content_type: None,
        };
        self.push(record, Status::Queued);
    }

    /// Every path the list owns or a rename has claimed.
    fn taken_names(&self) -> Vec<PathBuf> {
        self.downloads
            .iter()
            .map(|e| e.handle.dest().to_owned())
            .chain(self.pending_names.iter().cloned())
            .collect()
    }

    fn free_dest(&self, dest: PathBuf) -> PathBuf {
        let others = self.taken_names();
        unique_dest(dest, |p| is_taken(p, &others))
    }

    fn save(&self) {
        let list = DownloadList {
            downloads: self
                .downloads
                .iter()
                .map(|e| DownloadRecord {
                    url: e.handle.url().to_owned(),
                    dest: e.handle.dest().to_owned(),
                    category_id: e.category_id,
                    headers: e.headers.clone(),
                    mirrors: e.handle.mirrors().to_vec(),
                    total: e.snapshot.total,
                    complete: e.snapshot.status == Status::Finished,
                    kind: e.snapshot.kind.map(|k| k.id().to_owned()),
                    content_type: e.snapshot.content_type.clone(),
                })
                .collect(),
        };
        if let Err(e) = self.store.save_list(&list) {
            tracing::error!("could not save the download list: {e}");
        }
    }

    fn save_settings(&self) {
        if let Err(e) = self.store.save_settings(&self.settings) {
            tracing::error!("could not save settings: {e}");
        }
    }

    /// What a newly added download starts as. "Only add to the queue" means
    /// exactly that: it lands in the list and waits for the user.
    fn added_status(&self) -> Status {
        if self.settings.downloads.queue_only { Status::Paused } else { Status::Queued }
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.downloads.iter().position(|e| e.id == id)
    }

    fn entry(&self, id: u64) -> Option<&Entry> {
        self.downloads.iter().find(|e| e.id == id)
    }

    fn visible(&self) -> Vec<&Entry> {
        let needle = self.search.trim().to_lowercase();
        self.downloads
            .iter()
            .filter(|e| match self.filter {
                Filter::All => true,
                Filter::Category(id) => e.category_id == Some(id),
                Filter::Unfinished => e.snapshot.status != Status::Finished,
                Filter::Finished => e.snapshot.status == Status::Finished,
            })
            .filter(|e| {
                needle.is_empty()
                    || e.name.to_lowercase().contains(&needle)
                    || e.handle.url().to_lowercase().contains(&needle)
            })
            .collect()
    }

    /// Whether the clock currently allows the queue to run.
    ///
    /// shortcut: the schedule gates every start, including one the user asked
    /// for by hand — outside the window nothing runs until the schedule is
    /// turned off. Revisit if a per-download override is wanted.
    fn schedule_blocks(&self) -> bool {
        use tdm::scheduler::{Action, due, local_now};
        due(&self.settings.schedule, local_now()) == Action::Stop
    }

    fn start_ready(&mut self) -> Task<Message> {
        if self.schedule_blocks() {
            return Task::none();
        }
        let statuses: Vec<Status> = self.downloads.iter().map(|e| e.snapshot.status).collect();
        let mut tasks = Vec::new();
        for index in next_to_start(&statuses, self.settings.max_active) {
            let entry = &mut self.downloads[index];
            entry.snapshot.status = Status::Running;
            entry.snapshot.error = None;
            entry.started = true;
            entry.last_tick = Instant::now();
            let (id, handle) = (entry.id, entry.handle.clone());
            tasks.push(Task::perform(
                async move {
                    handle.run().await.map_err(|e| Failure { transient: e.is_transient(), text: e.to_string() })
                },
                move |r| Message::Finished(id, r),
            ));
        }
        if !tasks.is_empty() {
            self.queue_busy = true;
        }
        Task::batch(tasks)
    }

    /// Compare a finished download's real type with its name. A web page or a
    /// mismatch gets a toast, and the type dialog opens unless one is already open.
    fn check_file_type(&mut self, ix: usize) {
        if !self.settings.downloads.check_file_type {
            return;
        }
        let e = &self.downloads[ix];
        let Some(kind) = e.snapshot.kind else { return };
        let (id, name) = (e.id, e.name.clone());
        let content_type = e.snapshot.content_type.clone();
        let verdict = filetype::verdict(kind, &name, content_type.as_deref(), false);
        match verdict {
            Verdict::Match | Verdict::Unchecked => return,
            Verdict::WebPage => {
                self.toast(ToastKind::Error, format!("{} is a web page, not the file you asked for", middle_ellipsis(&name, 48)));
            }
            Verdict::Mismatch => {
                let ext = std::path::Path::new(&name)
                    .extension()
                    .map(|x| x.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.toast(ToastKind::Warning, format!("{}: the content is {}, but the name says .{ext}", middle_ellipsis(&name, 48), kind.label()));
            }
        }
        if self.type_prompt.is_none() {
            self.type_prompt = Some(TypePrompt { id, kind, content_type, verdict });
            self.open_overlay();
        }
    }

    fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toast_with(kind, text.into(), None);
    }

    fn toast_with(&mut self, kind: ToastKind, text: String, offer: Option<String>) {
        // Four stacked toasts is already a wall; drop the oldest.
        if self.toasts.len() >= 4 {
            self.toasts.remove(0);
        }
        self.toasts.push(Toast { id: self.next_toast, kind, text, born: Instant::now(), offer });
        self.next_toast += 1;
    }

    fn open_overlay(&mut self) {
        self.open_menu = None;
        self.row_menu = None;
        self.overlay_anim = Animation::new(false)
            .duration(Duration::from_millis(200))
            .easing(Easing::EaseOut)
            .go(true, Instant::now());
    }

    /// The bar shows while anything is ticked or select mode is on.
    fn bar_shown(&self) -> bool {
        self.select_mode || !self.marked.is_empty()
    }

    /// Keep the bar's slide in step with the marks. The bulk tick resets once nothing is ticked.
    fn sync_bar(&mut self) {
        if self.marked.is_empty() {
            self.bulk_delete_files = false;
        }
        let shown = self.bar_shown();
        if self.bar_anim.value() != shown {
            self.bar_anim.go_mut(shown, self.now);
        }
    }

    /// Tick or untick one download. A fresh tick gets its settling animation.
    fn toggle_mark(&mut self, id: u64) {
        // `remove` says whether it was ticked; untick, or tick and settle.
        if !self.marked.remove(&id) {
            self.marked.insert(id);
            self.mark_flash = Some((id, self.now));
        }
    }

    /// Enter or leave select mode. Leaving clears the marks.
    fn set_select_mode(&mut self, on: bool) {
        self.select_mode = on;
        self.select_anim.go_mut(on, Instant::now());
        if !on {
            self.marked.clear();
            self.mark_flash = None;
        }
    }

    /// Tick every visible download, or untick them.
    fn mark_visible(&mut self, on: bool) {
        let ids: Vec<u64> = self.visible().iter().map(|e| e.id).collect();
        for id in ids {
            if on {
                self.marked.insert(id);
            } else {
                self.marked.remove(&id);
            }
        }
    }

    fn running_among(&self, ids: &[u64]) -> usize {
        ids.iter().filter(|id| self.entry(**id).is_some_and(|e| e.snapshot.status == Status::Running)).count()
    }

    /// Open the delete confirmation for these downloads, if any still exist.
    /// A download whose rename is in progress is left out until the rename lands.
    fn open_delete(&mut self, ids: Vec<u64>, also_delete_file: bool) -> Task<Message> {
        let ids: Vec<u64> = ids.into_iter().filter(|id| self.index_of(*id).is_some()).collect();
        let (renaming, ids): (Vec<u64>, Vec<u64>) = ids.into_iter().partition(|id| self.renaming.contains(id));
        if !renaming.is_empty() {
            self.toast(ToastKind::Warning, "Wait for the rename to finish");
        }
        if ids.is_empty() {
            return Task::none();
        }
        let running = self.running_among(&ids);
        self.confirm_delete = Some(ConfirmDelete { ids, also_delete_file, typed: String::new(), running });
        self.open_overlay();
        iced::widget::operation::focus(CONFIRM_INPUT)
    }

    fn any_overlay(&self) -> bool {
        self.confirm_delete.is_some()
            || self.type_prompt.is_some()
            || self.settings_open
            || self.batch.is_some()
            || self.verify.is_some()
            || self.browser_guide
            || !self.prompts.is_empty()
            || self.grab.is_some()
    }

    /// Something is mid-transition and needs every frame.
    fn animating(&self) -> bool {
        self.theme_anim.is_animating(self.now)
            || self.overlay_anim.is_animating(self.now)
            || self.select_anim.is_animating(self.now)
            || self.bar_anim.is_animating(self.now)
            || self.mark_flash.is_some_and(|(_, at)| self.now.saturating_duration_since(at) < MARK_FLASH)
            || self.toasts.iter().any(|t| {
                let age = self.now.saturating_duration_since(t.born);
                age < TOAST_ENTER || t.life().saturating_sub(age) < TOAST_FADE
            })
    }

    fn palette(&self) -> Palette {
        Palette::at(self.theme_anim.interpolate(0.0, 1.0, self.now))
    }

    /// Start the cross-fade if the setting and the animation disagree.
    fn sync_theme(&mut self) {
        if self.theme_anim.value() != self.settings.light_theme {
            self.theme_anim.go_mut(self.settings.light_theme, Instant::now());
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.apply(message);
        self.sync_bar();
        task
    }

    fn apply(&mut self, message: Message) -> Task<Message> {
        self.now = Instant::now();
        match message {
            Message::UrlChanged(url) => {
                self.url = url;
                Task::none()
            }
            Message::SearchChanged(s) => {
                self.search = s;
                // Marks made under one search do not carry into another, so a
                // hidden row can never be deleted by Delete N.
                self.marked.clear();
                self.mark_flash = None;
                Task::none()
            }
            Message::AddUrl => {
                let url = self.url.trim().to_owned();
                if url.is_empty() {
                    return Task::none();
                }
                self.add_link(url);
                self.url.clear();
                self.save();
                self.start_ready()
            }
            Message::Frame(at) => {
                self.now = at;
                self.toasts.retain(|t| at.saturating_duration_since(t.born) < t.life());
                Task::none()
            }
            Message::Tick => self.tick(),
            Message::MenuOpen(which) => {
                self.open_menu = which;
                Task::none()
            }
            Message::Command(cmd) => {
                self.open_menu = None;
                self.run_command(cmd)
            }
            Message::Select(id) => {
                self.selected = Some(id);
                if self.row_menu != Some(id) {
                    self.row_menu = None;
                }
                Task::none()
            }
            Message::RowMenu(which) => {
                self.row_menu = which;
                if which.is_some() {
                    self.selected = which;
                }
                Task::none()
            }
            Message::SelectFilter(f) => {
                self.filter = f;
                // Marks made under one view do not carry into another.
                self.marked.clear();
                Task::none()
            }
            Message::ToggleSelectMode(on) => {
                self.set_select_mode(on);
                Task::none()
            }
            Message::Mark(id) => {
                if self.index_of(id).is_none() {
                    return Task::none();
                }
                if !self.select_mode {
                    self.set_select_mode(true);
                }
                self.toggle_mark(id);
                self.selected = Some(id);
                Task::none()
            }
            Message::RowPress(id) => match row_press(self.select_mode, self.ctrl_down) {
                RowAction::Mark => self.update(Message::Mark(id)),
                RowAction::Select => self.update(Message::Select(id)),
                RowAction::Toggle => {
                    if self.index_of(id).is_none() {
                        return Task::none();
                    }
                    // Ctrl+click ticks the row and leaves select mode alone.
                    self.toggle_mark(id);
                    self.selected = Some(id);
                    Task::none()
                }
            },
            Message::BulkDeleteFiles(on) => {
                self.bulk_delete_files = on;
                Task::none()
            }
            Message::MarkAllVisible(on) => {
                self.mark_visible(on);
                Task::none()
            }
            Message::ClearMarks => {
                self.marked.clear();
                self.mark_flash = None;
                Task::none()
            }
            Message::DeleteMarked => {
                let ids: Vec<u64> = self.marked.iter().copied().collect();
                let files = self.bulk_delete_files;
                self.open_delete(ids, files)
            }
            Message::TogglePause(id) => {
                let Some(ix) = self.index_of(id) else { return Task::none() };
                match self.downloads[ix].snapshot.status {
                    Status::Running => {
                        self.downloads[ix].handle.pause();
                        Task::none()
                    }
                    Status::Paused | Status::Failed => {
                        // A user resume starts the retry budget again.
                        let e = &mut self.downloads[ix];
                        e.snapshot.status = Status::Queued;
                        e.snapshot.error = None;
                        e.retries = 0;
                        e.retry_at = None;
                        self.start_ready()
                    }
                    _ => Task::none(),
                }
            }
            Message::Remove(id) => {
                // Deleting bytes off the user's disk is not something to do on
                // a single click, and the file stays unless the user opts in.
                self.open_delete(vec![id], false)
            }
            Message::ConfirmDeleteToggleFile(on) => {
                if let Some(c) = &mut self.confirm_delete {
                    c.also_delete_file = on;
                }
                Task::none()
            }
            Message::ConfirmTyped(text) => {
                if let Some(c) = &mut self.confirm_delete {
                    c.typed = text;
                }
                Task::none()
            }
            Message::CancelDelete => {
                self.confirm_delete = None;
                Task::none()
            }
            Message::ConfirmDelete => {
                let Some(confirm) = self.confirm_delete.take() else { return Task::none() };
                let n = confirm.ids.len();
                // The disabled button is not the only gate: a typed tier must match here too.
                if confirm_tier(n, confirm.running, confirm.also_delete_file) == Tier::Typed
                    && !typed_ok(n, &confirm.typed)
                {
                    self.confirm_delete = Some(confirm);
                    return Task::none();
                }
                // A rename that started after the dialog opened still holds its file.
                let gone: BTreeSet<u64> = confirm
                    .ids
                    .iter()
                    .copied()
                    .filter(|id| self.index_of(*id).is_some() && !self.renaming.contains(id))
                    .collect();
                if gone.is_empty() {
                    return Task::none();
                }
                // One pass over the list, however many are removed.
                let (removed, kept): (Vec<Entry>, Vec<Entry>) =
                    std::mem::take(&mut self.downloads).into_iter().partition(|e| gone.contains(&e.id));
                self.downloads = kept;
                let handles: Vec<(u64, Download)> = removed
                    .into_iter()
                    .map(|e| {
                        e.handle.pause();
                        (e.id, e.handle)
                    })
                    .collect();
                if self.selected.is_some_and(|id| gone.contains(&id)) {
                    self.selected = None;
                }
                if self.row_menu.is_some_and(|id| gone.contains(&id)) {
                    self.row_menu = None;
                }
                self.set_select_mode(false);
                self.bulk_delete_files = false;
                self.save();
                // A removed download may have been holding a slot in the queue.
                let start = self.start_ready();
                if !confirm.also_delete_file {
                    return start;
                }
                // Not `discard_partial`: that races the download's own worker,
                // which still holds the file open. `stop_and_discard` waits for
                // it to let go first. All run at once, and report as one batch.
                let discard = Task::perform(
                    async move {
                        let jobs = handles.into_iter().map(|(id, h)| async move {
                            h.stop_and_discard().await.err().map(|e| (id, e.to_string()))
                        });
                        futures_util::future::join_all(jobs)
                            .await
                            .into_iter()
                            .flatten()
                            .collect::<Vec<(u64, String)>>()
                    },
                    Message::DiscardedMany,
                );
                Task::batch(vec![start, discard])
            }
            Message::DiscardedMany(failures) => {
                for (id, e) in &failures {
                    tracing::warn!("could not delete the file of download {id}: {e}");
                }
                match failures.as_slice() {
                    [] => {}
                    [(_, e)] => self.toast(ToastKind::Error, format!("Could not delete the file: {}", short_error(e))),
                    many => self.toast(ToastKind::Error, format!("Could not delete {} files", many.len())),
                }
                Task::none()
            }
            Message::OpenFile(id) => {
                if let Some(path) = self.entry(id).map(|e| e.handle.dest().to_owned()) {
                    self.open_path(path);
                }
                Task::none()
            }
            Message::ShowInFolder(id) => {
                let dir = self.entry(id).and_then(|e| e.handle.dest().parent().map(PathBuf::from));
                if let Some(dir) = dir {
                    self.open_path(dir);
                }
                Task::none()
            }
            Message::CopyLink(id) => match self.entry(id).map(|e| e.handle.url().to_owned()) {
                Some(url) => {
                    // Not an offer to download what we just copied ourselves.
                    self.last_clipboard = Some(url.clone());
                    self.toast(ToastKind::Info, "Link copied");
                    iced::clipboard::write(url)
                }
                None => Task::none(),
            },
            Message::Verify(id) => {
                let Some(e) = self.entry(id) else { return Task::none() };
                let (path, name) = (e.handle.dest().to_owned(), e.name.clone());
                self.verify = Some(Verify { id, name, expected: String::new(), digest: None });
                self.open_overlay();
                // Hashing a multi-gigabyte file takes seconds; it must not
                // run on the thread that draws the window.
                Task::perform(
                    async move {
                        match tokio::task::spawn_blocking(move || tdm::checksum::digests_file(&path)).await {
                            Ok(Ok(d)) => Ok(d),
                            Ok(Err(e)) => Err(e.to_string()),
                            Err(e) => Err(e.to_string()),
                        }
                    },
                    move |r| Message::VerifyDone(id, r),
                )
            }
            Message::ReleaseLevel(id) => window::set_level(id, window::Level::Normal),
            Message::Resized(size) => {
                self.window_width = size.width;
                // A minimised window reports a tiny or empty size; keep the last real one.
                if size.width >= 100.0 && size.height >= 100.0 {
                    let g = self.window_geometry.get_or_insert_with(default_geometry);
                    g.width = size.width;
                    g.height = size.height;
                }
                Task::none()
            }
            Message::WindowMoved(point) => {
                // Windows reports minimised windows far off screen; ignore those.
                if on_screen(point.x) && on_screen(point.y) {
                    let g = self.window_geometry.get_or_insert_with(default_geometry);
                    g.x = Some(point.x);
                    g.y = Some(point.y);
                }
                Task::none()
            }
            Message::WindowFocus(focused) => {
                self.focused = focused;
                // Ctrl may be released while the window is away, with no event to say so.
                self.ctrl_down = false;
                Task::none()
            }
            Message::CopyBuildCommand => iced::clipboard::write(build_command()),
            Message::ChangeDownloadFolder => {
                let start = self.settings.download_dir.clone();
                // The native dialog runs off the UI thread, so the window
                // keeps drawing while it is open.
                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_title("Choose the default download folder")
                            .set_directory(&start)
                            .pick_folder()
                            .await
                            .map(|folder| folder.path().to_path_buf())
                    },
                    Message::DownloadFolderPicked,
                )
            }
            Message::DownloadFolderPicked(Some(dir)) => {
                self.toast(ToastKind::Success, format!("New downloads go to {}", middle_ellipsis(&dir.display().to_string(), 48)));
                self.settings.download_dir = dir;
                self.save_settings();
                Task::none()
            }
            Message::DownloadFolderPicked(None) => Task::none(),
            Message::VerifyExpected(s) => {
                if let Some(v) = &mut self.verify {
                    v.expected = s;
                }
                Task::none()
            }
            Message::VerifyDone(id, r) => {
                // Ignore a result for a dialog the user already closed.
                if let Some(v) = self.verify.as_mut().filter(|v| v.id == id) {
                    v.digest = Some(r);
                }
                Task::none()
            }
            Message::CloseVerify => {
                self.verify = None;
                Task::none()
            }
            Message::BatchEdit(action) => {
                if let Some(b) = &mut self.batch {
                    let edited = action.is_edit();
                    b.content.perform(action);
                    if edited {
                        b.links = batch::expand_all(&b.content.text());
                    }
                }
                Task::none()
            }
            Message::BatchMirrors(action) => {
                if let Some(b) = &mut self.batch {
                    b.mirrors.perform(action);
                }
                Task::none()
            }
            Message::BatchAdd => {
                let Some(Batch { links: Ok(links), mirrors, .. }) = self.batch.take() else { return Task::none() };
                let n = links.len();
                let mirrors = parse_mirrors(&mirrors.text());
                for url in links {
                    self.add_link_mirrored(url, mirrors.clone());
                }
                self.save();
                self.toast(ToastKind::Success, format!("Added {n} download{}", plural(n)));
                self.start_ready()
            }
            Message::BatchClose => {
                self.batch = None;
                Task::none()
            }
            Message::SettingsOpen(open) => {
                self.settings_open = open;
                if open {
                    self.open_overlay();
                } else {
                    self.save_settings();
                }
                Task::none()
            }
            Message::Shutdown => self.shutdown(),
            Message::Options(tdm::ui::options::Message::BrowseDownloadDir) => {
                self.update(Message::ChangeDownloadFolder)
            }
            Message::Options(m) => {
                let was_autostart = self.settings.general.launch_on_startup;
                tdm::ui::options::update(&mut self.settings, m);
                // reg.exe runs here, so the change happens off the UI thread.
                let autostart = if self.settings.general.launch_on_startup != was_autostart {
                    let on = self.settings.general.launch_on_startup;
                    Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || tdm::register::set_launch_at_login(on).map_err(|e| e.to_string()))
                                .await
                                .unwrap_or_else(|e| Err(e.to_string()))
                        },
                        Message::LaunchAtLoginDone,
                    )
                } else {
                    Task::none()
                };
                self.sync_theme();
                // Settings the running engine can honour immediately.
                self.apply_speed_cap();
                // Turning automatic retries off cancels any retry already waiting.
                if !self.settings.connection.auto_retry {
                    for e in &mut self.downloads {
                        e.retry_at = None;
                    }
                }
                // The proxy is read when a download builds its HTTP client, so
                // this reaches anything not already running.
                let proxy = self.settings.proxy.resolved();
                let (retries, timeout) =
                    (self.settings.connection.retries, self.settings.connection.timeout_secs);
                for e in &mut self.downloads {
                    e.handle.set_proxy(proxy.clone());
                    e.handle.set_limits(retries, timeout);
                }
                if let Ok(mut advert) = self.advert.lock() {
                    advert.capture_extensions = self.settings.capture_list();
                    advert.exception_hosts = self.settings.exception_hosts.clone();
                }
                // A change to the concurrency limit should take effect now.
                Task::batch([autostart, self.start_ready()])
            }
            Message::LaunchAtLoginDone(Err(e)) => {
                tracing::warn!("could not change launch at login: {e}");
                self.toast(ToastKind::Error, format!("Could not change launch at login: {e}"));
                Task::none()
            }
            Message::LaunchAtLoginDone(Ok(())) => Task::none(),
            Message::ToggleCapture(on) => {
                // Poisoned by a panicked IPC thread is not a reason to kill
                // the UI; the flag is a plain bool.
                match self.advert.lock() {
                    Ok(mut a) => a.enabled = on,
                    Err(poisoned) => poisoned.into_inner().enabled = on,
                }
                Task::none()
            }
            Message::Finished(id, result) => {
                let mut scans = Vec::new();
                let mut flash = Task::none();
                if let Some(ix) = self.index_of(id) {
                    let conn = &self.settings.connection;
                    let (auto_retry, max_retries) = (conn.auto_retry, conn.auto_retry_max);
                    let e = &mut self.downloads[ix];
                    e.snapshot = e.handle.snapshot();
                    e.speed = 0.0;
                    let mut retrying = false;
                    match result {
                        Ok(()) => {
                            e.retries = 0;
                            e.retry_at = None;
                        }
                        Err(failure) => {
                            e.snapshot.error = Some(failure.text.clone());
                            let wait = retry_wait(failure.transient, auto_retry, max_retries, e.retries)
                                .filter(|_| e.snapshot.status == Status::Failed);
                            if let Some(wait) = wait {
                                e.retries += 1;
                                e.retry_at = Some(Instant::now() + wait);
                                e.snapshot.error = Some(format!("Retrying in {} s: {}", wait.as_secs(), failure.text));
                                retrying = true;
                            }
                        }
                    }
                    let (status, name, error) = (e.snapshot.status, e.name.clone(), e.snapshot.error.clone());
                    match (status, error) {
                        (Status::Finished, _) if self.settings.downloads.show_complete_dialog => {
                            self.toast(ToastKind::Success, format!("Finished: {}", middle_ellipsis(&name, 48)));
                        }
                        (Status::Failed, Some(err)) if !retrying => {
                            self.toast(ToastKind::Error, format!("{}: {}", middle_ellipsis(&name, 48), short_error(&err)));
                        }
                        _ => {}
                    }
                    let finished = self.downloads[ix].snapshot.status == Status::Finished;
                    if finished && !self.focused && self.settings.downloads.flash_on_complete {
                        flash = window::latest().then(|id| match id {
                            Some(id) => window::request_user_attention(id, Some(window::UserAttention::Informational)),
                            None => Task::none(),
                        });
                    }
                    if finished && self.downloads[ix].scan.is_none() && !self.settings.scanner.is_empty() {
                        let scanner = self.settings.scanner.clone();
                        let file = self.downloads[ix].handle.dest().to_owned();
                        self.downloads[ix].scanning = true;
                        scans.push(Task::perform(
                            async move {
                                tokio::task::spawn_blocking(move || tdm::scan::scan(&scanner, &file))
                                    .await
                                    .unwrap_or_else(|e| tdm::scan::Verdict::Error(e.to_string()))
                            },
                            move |v| Message::Scanned(id, v),
                        ));
                    }
                    if finished {
                        self.check_file_type(ix);
                    }
                }
                self.save();
                Task::batch([Task::batch(scans), flash, self.start_ready()])
            }
            Message::Scanned(id, verdict) => {
                if let Some(ix) = self.index_of(id) {
                    if verdict == tdm::scan::Verdict::Threat {
                        let name = self.downloads[ix].name.clone();
                        self.toast(ToastKind::Error, format!("Threat found in {}. The file is kept: review it before opening.", middle_ellipsis(&name, 48)));
                    }
                    self.downloads[ix].scan = Some(verdict);
                    self.downloads[ix].scanning = false;
                }
                Task::none()
            }
            Message::SearchSubmit => Task::none(),
            Message::TypeRename(id) => {
                self.type_prompt = None;
                let Some(ix) = self.index_of(id) else { return Task::none() };
                let e = &self.downloads[ix];
                // Renaming under a running scan would race it on the same file.
                if e.scanning {
                    return Task::none();
                }
                let Some(kind) = e.snapshot.kind else { return Task::none() };
                let from = e.handle.dest().to_owned();
                let want = from.with_file_name(filetype::fixed_name(&e.name, kind));
                // Claim the name now, so no new download is given it meanwhile.
                let others = self.taken_names();
                self.pending_names.insert(want.clone());
                self.renaming.insert(id);
                let reserved = want.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || rename_to_free_name(&from, &want, &others))
                            .await
                            .unwrap_or_else(|err| Err(err.to_string()))
                    },
                    move |r| Message::Renamed(id, reserved, r),
                )
            }
            Message::Renamed(id, reserved, result) => {
                self.pending_names.remove(&reserved);
                self.renaming.remove(&id);
                match (self.index_of(id), result) {
                    (Some(ix), Ok(to)) => {
                        let name = file_name_of(&to);
                        let e = &mut self.downloads[ix];
                        e.handle.set_dest(to);
                        e.name = name.clone();
                        self.save();
                        self.toast(ToastKind::Success, format!("Renamed to {}", middle_ellipsis(&name, 48)));
                    }
                    (_, Err(err)) => self.toast(ToastKind::Error, format!("Could not rename: {}", short_error(&err))),
                    (None, Ok(_)) => {}
                }
                Task::none()
            }
            Message::TypeOpenPage(id) => {
                self.type_prompt = None;
                if let Some(url) = self.entry(id).map(|e| e.handle.url().to_owned()) {
                    if let Err(err) = open::that_detached(&url) {
                        self.toast(ToastKind::Error, format!("Could not open the page: {err}"));
                    }
                }
                Task::none()
            }
            Message::TypeKeep => {
                self.type_prompt = None;
                Task::none()
            }
            Message::Clipboard(Some(clip)) => {
                let link = clip.trim();
                let is_link = (link.starts_with("http://") || link.starts_with("https://"))
                    && link.len() < 4096
                    && !link.contains(char::is_whitespace);
                if is_link && self.last_clipboard.as_deref() != Some(link) {
                    self.last_clipboard = Some(link.to_owned());
                    if !self.downloads.iter().any(|e| e.handle.url() == link) {
                        let name = tdm::native_host::filename_from_url(link);
                        self.toast_with(ToastKind::Info, format!("Download copied link? {}", middle_ellipsis(&name, 48)), Some(link.to_owned()));
                    }
                }
                Task::none()
            }
            Message::Clipboard(None) => Task::none(),
            Message::PasteUrl(clip) => {
                if let Some(clip) = clip.map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()) {
                    // Several links in the clipboard go to the batch dialog, ready to edit.
                    let links = batch::links_in(&clip);
                    if links.len() >= 2 {
                        let text = links.join("\n");
                        self.batch = Some(Batch {
                            content: text_editor::Content::with_text(&text),
                            mirrors: text_editor::Content::new(),
                            links: batch::expand_all(&text),
                        });
                        self.open_overlay();
                        return Task::none();
                    }
                    self.url = clip;
                }
                iced::widget::operation::focus(URL_INPUT)
            }
            Message::CaptureStart => self.accept_capture(true),
            Message::CaptureQueue => self.accept_capture(false),
            Message::CaptureCancel => {
                self.pop_prompt();
                Task::none()
            }
            Message::CaptureName(stem) => {
                if !self.prompts.is_empty() {
                    self.prompt_name = Some(stem);
                }
                Task::none()
            }
            Message::CaptureFolder => {
                let Some(default) = self.prompts.front().map(capture_name) else { return Task::none() };
                let start = self.prompt_folder(&default);
                let seq = self.prompt_seq;
                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_title("Choose where to save this file")
                            .set_directory(&start)
                            .pick_folder()
                            .await
                            .map(|folder| folder.path().to_path_buf())
                    },
                    move |dir| Message::CaptureFolderPicked(seq, dir),
                )
            }
            Message::CaptureFolderPicked(seq, Some(dir)) => {
                if seq == self.prompt_seq && !self.prompts.is_empty() {
                    self.prompt_dir = Some(dir);
                }
                Task::none()
            }
            Message::CaptureFolderPicked(_, None) => Task::none(),
            Message::GrabOpen => {
                self.grab = Some(Grab { url: String::new(), working: false, error: None });
                Task::none()
            }
            Message::GrabUrl(url) => {
                if let Some(g) = &mut self.grab {
                    g.url = url;
                }
                Task::none()
            }
            Message::GrabStart => {
                let Some(g) = &mut self.grab else { return Task::none() };
                if g.working {
                    return Task::none();
                }
                match reqwest::Url::parse(g.url.trim()) {
                    Ok(url) if matches!(url.scheme(), "http" | "https") => {
                        g.working = true;
                        g.error = None;
                        Task::perform(
                            async move {
                                tdm::grabber::crawl(url)
                                    .await
                                    .map(|found| found.into_iter().map(|u| u.to_string()).collect())
                            },
                            Message::GrabDone,
                        )
                    }
                    _ => {
                        g.error = Some("Enter a full http:// or https:// address.".to_owned());
                        Task::none()
                    }
                }
            }
            Message::GrabDone(result) => match result {
                Ok(urls) => {
                    self.grab = None;
                    let n = urls.len();
                    for url in &urls {
                        self.add_site_file(url);
                    }
                    self.save();
                    self.toast(ToastKind::Success, format!("Grabbed {n} file{}", plural(n)));
                    self.start_ready()
                }
                Err(e) => {
                    if let Some(g) = &mut self.grab {
                        g.working = false;
                        g.error = Some(e);
                    }
                    Task::none()
                }
            },
            Message::GrabClose => {
                self.grab = None;
                Task::none()
            }
            Message::OpenBrowserGuide => {
                self.browsers = tdm::register::Browser::detected();
                self.browser_guide = true;
                Task::none()
            }
            Message::CloseBrowserGuide => {
                self.browser_guide = false;
                Task::none()
            }
            Message::CopyExtensionPath(browser) => {
                iced::clipboard::write(browser.extension_dir().display().to_string())
            }
            Message::OpenExtensionFolder(browser) => {
                let dir = browser.extension_dir();
                if let Err(e) = open::that_detached(&dir) {
                    self.toast(ToastKind::Error, format!("Could not open {}: {e}", middle_ellipsis(&dir.display().to_string(), 48)));
                }
                Task::none()
            }
            Message::AddToBrowser(browser) => {
                let dir = browser.extension_dir();
                if !dir.join("manifest.json").is_file() {
                    self.toast(ToastKind::Error, "TDM's extension files are missing from this install. Reinstall TDM.");
                    return Task::none();
                }
                // Registration runs reg.exe, so it happens off the UI thread;
                // `SetUpDone` carries on from here.
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(|| tdm::register::set_up().map_err(|e| e.to_string()))
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                    },
                    move |r| Message::SetUpDone(browser, r),
                )
            }
            Message::SetUpDone(_, Err(e)) => {
                self.toast(ToastKind::Error, format!("Could not register TDM: {e}"));
                Task::none()
            }
            Message::SetUpDone(browser, Ok(())) => {
                let dir = browser.extension_dir();
                // The path goes to the clipboard before the page opens, so Ctrl+V works at once.
                let copy = iced::clipboard::write(dir.display().to_string());
                match browser.open_extensions_page() {
                    Ok(()) => self.toast(
                        ToastKind::Success,
                        format!("Opened {}. The folder path is copied: press Ctrl+V in the folder window.", browser.label()),
                    ),
                    Err(e) => self.toast(ToastKind::Error, format!("Could not open {}: {e}", browser.label())),
                }
                copy
            }
            Message::ToastAction(id) => {
                let offer = self.toasts.iter().position(|t| t.id == id).and_then(|i| self.toasts.remove(i).offer);
                if let Some(url) = offer {
                    self.add_link(url);
                    self.save();
                    return self.start_ready();
                }
                Task::none()
            }
            Message::DismissToast(id) => {
                self.toasts.retain(|t| t.id != id);
                Task::none()
            }
            Message::Key(event) => self.key(event),
        }
    }

    /// Add the front browser download, started now or left paused in the queue.
    fn accept_capture(&mut self, start: bool) -> Task<Message> {
        let Some(default) = self.prompts.front().map(capture_name) else { return Task::none() };
        // Only an edited name or a picked folder changes the path; otherwise
        // the path is the one every other download gets.
        let custom = self.prompt_name.is_some() || self.prompt_dir.is_some();
        let dest = if custom {
            let folder = self.prompt_folder(&default);
            match capture_target(&default, self.prompt_name.as_deref(), &folder) {
                Ok(target) => self.free_dest(target),
                // The prompt stays open; the dialog shows the same reason.
                Err(reason) => {
                    self.toast(ToastKind::Error, reason);
                    return Task::none();
                }
            }
        } else {
            self.free_dest(self.settings.destination_for(&default))
        };
        let category_name = if custom { file_name_of(&dest) } else { default };
        let category_id = self.settings.category_for(&category_name).map(|c| c.id);
        let Some(item) = self.pop_prompt() else { return Task::none() };
        let rec = DownloadRecord {
            url: item.url,
            dest,
            category_id,
            headers: item.headers,
            mirrors: Vec::new(),
            total: item.size,
            complete: false,
            kind: None,
            content_type: None,
        };
        self.push(rec, if start { Status::Queued } else { Status::Paused });
        self.save();
        if start { self.start_ready() } else { Task::none() }
    }

    /// Remove the front browser download, and forget the edits made to its prompt.
    fn pop_prompt(&mut self) -> Option<Incoming> {
        let item = self.prompts.pop_front();
        self.prompt_name = None;
        self.prompt_dir = None;
        if item.is_some() {
            self.prompt_seq += 1;
        }
        item
    }

    /// The folder a prompt saves into: the picked one, else the default folder for its name.
    fn prompt_folder(&self, default: &str) -> PathBuf {
        self.prompt_dir.clone().unwrap_or_else(|| {
            self.settings
                .destination_for(default)
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| self.settings.download_dir.clone())
        })
    }

    /// Forget marks and dialog entries for downloads that have gone, and keep
    /// the dialog's running count current.
    fn prune_selection(&mut self) {
        let live: HashMap<u64, Status> = self.downloads.iter().map(|e| (e.id, e.snapshot.status)).collect();
        prune_marks(&mut self.marked, &live);
        let mut emptied = false;
        if let Some(c) = &mut self.confirm_delete {
            c.ids.retain(|id| live.contains_key(id));
            c.running = c.ids.iter().filter(|id| live.get(id) == Some(&Status::Running)).count();
            emptied = c.ids.is_empty();
        }
        if emptied {
            self.confirm_delete = None;
        }
        if self.type_prompt.as_ref().is_some_and(|t| !live.contains_key(&t.id)) {
            self.type_prompt = None;
        }
    }

    /// Give the shared limiter the cap in force now: the speed limit, or a
    /// time-of-day window's cap. `set` restarts the limiter's measurement, so
    /// it is only called when the cap actually changes.
    fn apply_speed_cap(&self) {
        let now = tdm::scheduler::local_now().1;
        let cap = tdm::model::effective_limit(self.settings.speed_limit, &self.settings.speed_windows, now);
        if self.limiter.get() != cap {
            self.limiter.set(cap);
        }
    }

    fn tick(&mut self) -> Task<Message> {
        self.prune_selection();
        let clock = Instant::now();
        self.toasts.retain(|t| clock.saturating_duration_since(t.born) < t.life());
        let mut tasks = Vec::new();
        // Pick up anything a browser handed over since the last tick. These
        // mutexes are shared with IPC worker threads. If one of those
        // panicked the lock is poisoned, and taking it with `expect` here
        // would kill the UI thread on the next tick — turning a background
        // failure into a dead application.
        let captured: Vec<_> = match self.inbox.lock() {
            Ok(mut queue) => queue.drain(..).collect(),
            Err(poisoned) => poisoned.into_inner().drain(..).collect(),
        };
        let had_capture = !captured.is_empty();
        self.prompts.extend(captured);
        // A second launch asks the running window to show itself.
        if (had_capture && self.settings.bring_to_front_on_capture) | tdm::ipc::take_show_request() {
            tasks.push(raise_window());
        }
        if self.settings.general.monitor_clipboard {
            tasks.push(iced::clipboard::read().map(Message::Clipboard));
        }

        let now = Instant::now();
        for entry in &mut self.downloads {
            // A pending retry only means something while the download is still
            // Failed; anything else means the user or a retry has moved it on.
            if entry.snapshot.status != Status::Failed {
                entry.retry_at = None;
            }
            // The retry is due: requeue it, and `start_ready` below starts it.
            if entry.retry_at.is_some_and(|at| now >= at) {
                entry.retry_at = None;
                entry.snapshot.status = Status::Queued;
                entry.snapshot.error = None;
            }
            // Only adopt the engine's snapshot once this entry has actually
            // been run in this session. A restored entry's handle has never
            // run, so its snapshot is the default — Queued — and copying that
            // over a restored Finished or Paused row would hand it straight to
            // `start_ready`, re-downloading a completed file from byte 0 over
            // itself.
            // A Queued entry was resumed and is waiting for a slot; the handle's
            // older state must not undo that. A download waiting to retry keeps
            // its "Retrying in" error text.
            if entry.started && entry.snapshot.status != Status::Queued && entry.retry_at.is_none() {
                entry.snapshot = entry.handle.snapshot();
            }
            let elapsed = now.duration_since(entry.last_tick).as_secs_f64();
            if entry.snapshot.status == Status::Running && elapsed > 0.0 {
                let delta = entry.snapshot.done.saturating_sub(entry.last_done);
                entry.speed = delta as f64 / elapsed;
            } else {
                entry.speed = 0.0;
            }
            entry.last_done = entry.snapshot.done;
            entry.last_tick = now;
        }
        if self.spark.len() == SPARK_LEN {
            self.spark.pop_front();
        }
        // The queue has run dry: nothing running and nothing waiting. Paused
        // downloads do not count, since the user stopped those on purpose.
        // A download waiting to retry still counts as work left in the queue.
        let retry_waiting = self.downloads.iter().any(|e| e.retry_at.is_some());
        if self.queue_busy && self.count(Status::Running) == 0 && self.count(Status::Queued) == 0 && !retry_waiting {
            self.queue_busy = false;
            tasks.push(self.after_queue());
        }
        // Apply the time-of-day cap on every tick, so the limit changes at a
        // window edge without waiting for the user to touch a setting.
        self.apply_speed_cap();
        self.spark.push_back(self.downloads.iter().map(|e| e.speed).sum());

        // Past the schedule's stop time, running downloads come to a halt —
        // that is what a stop time is for. Progress is saved, so the next
        // window resumes rather than restarts.
        if self.schedule_blocks() {
            for entry in &mut self.downloads {
                if entry.snapshot.status == Status::Running {
                    entry.handle.pause();
                    entry.paused_by_schedule = true;
                }
            }
            return Task::batch(tasks);
        }
        // Inside the window again: only the downloads the schedule stopped go
        // back in the queue. One the user paused stays paused.
        for entry in &mut self.downloads {
            if entry.paused_by_schedule && entry.snapshot.status == Status::Paused {
                entry.snapshot.status = Status::Queued;
                entry.snapshot.error = None;
                entry.paused_by_schedule = false;
            }
        }
        tasks.push(self.start_ready());
        Task::batch(tasks)
    }

    fn key(&mut self, event: keyboard::Event) -> Task<Message> {
        // A modifier change is not a key press. It only tells a row click whether Ctrl is held.
        if let keyboard::Event::ModifiersChanged(modifiers) = event {
            self.ctrl_down = modifiers.command();
            return Task::none();
        }
        let keyboard::Event::KeyPressed { key, modifiers, .. } = event else { return Task::none() };
        if let Key::Named(Named::Escape) = key {
            // The capture prompt is drawn above the other dialogs, except the
            // type warning and the grab dialog, which are drawn above it.
            if self.type_prompt.is_none() && self.grab.is_none() && !self.prompts.is_empty() {
                return self.update(Message::CaptureCancel);
            }
            // Close only the topmost thing; `||` stops at the first one taken.
            let closed = self.type_prompt.take().is_some()
                || self.confirm_delete.take().is_some()
                || self.open_menu.take().is_some()
                || self.row_menu.take().is_some()
                || self.verify.take().is_some()
                || self.batch.take().is_some();
            if !closed && self.grab.is_some() {
                return self.update(Message::GrabClose);
            }
            if !closed && self.browser_guide {
                return self.update(Message::CloseBrowserGuide);
            }
            if !closed && self.settings_open {
                return self.update(Message::SettingsOpen(false));
            }
            if !closed && self.select_mode {
                return self.update(Message::ToggleSelectMode(false));
            }
            return Task::none();
        }
        // Everything else acts on the list, which a dialog covers.
        if self.any_overlay() {
            return Task::none();
        }
        match key.as_ref() {
            Key::Character("n") if modifiers.command() => self.run_command(Cmd::AddUrl),
            Key::Character("f") if modifiers.command() => self.run_command(Cmd::Find),
            Key::Character("a") if modifiers.command() => self.run_command(Cmd::SelectMode),
            Key::Character("v") if modifiers.command() && modifiers.shift() => self.run_command(Cmd::PasteUrl),
            // Ctrl+P pauses everything while something runs, otherwise starts the queue.
            Key::Character("p") if modifiers.command() => {
                if self.count(Status::Running) > 0 {
                    self.run_command(Cmd::PauseAll)
                } else {
                    self.run_command(Cmd::StartQueue)
                }
            }
            Key::Character("r") if modifiers.command() => self.run_command(Cmd::RetryFailed),
            // With ticks made, Delete asks about all of them.
            // Delete acts on the marks only when the highlighted row is one of them
            // (or select mode is on). Otherwise it deletes just the highlighted row,
            // so a stray Delete cannot remove marked files the user is not looking at.
            Key::Named(Named::Delete)
                if !self.marked.is_empty()
                    && (self.select_mode || self.selected.is_some_and(|id| self.marked.contains(&id))) =>
            {
                self.update(Message::DeleteMarked)
            }
            Key::Named(Named::Delete) => self.run_command(Cmd::Delete),
            Key::Named(Named::F2) => self.run_command(Cmd::ShowInFolder),
            Key::Named(Named::ArrowDown) => self.move_selection(1),
            Key::Named(Named::ArrowUp) => self.move_selection(-1),
            Key::Named(Named::Home) => self.select_end(true),
            Key::Named(Named::End) => self.select_end(false),
            // In select mode Enter and Space tick the row. Otherwise Enter opens a
            // finished file, and for anything else both act like Space.
            Key::Named(Named::Space | Named::Enter) if self.select_mode => match self.selected {
                Some(id) => self.update(Message::Mark(id)),
                None => Task::none(),
            },
            Key::Named(Named::Enter) if self.selected_status() == Some(Status::Finished) => self.run_command(Cmd::OpenFile),
            Key::Named(Named::Space | Named::Enter) => match self.selected {
                Some(id) => self.update(Message::TogglePause(id)),
                None => Task::none(),
            },
            _ => Task::none(),
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        if let Err(e) = open::that_detached(&path) {
            self.toast(
                ToastKind::Error,
                format!("Could not open {}: {}", middle_ellipsis(&path.display().to_string(), 48), short_error(&e.to_string())),
            );
        }
    }

    /// Moves the keyboard cursor `step` rows through the visible list, stopping at the ends.
    fn move_selection(&mut self, step: isize) -> Task<Message> {
        let ids: Vec<u64> = self.visible().iter().map(|e| e.id).collect();
        let Some(last) = ids.len().checked_sub(1) else { return Task::none() };
        let at = self.selected.and_then(|id| ids.iter().position(|v| *v == id));
        let next = match at {
            Some(p) => (p as isize + step).clamp(0, last as isize) as usize,
            None => 0,
        };
        self.selected = Some(ids[next]);
        Task::none()
    }

    /// Selects the first (`first`) or last visible download.
    fn select_end(&mut self, first: bool) -> Task<Message> {
        let ids: Vec<u64> = self.visible().iter().map(|e| e.id).collect();
        self.selected = if first { ids.first().copied() } else { ids.last().copied() };
        Task::none()
    }

    fn selected_status(&self) -> Option<Status> {
        self.selected.and_then(|id| self.entry(id)).map(|e| e.snapshot.status)
    }

    fn run_command(&mut self, cmd: Cmd) -> Task<Message> {
        let sel = self.selected;
        match cmd {
            Cmd::AddUrl => iced::widget::operation::focus(URL_INPUT),
            Cmd::Find => iced::widget::operation::focus(SEARCH_INPUT),
            Cmd::PasteUrl => iced::clipboard::read().map(Message::PasteUrl),
            Cmd::BatchAdd => {
                self.batch = Some(Batch {
                    content: text_editor::Content::new(),
                    mirrors: text_editor::Content::new(),
                    links: Ok(Vec::new()),
                });
                self.open_overlay();
                Task::none()
            }
            Cmd::Exit => self.shutdown(),
            Cmd::PauseAll => {
                for e in &mut self.downloads {
                    e.handle.pause();
                    // A retry waiting in the wings is cancelled too.
                    e.retry_at = None;
                }
                Task::none()
            }
            Cmd::RetryFailed => {
                for e in self.downloads.iter_mut().filter(|e| e.snapshot.status == Status::Failed) {
                    e.snapshot.status = Status::Queued;
                    e.snapshot.error = None;
                    e.retries = 0;
                    e.retry_at = None;
                }
                self.start_ready()
            }
            Cmd::DeleteCompleted => {
                self.downloads.retain(|e| e.snapshot.status != Status::Finished);
                self.save();
                Task::none()
            }
            Cmd::ToggleSidebar => {
                self.settings.show_categories = !self.settings.show_categories;
                self.save_settings();
                Task::none()
            }
            Cmd::ToggleTheme => {
                self.settings.light_theme = !self.settings.light_theme;
                self.sync_theme();
                self.save_settings();
                Task::none()
            }
            Cmd::Pause => {
                if let Some(e) = sel.and_then(|id| self.entry(id)) {
                    e.handle.pause();
                }
                Task::none()
            }
            Cmd::Resume => match sel {
                Some(id) if self.selected_status() != Some(Status::Running) => {
                    self.update(Message::TogglePause(id))
                }
                _ => Task::none(),
            },
            Cmd::Delete => sel.map_or(Task::none(), |id| self.update(Message::Remove(id))),
            Cmd::SelectMode => {
                // Entering select mode from the menu or Ctrl+A selects what is in view.
                self.set_select_mode(true);
                self.mark_visible(true);
                Task::none()
            }
            Cmd::DeleteSelected => self.update(Message::DeleteMarked),
            Cmd::OpenFile => sel.map_or(Task::none(), |id| self.update(Message::OpenFile(id))),
            Cmd::ShowInFolder => sel.map_or(Task::none(), |id| self.update(Message::ShowInFolder(id))),
            Cmd::VerifySha256 => match sel {
                Some(id) if self.selected_status() == Some(Status::Finished) => {
                    self.update(Message::Verify(id))
                }
                _ => Task::none(),
            },
            Cmd::Options => self.update(Message::SettingsOpen(true)),
            Cmd::ChangeDownloadFolder => self.update(Message::ChangeDownloadFolder),
            Cmd::OpenDownloadFolder => {
                let dir = self.settings.download_dir.clone();
                if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| open::that_detached(&dir)) {
                    self.toast(ToastKind::Error, format!("Could not open {}: {e}", middle_ellipsis(&dir.display().to_string(), 48)));
                }
                Task::none()
            }
            Cmd::Scheduler => {
                // The queue schedule lives on the Downloads page.
                self.settings.options_tab = OptionsTab::Downloads;
                self.update(Message::SettingsOpen(true))
            }
            Cmd::StartQueue => {
                // Re-queue everything idle; `start_ready` respects max_active.
                for e in &mut self.downloads {
                    if matches!(e.snapshot.status, Status::Paused | Status::Failed) {
                        e.snapshot.status = Status::Queued;
                        e.snapshot.error = None;
                        e.retries = 0;
                        e.retry_at = None;
                    }
                }
                self.start_ready()
            }
            Cmd::StopQueue => {
                for e in &mut self.downloads {
                    e.retry_at = None;
                    if e.snapshot.status == Status::Queued {
                        e.snapshot.status = Status::Paused;
                    } else if e.snapshot.status == Status::Running {
                        e.handle.pause();
                    }
                }
                Task::none()
            }
            Cmd::About => {
                self.toast(ToastKind::Info, concat!("Turbo Download Manager ", env!("CARGO_PKG_VERSION")));
                Task::none()
            }
            Cmd::ImportList | Cmd::ExportList | Cmd::UserGuide | Cmd::CheckUpdates => Task::none(),
        }
    }

    /// Whether a command has something to act on right now.
    fn can(&self, cmd: Cmd) -> bool {
        let status = self.selected_status();
        let any = |s: Status| self.downloads.iter().any(|e| e.snapshot.status == s);
        cmd.implemented()
            && match cmd {
                Cmd::Resume => matches!(status, Some(Status::Paused | Status::Failed)),
                Cmd::Pause => status == Some(Status::Running),
                Cmd::Delete => status.is_some(),
                Cmd::DeleteSelected => !self.marked.is_empty(),
                Cmd::OpenFile | Cmd::VerifySha256 => status == Some(Status::Finished),
                Cmd::ShowInFolder => status.is_some(),
                Cmd::DeleteCompleted => any(Status::Finished),
                Cmd::StartQueue => any(Status::Paused) || any(Status::Failed) || any(Status::Queued),
                Cmd::StopQueue | Cmd::PauseAll => any(Status::Running) || any(Status::Queued),
                Cmd::RetryFailed => any(Status::Failed),
                _ => true,
            }
    }

    fn theme(&self) -> Theme {
        self.palette().theme()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            iced::time::every(TICK).map(|_| Message::Tick),
            // Closing the window must not drop in-flight progress.
            window::close_requests().map(|_| Message::Shutdown),
            keyboard::listen().map(Message::Key),
            iced::event::listen_with(window_event),
        ];
        if self.animating() {
            subs.push(window::frames().map(Message::Frame));
        } else if self.downloads.iter().any(|e| e.snapshot.status == Status::Running) {
            subs.push(iced::time::every(SHIMMER_TICK).map(Message::Frame));
        }
        Subscription::batch(subs)
    }

    /// Stop every worker and persist, then quit.
    ///
    /// Each engine checkpoints its own segments when it stops, so pausing is
    /// what makes the resume data current; quitting without it discards up to
    /// a tick's worth of transfer on every running download.
    /// Run the user's after-queue choice. Called once, when the queue goes idle.
    fn after_queue(&mut self) -> Task<Message> {
        match self.settings.after_queue {
            tdm::model::AfterQueue::DoNothing => Task::none(),
            tdm::model::AfterQueue::CloseApp => self.shutdown(),
            tdm::model::AfterQueue::PowerOff => {
                let started = power_off();
                if started {
                    // Quitting first is fine: the OS waits out the 60 seconds.
                    self.shutdown()
                } else {
                    // Do not quit if the shutdown command failed; the user
                    // would lose the window with nothing happening.
                    self.toast(ToastKind::Error, "Could not start the shutdown. The queue is done.");
                    Task::none()
                }
            }
        }
    }

    fn shutdown(&mut self) -> Task<Message> {
        for entry in &self.downloads {
            entry.handle.pause();
        }
        self.save();
        self.settings.window = self.window_geometry;
        self.save_settings();
        iced::exit()
    }

    // ------------------------------------------------------------------ view

    fn view(&self) -> Element<'_, Message> {
        let p = self.palette();
        let body = column![self.header(&p), self.main_area(&p), self.status_bar(&p)];
        let base = container(body).width(Length::Fill).height(Length::Fill).style(theme::chrome);

        let mut layers = stack![base].width(Length::Fill).height(Length::Fill);
        if let Some(i) = self.open_menu {
            layers = layers.push(self.menu_dropdown(i, &p));
        }
        // Each dialog: its content, what dismisses it, its maximum width, and
        // whether its content scrolls.
        let dialog = if self.type_prompt.is_some() {
            Some((self.type_dialog(&p), Message::TypeKeep, 560.0, true))
        } else if self.grab.is_some() {
            Some((self.grab_dialog(&p), Message::GrabClose, 560.0, true))
        } else if !self.prompts.is_empty() {
            Some((self.capture_dialog(&p), Message::CaptureCancel, 560.0, true))
        } else if self.verify.is_some() {
            Some((self.verify_dialog(&p), Message::CloseVerify, 560.0, true))
        } else if self.batch.is_some() {
            Some((self.batch_dialog(&p), Message::BatchClose, 560.0, true))
        } else if self.confirm_delete.is_some() {
            Some((self.delete_dialog(&p), Message::CancelDelete, 480.0, true))
        } else if self.settings_open {
            Some((self.options_dialog(), Message::SettingsOpen(false), 720.0, false))
        } else if self.browser_guide {
            Some((self.browser_dialog(&p), Message::CloseBrowserGuide, 560.0, true))
        } else {
            None
        };
        if let Some((content, dismiss, max_w, scroll)) = dialog {
            layers = layers.push(self.modal(content, dismiss, max_w, scroll));
        }
        if !self.toasts.is_empty() {
            layers = layers.push(self.toast_layer(&p));
        }
        layers.into()
    }

    fn header(&self, p: &Palette) -> Element<'_, Message> {
        let compact = self.window_width < 760.0;
        let mark = image(app_mark()).width(20).height(20);
        let mut bar = row![
            container(row![mark, text("TDM").size(14).font(bold())].spacing(6).align_y(Alignment::Center))
                .padding([0, 6]),
        ]
        .spacing(2)
        .align_y(Alignment::Center);
        for (i, menu) in MENU_BAR.iter().enumerate() {
            let open = self.open_menu == Some(i);
            bar = bar.push(
                button(text(menu.title).size(13))
                    .padding([5, 10])
                    .style(theme::flat(open))
                    .on_press(Message::MenuOpen(if open { None } else { Some(i) })),
            );
        }
        bar = bar.push(space(Length::Fill, Length::Shrink)).push(self.theme_toggle(p));

        let mut tools = row![].spacing(2).align_y(Alignment::Center);
        for t in assets::TOOLBAR {
            let on = self.can(t.cmd);
            let colour = if on { p.text } else { alpha(p.muted, 0.45) };
            let mut b = button(glyph(t.icon, 18.0, colour)).padding(7).style(theme::flat(false));
            if on {
                b = b.on_press(Message::Command(t.cmd));
            }
            tools = tools.push(tip(b, t.label));
        }
        let search = container(
            row![
                glyph(Icon::Search, 15.0, p.muted),
                text_input("Search downloads", &self.search)
                    .id(SEARCH_INPUT)
                    .on_input(Message::SearchChanged)
                    // Enter is captured here, so the list never sees it.
                    .on_submit(Message::SearchSubmit)
                    .style(theme::bare_input)
                    .size(13)
                    .padding([6, 4]),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([0, 10])
        .width(Length::Fixed(if compact { 180.0 } else { 260.0 }))
        .style(move |t: &Theme| container::Style {
            border: iced::Border { color: Palette::of(t).border, width: 1.0, radius: 99.0.into() },
            background: Some(Palette::of(t).card.into()),
            ..Default::default()
        });
        let toolbar = row![
            scrollable(tools)
                .width(Length::Fill)
                .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(0).scroller_width(0))),
            space(Length::Fixed(8.0), Length::Shrink),
            search,
        ]
        .align_y(Alignment::Center);

        // Where new downloads are saved. Click to choose another folder. The
        // full path is in the tooltip; narrow windows show only the icon.
        let folder = self.settings.download_dir.display().to_string();
        let mut folder_row = row![glyph(Icon::Folder, 14.0, p.accent)].spacing(6).align_y(Alignment::Center);
        if !compact {
            folder_row = folder_row.push(text(middle_ellipsis(&folder, 32)).size(12).font(iced::Font::MONOSPACE));
        }
        let folder_button = tip(
            button(folder_row)
                .padding([8, 10])
                .style(theme::flat(false))
                .on_press(Message::ChangeDownloadFolder),
            folder,
        );

        let link_bar = row![
            folder_button,
            text_input("Paste a link and press Enter", &self.url)
                .id(URL_INPUT)
                .on_input(Message::UrlChanged)
                .on_submit(Message::AddUrl)
                .style(theme::input)
                .size(13)
                .padding([8, 12]),
            tip(
                button(text("Grab site").size(13))
                    .padding([8, 10])
                    .style(theme::flat(false))
                    .on_press(Message::GrabOpen),
                "Download every file of a website",
            ),
            tip(
                button(glyph(Icon::Batch, 16.0, p.text))
                    .padding(8)
                    .style(theme::flat(false))
                    .on_press(Message::Command(Cmd::BatchAdd)),
                "Add a batch of links",
            ),
            button(row![glyph(Icon::Add, 16.0, p.on_accent), text("Add").size(13).font(bold())].spacing(6).align_y(Alignment::Center))
                .padding([8, 16])
                .style(theme::accent)
                .on_press(Message::AddUrl),
        ]
        .spacing(6)
        .align_y(Alignment::Center);

        container(column![bar, toolbar, link_bar].spacing(6))
            .padding([6, 10])
            .width(Length::Fill)
            .style(theme::surface)
            .into()
    }

    /// Sun and moon cross-fade while turning half a revolution.
    fn theme_toggle(&self, p: &Palette) -> Element<'_, Message> {
        let t = self.theme_anim.interpolate(0.0, 1.0, self.now);
        let turn = Radians(std::f32::consts::PI * t);
        let icons = stack![
            glyph(Icon::Moon, 18.0, p.accent2).rotation(turn).opacity(1.0 - t),
            glyph(Icon::Sun, 18.0, p.accent).rotation(turn).opacity(t),
        ];
        tip(
            button(icons).padding(7).style(theme::flat(false)).on_press(Message::Command(Cmd::ToggleTheme)),
            if self.settings.light_theme { "Switch to TDM Dark" } else { "Switch to TDM Light" },
        )
    }

    /// The open drop-down, with a click-away layer behind it.
    fn menu_dropdown(&self, index: usize, p: &Palette) -> Element<'_, Message> {
        let Some(menu) = MENU_BAR.get(index) else { return space(Length::Shrink, Length::Shrink).into() };
        let mut col = column![].spacing(1).padding(6);
        for item in menu.items {
            col = col.push(match item {
                Item::Separator => container(space(Length::Fill, Length::Fixed(1.0)))
                    .padding([4, 6])
                    .style(theme::separator)
                    .into(),
                Item::Command { label, cmd, accel } => {
                    // Commands this build cannot perform are greyed rather
                    // than silently swallowing a click.
                    let works = self.can(*cmd);
                    let colour = if works { p.text } else { alpha(p.muted, 0.6) };
                    let mut line = row![text(*label).size(13).color(colour).wrapping(text::Wrapping::None)].spacing(12);
                    if let Some(a) = accel {
                        line = line.push(space(Length::Fill, Length::Shrink)).push(text(*a).size(11).color(p.muted));
                    }
                    let mut b = button(line).width(Length::Fill).padding([6, 10]).style(theme::flat(false));
                    if works {
                        b = b.on_press(Message::Command(*cmd));
                    }
                    Element::from(b)
                }
            });
        }
        let panel = container(col).width(290).style(theme::card);
        // Title buttons are text plus 20px padding; the mark and wordmark
        // before them are about 90px.
        let left = 16.0 + 90.0 + MENU_BAR[..index].iter().map(|m| m.title.len() as f32 * 7.4 + 22.0).sum::<f32>();
        stack![
            mouse_area(space(Length::Fill, Length::Fill)).on_press(Message::MenuOpen(None)),
            column![
                space(Length::Fill, Length::Fixed(40.0)),
                row![space(Length::Fixed(left), Length::Shrink), opaque(panel)],
            ],
        ]
        .into()
    }

    fn main_area(&self, p: &Palette) -> Element<'_, Message> {
        let mut area = row![].spacing(0);
        if self.settings.show_categories {
            area = area.push(self.sidebar(p));
        }
        area = area.push(self.download_list(p));
        container(area).height(Length::Fill).into()
    }

    /// The sidebar is wider on bigger windows: about a quarter of the window,
    /// never narrower than 230px (so labels and counts fit) or wider than 340px.
    fn sidebar_width(&self) -> f32 {
        (self.window_width * 0.25).clamp(230.0, 340.0)
    }

    /// The width the download list gets: the window less the sidebar, when shown.
    fn list_width(&self) -> f32 {
        self.window_width - if self.settings.show_categories { self.sidebar_width() } else { 0.0 }
    }

    fn sidebar(&self, p: &Palette) -> Element<'_, Message> {
        let count = |f: Filter| {
            self.downloads
                .iter()
                .filter(|e| match f {
                    Filter::All => true,
                    Filter::Category(id) => e.category_id == Some(id),
                    Filter::Unfinished => e.snapshot.status != Status::Finished,
                    Filter::Finished => e.snapshot.status == Status::Finished,
                })
                .count()
        };
        let entry = |label: String, icon: Icon, indent: f32, f: Filter| -> Element<'_, Message> {
            let selected = self.filter == f;
            let n = count(f);
            let shown = middle_ellipsis(&label, 18);
            let name: Element<'_, Message> = {
                let el = container(text(shown.clone()).size(13).wrapping(text::Wrapping::None))
                    .width(Length::Fill)
                    .clip(true);
                if shown != label { tip(el, label.clone()) } else { el.into() }
            };
            button(
                row![
                    space(Length::Fixed(indent), Length::Shrink),
                    glyph(icon, 16.0, if selected { p.accent } else { p.muted }),
                    name,
                    container(text(n.to_string()).size(11).color(if selected { p.text } else { p.muted }))
                        .padding([1, 7])
                        .style(theme::pill(if selected { p.accent } else { p.muted })),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([6, 8])
            .style(theme::side_button(selected))
            .on_press(Message::SelectFilter(f))
            .into()
        };
        let heading = |s: &'static str| container(text(s).size(11).color(p.muted)).padding([8, 8]);

        let mut col = column![heading("LIBRARY"), entry("All downloads".into(), Icon::All, 0.0, Filter::All)].spacing(2);
        for c in &self.settings.categories {
            col = col.push(entry(c.name.clone(), assets::category_icon(Some(c.id)), 10.0, Filter::Category(c.id)));
        }
        col = col
            .push(heading("STATUS"))
            .push(entry("Unfinished".into(), Icon::Unfinished, 0.0, Filter::Unfinished))
            .push(entry("Finished".into(), Icon::Finished, 0.0, Filter::Finished));

        if !self.browsers.is_empty() {
            col = col.push(container(container(space(Length::Fill, Length::Fixed(1.0))).style(theme::separator)).padding([6, 8]));
            col = col.push(
                row![
                    heading("BROWSERS"),
                    space(Length::Fill, Length::Shrink),
                    button(text("Add").size(11))
                        .padding([2, 7])
                        .style(theme::flat(false))
                        .on_press(Message::OpenBrowserGuide),
                ]
                .align_y(Alignment::Center)
                .padding([0, 8]),
            );
            for &b in &self.browsers {
                // The host does not say which browser sent Hello, so this is shared across rows.
                let (status, tint) = if tdm::ipc::hello_within(CONNECTED_WITHIN) {
                    ("Connected", p.success)
                } else {
                    ("Not added yet", p.muted)
                };
                col = col.push(
                    row![
                        container(text(b.label()).size(13).wrapping(text::Wrapping::None)).width(Length::Fill).clip(true),
                        container(text(status).size(11).color(tint)).padding([1, 7]).style(theme::pill(tint)),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center)
                    .padding([3, 8]),
                );
            }
        }

        container(scrollable(col).height(Length::Fill))
            .width(self.sidebar_width())
            .height(Length::Fill)
            .padding([4, 8])
            .style(theme::surface)
            .into()
    }

    /// The bulk action bar above the list. It shows how many are ticked, the
    /// "also delete files" tick, Clear and Delete N. In select mode it also
    /// selects all in view and offers Done.
    fn selection_strip(&self, p: &Palette, visible_ids: &[u64]) -> Element<'_, Message> {
        let fade = ease(self.bar_anim.interpolate(0.0, 1.0, self.now));
        let total = self.marked.len();
        let marked_visible = visible_ids.iter().filter(|id| self.marked.contains(id)).count();
        let hidden = total - marked_visible;
        let all_visible = !visible_ids.is_empty() && marked_visible == visible_ids.len();
        let count = if hidden > 0 {
            format!("{total} selected  \u{b7}  {hidden} hidden")
        } else {
            format!("{total} selected")
        };

        // On a narrow list the bar drops words into tips: a bare check box,
        // a count, the delete icon with its number.
        let narrow = self.list_width() < 520.0;

        let mut clear = button(text("Clear").size(12)).padding([5, 10]).style(theme::flat(false));
        if total > 0 {
            clear = clear.on_press(Message::ClearMarks);
        }
        let delete_label = if narrow {
            row![glyph(Icon::Delete, 14.0, p.on_accent), text(total.to_string()).size(12).font(bold())]
        } else {
            row![glyph(Icon::Delete, 14.0, p.on_accent), text(format!("Delete {total}")).size(12).font(bold())]
        };
        let mut delete = button(delete_label.spacing(6).align_y(Alignment::Center))
            .padding([5, 10])
            .style(theme::danger);
        if total > 0 {
            delete = delete.on_press(Message::DeleteMarked);
        }
        let delete: Element<'_, Message> = if narrow { tip(delete, "Delete the ticked downloads") } else { delete.into() };

        let tick = checkbox(self.bulk_delete_files).on_toggle(Message::BulkDeleteFiles).size(16);
        let tick: Element<'_, Message> = if narrow {
            tip(tick, "Also delete files from disk")
        } else {
            tick.label("Also delete files from disk").text_size(13).into()
        };

        let pill_text = if narrow { total.to_string() } else { count.clone() };
        let pill = container(text(pill_text).size(11).color(p.text)).padding([2, 9]).style(theme::pill(p.accent));
        let pill: Element<'_, Message> = if narrow { tip(pill, count) } else { pill.into() };

        let mut bar = row![].width(Length::Fill).spacing(8).align_y(Alignment::Center);
        if self.select_mode {
            let select_all = button(check_box(if all_visible { 1.0 } else { 0.0 }, p))
                .padding([5, 8])
                .style(theme::flat(false))
                .on_press(Message::MarkAllVisible(!all_visible));
            let select_all: Element<'_, Message> = if narrow {
                tip(select_all, if all_visible { "Clear the ticks on these" } else { "Tick every download in view" })
            } else {
                button(
                    row![check_box(if all_visible { 1.0 } else { 0.0 }, p), text("Select all").size(12)]
                        .spacing(8)
                        .align_y(Alignment::Center),
                )
                .padding([5, 8])
                .style(theme::flat(false))
                .on_press(Message::MarkAllVisible(!all_visible))
                .into()
            };
            bar = bar.push(select_all);
        }
        bar = bar.push(pill).push(tick).push(space(Length::Fill, Length::Shrink)).push(clear).push(delete);
        if self.select_mode {
            bar = bar.push(button(text("Done").size(12)).padding([5, 10]).style(theme::flat(false)).on_press(Message::ToggleSelectMode(false)));
        }

        // The bar unrolls from the top: its height follows the animation, and
        // the content is clipped, so the list below moves with it.
        let surface = p.surface;
        let inner = container(bar)
            .padding([6, 12])
            .width(Length::Fill)
            .center_y(Length::Fixed(BAR_HEIGHT))
            .style(move |_| container::Style { background: Some(alpha(surface, fade).into()), ..Default::default() });
        container(inner)
            .width(Length::Fill)
            .height(Length::Fixed(BAR_HEIGHT * fade))
            .clip(true)
            .into()
    }

    fn download_list(&self, p: &Palette) -> Element<'_, Message> {
        let visible = self.visible();
        let visible_ids: Vec<u64> = visible.iter().map(|e| e.id).collect();
        let content: Element<'_, Message> = if visible.is_empty() {
            let (title, hint) = if self.downloads.is_empty() {
                ("No downloads yet", "Paste a link above, or let your browser hand one over.")
            } else {
                ("Nothing matches", "Try another category or search.")
            };
            container(
                column![
                    glyph(Icon::All, 40.0, alpha(p.muted, 0.6)),
                    text(title).size(15),
                    text(hint).size(12).color(p.muted),
                ]
                .spacing(8)
                .align_x(Alignment::Center),
            )
            .center(Length::Fill)
            .into()
        } else {
            let mut rows = column![].spacing(6).padding(iced::Padding { top: 10.0, right: 14.0, bottom: 10.0, left: 10.0 });
            for e in visible {
                rows = rows.push(self.list_row(e, p));
            }
            scrollable(rows).height(Length::Fill).into()
        };
        let mut list = column![].width(Length::Fill).height(Length::Fill);
        // The bar stays while it slides away, so it is drawn until its animation ends.
        if self.bar_shown() || self.bar_anim.is_animating(self.now) {
            list = list.push(self.selection_strip(p, &visible_ids));
        }
        list = list.push(content);
        container(list).width(Length::Fill).height(Length::Fill).style(theme::chrome).into()
    }

    fn list_row<'a>(&'a self, e: &'a Entry, p: &Palette) -> Element<'a, Message> {
        let s = &e.snapshot;
        let selected = self.selected == Some(e.id);
        let frac = match s.total {
            Some(t) if t > 0 => (s.done as f32 / t as f32).clamp(0.0, 1.0),
            _ if s.status == Status::Finished => 1.0,
            _ => 0.0,
        };
        let running = s.status == Status::Running;
        let (label, tint) = status_badge(s.status, p);
        // The info column gets what the row has left after its fixed parts:
        // padding, badge, actions, gaps and the select-mode check slot.
        let slot = self.select_anim.interpolate(0.0, CHECK_SLOT, self.now);
        let fixed = 24.0 + 24.0 + 12.0 + 36.0 + 32.0 + 32.0 + 40.0 + if slot > 0.5 { slot + 10.0 } else { 0.0 };
        let info_w = (self.list_width() - fixed).max(0.0);
        let narrow = info_w < 260.0;
        // The detected type, coloured by whether it agrees with the name. A
        // narrow row drops it, unless it is a warning worth seeing.
        let verdict = s.kind.map(|kind| filetype::verdict(kind, &e.name, s.content_type.as_deref(), false));
        let show_type = !narrow || matches!(verdict, Some(Verdict::Mismatch | Verdict::WebPage));
        let type_pill = s.kind.filter(|_| show_type).map(|kind| {
            let colour = match filetype::verdict(kind, &e.name, s.content_type.as_deref(), false) {
                Verdict::Mismatch => p.warning,
                Verdict::WebPage => p.danger,
                Verdict::Match | Verdict::Unchecked => p.muted,
            };
            container(text(kind.label()).size(11).color(colour)).padding([2, 9]).style(theme::pill(colour))
        });

        let icon = e.category_id.map_or(Icon::File, |id| assets::category_icon(Some(id)));
        let badge = container(glyph(icon, 18.0, p.accent))
            .center(36)
            .style(move |t: &Theme| container::Style {
                background: Some(alpha(Palette::of(t).accent, 0.12).into()),
                border: iced::Border { radius: 10.0.into(), ..Default::default() },
                ..Default::default()
            });

        let mut detail = match s.total {
            // A narrow row drops "of total": the progress bar shows it.
            Some(_) if s.status != Status::Finished && narrow => fmt_bytes(s.done as f64),
            Some(t) if s.status != Status::Finished => format!("{} of {}", fmt_bytes(s.done as f64), fmt_bytes(t as f64)),
            Some(t) => fmt_bytes(t as f64),
            None if s.done > 0 => fmt_bytes(s.done as f64),
            None => "size unknown".to_owned(),
        };
        if s.total.is_some() && s.status != Status::Finished && !narrow {
            detail.push_str(&format!("  ·  {:.0}%", frac * 100.0));
        }
        if running && e.speed > 0.0 {
            detail.push_str(&format!("  ·  {}/s  ·  {}", fmt_bytes(e.speed), time_left(s, e.speed)));
        }
        if let (Status::Failed, Some(err)) = (s.status, &s.error) {
            detail = short_error(err);
        }
        if let (Status::Finished, Some(verdict)) = (s.status, &e.scan) {
            detail = match verdict {
                tdm::scan::Verdict::Clean => "Virus scan: clean".to_owned(),
                tdm::scan::Verdict::Threat => "Virus scan: THREAT FOUND".to_owned(),
                tdm::scan::Verdict::Error(_) => "Virus scan: could not run".to_owned(),
            };
        }

        let shimmer = running.then(|| (self.now.saturating_duration_since(self.epoch).as_secs_f32() / 1.8).fract());
        let bar_colour = match s.status {
            Status::Finished => Some(p.success),
            Status::Failed => Some(p.danger),
            Status::Paused | Status::Queued => Some(alpha(p.muted, 0.7)),
            Status::Running => None,
        };
        let bar = progress_bar(0.0..=1.0, frac).girth(6).style(theme::progress(bar_colour, shimmer));

        // The name is cut to what the title has room for, with the full name in a tooltip.
        let pills_w = 100.0 + if type_pill.is_some() { 100.0 } else { 0.0 };
        let budget = ((info_w - pills_w) / 7.0).max(8.0) as usize;
        let shown = middle_ellipsis(&e.name, budget);
        let name_text = container(text(shown.clone()).size(13).font(bold()).wrapping(text::Wrapping::None))
            .width(Length::Fill)
            .clip(true);
        let name_el: Element<'a, Message> = if shown != e.name { tip(name_text, e.name.clone()) } else { name_text.into() };
        let mut title = row![name_el].spacing(8).align_y(Alignment::Center);
        if let Some(pill) = type_pill {
            title = title.push(pill);
        }
        let title = title.push(container(text(label).size(11).color(p.text)).padding([2, 9]).style(theme::pill(tint)));
        // A failed row's detail is short; the full error waits in a tooltip.
        let failed_error = if s.status == Status::Failed { s.error.clone() } else { None };
        let detail_text = container(
            text(detail).size(11).color(if s.status == Status::Failed { p.danger } else { p.muted }).wrapping(text::Wrapping::None),
        )
        .width(Length::Fill)
        .clip(true);
        let detail_el: Element<'a, Message> = match failed_error {
            Some(err) => tip(detail_text, err),
            None => detail_text.into(),
        };
        let info = column![title, bar, detail_el].spacing(5).width(Length::Fill);

        let toggle: Element<'_, Message> = match s.status {
            Status::Running | Status::Paused | Status::Failed => {
                let (icon, label) = if running { (Icon::Pause, "Pause") } else { (Icon::Resume, "Resume") };
                tip(icon_button(icon, p.text, Message::TogglePause(e.id)), label)
            }
            Status::Finished => tip(icon_button(Icon::Open, p.text, Message::OpenFile(e.id)), "Open file"),
            Status::Queued => space(Length::Fixed(32.0), Length::Shrink).into(),
        };
        let more = tip(
            icon_button(Icon::More, p.muted, Message::RowMenu(if self.row_menu == Some(e.id) { None } else { Some(e.id) })),
            "More actions",
        );

        // In select mode the row grows a check box column; it fades in and out with the mode.
        // How far this row is ticked. A row ticked just now eases in over MARK_FLASH.
        let flash = match self.mark_flash {
            Some((id, at)) if id == e.id => {
                Some(ease(self.now.saturating_duration_since(at).as_secs_f32() / MARK_FLASH.as_secs_f32()))
            }
            _ => None,
        };
        let marked = match (self.marked.contains(&e.id), flash) {
            (false, _) => 0.0,
            (true, Some(t)) => t,
            (true, None) => 1.0,
        };

        let mut line = row![].spacing(10).align_y(Alignment::Center);
        if slot > 0.5 {
            line = line.push(container(check_box(marked, p)).width(Length::Fixed(slot)).clip(true));
        }
        let line = line.push(badge).push(info).push(toggle).push(more);
        let mut card = column![line].spacing(8);
        if self.row_menu == Some(e.id) {
            card = card.push(self.row_actions(e, p));
        }

        let row_button = button(card)
            .width(Length::Fill)
            .padding([10, 12])
            .style(theme::row_button(selected, marked))
            .on_press(Message::RowPress(e.id));
        // A fresh tick swells the row a touch and settles it back.
        let row_button: Element<'a, Message> = match flash.filter(|t| *t < 1.0) {
            Some(t) => float(row_button).scale(1.0 + 0.01 * (1.0 - t)).into(),
            None => row_button.into(),
        };
        let area = mouse_area(row_button).on_right_press(Message::RowMenu(Some(e.id)));
        if self.select_mode || self.ctrl_down {
            // Double-click means nothing while ticking rows, and Ctrl+click never opens or pauses.
            area.into()
        } else {
            area.on_double_click(if s.status == Status::Finished { Message::OpenFile(e.id) } else { Message::TogglePause(e.id) })
                .into()
        }
    }

    /// The per-row action strip, opened from the row's "more" button or a
    /// right-click.
    fn row_actions<'a>(&'a self, e: &'a Entry, p: &Palette) -> Element<'a, Message> {
        let finished = e.snapshot.status == Status::Finished;
        let action = |icon: Icon, label: &'static str, msg: Option<Message>| {
            let colour = if msg.is_some() { p.text } else { alpha(p.muted, 0.5) };
            let mut b = button(row![glyph(icon, 14.0, colour), text(label).size(12).color(colour)].spacing(6).align_y(Alignment::Center))
                .padding([5, 10])
                .style(theme::flat(false));
            if let Some(m) = msg {
                b = b.on_press(m);
            }
            b
        };
        let actions = row![
            action(Icon::Open, "Open", finished.then_some(Message::OpenFile(e.id))),
            action(Icon::Folder, "Show in folder", Some(Message::ShowInFolder(e.id))),
            action(Icon::Shield, "Verify SHA-256", finished.then_some(Message::Verify(e.id))),
            action(Icon::Check, "Select", Some(Message::Mark(e.id))),
            action(Icon::Batch, "Copy link", Some(Message::CopyLink(e.id))),
            action(Icon::Delete, "Delete", Some(Message::Remove(e.id))),
        ]
        .spacing(2);
        container(scrollable(actions).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(3).scroller_width(3))))
            .padding(iced::Padding::default().left(46.0))
            .into()
    }

    fn status_bar(&self, p: &Palette) -> Element<'_, Message> {
        let active = self.count(Status::Running);
        let queued = self.count(Status::Queued);
        let done = self.count(Status::Finished);
        let total = self.spark.back().copied().unwrap_or(0.0);

        // The sparkline: one thin bar per tick, scaled to the window's peak.
        let peak = self.spark.iter().copied().fold(1.0, f64::max);
        let mut spark = row![].spacing(1).align_y(Alignment::End).height(16);
        for _ in self.spark.len()..SPARK_LEN {
            spark = spark.push(space(Length::Fixed(2.0), Length::Fixed(1.0)));
        }
        for &v in &self.spark {
            let h = (v / peak * 15.0).max(1.0) as f32;
            let a = if v > 0.0 { 0.45 + 0.55 * (v / peak) as f32 } else { 0.25 };
            let c = if v > 0.0 { p.accent } else { p.muted };
            spark = spark.push(container(space(Length::Fixed(2.0), Length::Fixed(h))).style(move |_| container::Style {
                background: Some(alpha(c, a).into()),
                border: iced::Border { radius: 1.0.into(), ..Default::default() },
                ..Default::default()
            }));
        }

        let dot = |c: Color| container(space(Length::Fixed(7.0), Length::Fixed(7.0))).style(move |_| container::Style {
            background: Some(c.into()),
            border: iced::Border { radius: 99.0.into(), ..Default::default() },
            ..Default::default()
        });
        let mut bar = row![
            dot(if active > 0 { p.accent } else { p.muted }),
            text(format!("{active} active  ·  {queued} queued  ·  {done} done")).size(12).color(p.muted).wrapping(text::Wrapping::None),
            space(Length::Fill, Length::Shrink),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if let Some(l) = self.settings.speed_limit {
            bar = bar.push(text(format!("cap {}/s", fmt_bytes(l as f64))).size(12).color(p.muted));
        }
        // Narrow windows drop the sparkline and shorten the capture warning.
        let narrow = self.window_width < 760.0;
        if !self.capture_ready {
            let note: Element<'_, Message> = if narrow {
                tip(text("no capture").size(12).color(p.danger).wrapping(text::Wrapping::None), "browser capture unavailable")
            } else {
                text("browser capture unavailable").size(12).color(p.danger).into()
            };
            bar = bar.push(note);
        }
        if !narrow {
            bar = bar.push(spark);
        }
        bar = bar.push(container(text(format!("{}/s", fmt_bytes(total))).size(12).font(bold())).width(80).align_right(80));
        container(bar).padding([5, 12]).width(Length::Fill).style(theme::surface).into()
    }

    // --------------------------------------------------------------- overlays

    /// A dimmed backdrop that fades in, with the dialog settling from a
    /// slight zoom. Clicking the backdrop dismisses.
    fn modal<'a>(&self, content: Element<'a, Message>, dismiss: Message, max_w: f32, scroll: bool) -> Element<'a, Message> {
        let t = ease(self.overlay_anim.interpolate(0.0, 1.0, self.now));
        let backdrop = mouse_area(
            container(space(Length::Fill, Length::Fill)).style(move |_| container::Style {
                background: Some(Color { a: 0.5 * t, ..Color::BLACK }.into()),
                ..Default::default()
            }),
        )
        .on_press(dismiss);
        // Cap the card so a wide window cannot stretch a dialog across the
        // screen. It fills up to `max_w`, then stays centred. A scrolling
        // dialog takes the window's height, so its content scrolls inside it.
        let content: Element<'a, Message> = if scroll {
            // Shrink, not Fill: a Fill child makes the whole card fill the window.
            // The centred wrapper already caps the card at the window height, so a
            // tall dialog scrolls inside it and a short one fits its content.
            // The bar's spacing keeps it beside the content, not over it.
            scrollable(content)
                .direction(scrollable::Direction::Vertical(scrollable::Scrollbar::new().spacing(4)))
                .height(Length::Shrink)
                .into()
        } else {
            content
        };
        let card = container(content).padding(20).width(Length::Fill).max_width(max_w).style(theme::card);
        let card = float(card).scale(1.0 + 0.04 * (1.0 - t));
        stack![
            opaque(backdrop),
            container(opaque(card)).center(Length::Fill).padding(16),
        ]
        .into()
    }

    /// Two wordings: a finished download offers to remove the file from disk;
    /// an unfinished one offers to drop its partial data.
    /// The type warning: what the file is, and what the user can do about it.
    fn type_dialog(&self, p: &Palette) -> Element<'_, Message> {
        let Some(t) = &self.type_prompt else { return space(Length::Shrink, Length::Shrink).into() };
        let Some(entry) = self.entry(t.id) else { return space(Length::Shrink, Length::Shrink).into() };
        let web = t.verdict == Verdict::WebPage;
        let mut body = column![
            row![
                glyph(Icon::Shield, 20.0, p.warning),
                text("This file is not what its name says").size(16).font(bold()).wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            tip_long(
                text(middle_ellipsis(&entry.name, 48)).size(13).font(bold()).wrapping(text::Wrapping::None),
                entry.name.clone(),
            ),
            text(format!("The content is {}.", t.kind.label())).size(13).wrapping(text::Wrapping::WordOrGlyph),
        ]
        .spacing(14)
        .max_width(440);
        if let Some(ct) = &t.content_type {
            body = body.push(tip_long(
                text(format!("The server said it was {}.", middle_ellipsis(ct, 36))).size(12).color(p.muted),
                ct.clone(),
            ));
        }
        if entry.scanning {
            body = body.push(text("Renaming is available once the virus scan finishes.").size(12).color(p.muted));
        }
        let open_page = button(text("Open the page in browser").size(13)).padding([7, 18]);
        let open_page = if web {
            open_page.style(theme::accent).on_press(Message::TypeOpenPage(t.id))
        } else {
            open_page.style(theme::flat(false)).on_press(Message::TypeOpenPage(t.id))
        };
        let mut buttons = row![
            space(Length::Fill, Length::Shrink),
            button(text("Keep").size(13)).padding([7, 18]).style(theme::flat(false)).on_press(Message::TypeKeep),
        ]
        .spacing(8);
        if !web && t.kind.renamable() {
            let ext = std::path::Path::new(&filetype::fixed_name(&entry.name, t.kind))
                .extension()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut rename = button(text(format!("Rename to .{ext}")).size(13).font(bold())).padding([7, 18]).style(theme::accent);
            if !entry.scanning {
                rename = rename.on_press(Message::TypeRename(t.id));
            }
            buttons = buttons.push(rename);
        }
        buttons = buttons.push(open_page);
        body.push(buttons).into()
    }

    /// Three strengths, picked by `confirm_tier`. One download keeps the plain
    /// dialog. Several get a count and a list of names. Many, running ones, or
    /// files on disk make the user type the count first.
    fn delete_dialog(&self, p: &Palette) -> Element<'_, Message> {
        let Some(confirm) = &self.confirm_delete else { return space(Length::Shrink, Length::Shrink).into() };
        let n = confirm.ids.len();
        let tier = confirm_tier(n, confirm.running, confirm.also_delete_file);
        let entries: Vec<&Entry> = confirm.ids.iter().filter_map(|id| self.entry(*id)).collect();

        if tier == Tier::Simple {
            let Some(entry) = entries.first().copied() else { return space(Length::Shrink, Length::Shrink).into() };
            let finished = entry.snapshot.status == Status::Finished;
            return column![
                row![glyph(Icon::Delete, 20.0, p.danger), text("Delete download").size(16).font(bold())].spacing(8).align_y(Alignment::Center),
                tip_long(
                    text(format!("Remove \u{201c}{}\u{201d} from the list?", middle_ellipsis(&entry.name, 34)))
                        .size(13)
                        .wrapping(text::Wrapping::WordOrGlyph),
                    entry.name.clone(),
                ),
                checkbox(confirm.also_delete_file)
                    .label(if finished {
                        "Also delete the file from disk"
                    } else {
                        "Also delete the partial file and its resume data"
                    })
                    .on_toggle(Message::ConfirmDeleteToggleFile)
                    .size(16)
                    .text_size(13),
                row![
                    space(Length::Fill, Length::Shrink),
                    button(text("Cancel").size(13)).padding([7, 18]).style(theme::flat(false)).on_press(Message::CancelDelete),
                    button(text("Delete").size(13).font(bold())).padding([7, 18]).style(theme::danger).on_press(Message::ConfirmDelete),
                ]
                .spacing(8),
            ]
            .spacing(14)
            .max_width(440)
            .into();
        }

        let typed_ok_now = typed_ok(n, &confirm.typed);
        let mut names = column![].spacing(3);
        for e in entries.iter().take(5) {
            names = names.push(tip_long(
                text(middle_ellipsis(&e.name, 52)).size(13).wrapping(text::Wrapping::None),
                e.name.clone(),
            ));
        }
        if entries.len() > 5 {
            names = names.push(text(format!("and {} more", entries.len() - 5)).size(12).color(p.muted));
        }
        let mut body = column![
            row![
                glyph(Icon::Delete, 20.0, p.danger),
                text(format!("Delete {n} downloads")).size(16).font(bold()).wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            names,
        ]
        .spacing(14)
        .max_width(440);
        if confirm.running > 0 {
            body = body.push(text(format!("{} running {} will be stopped", confirm.running, if confirm.running == 1 { "download" } else { "downloads" })).size(13).color(p.danger));
        }
        // Ticked downloads the search or filter hides are still deleted, so say how many.
        let shown_ids: BTreeSet<u64> = self.visible().iter().map(|e| e.id).collect();
        let hidden = confirm.ids.iter().filter(|id| !shown_ids.contains(id)).count();
        if hidden > 0 {
            body = body.push(text(format!("Including {hidden} not shown in this list, because the search or filter hides them.")).size(12).color(p.muted));
        }
        body = body.push(
            checkbox(confirm.also_delete_file)
                .label("Also delete the files from disk")
                .on_toggle(Message::ConfirmDeleteToggleFile)
                .size(16)
                .text_size(13),
        );
        if tier == Tier::Typed {
            // Enter submits only once the count is right, so a stray Enter cannot delete.
            let mut input = text_input("", &confirm.typed)
                .id(CONFIRM_INPUT)
                .on_input(Message::ConfirmTyped)
                .style(theme::input)
                .size(13)
                .padding([8, 12]);
            if typed_ok_now {
                input = input.on_submit(Message::ConfirmDelete);
            }
            body = body.push(column![text(format!("Type {n} to confirm.")).size(12).color(p.muted), input].spacing(6));
        }
        let mut delete = button(text(format!("Delete {n}")).size(13).font(bold())).padding([7, 18]).style(theme::danger);
        if tier != Tier::Typed || typed_ok_now {
            delete = delete.on_press(Message::ConfirmDelete);
        }
        body.push(
            row![
                space(Length::Fill, Length::Shrink),
                button(text("Cancel").size(13)).padding([7, 18]).style(theme::flat(false)).on_press(Message::CancelDelete),
                delete,
            ]
            .spacing(8),
        )
        .into()
    }

    fn batch_dialog(&self, p: &Palette) -> Element<'_, Message> {
        let Some(b) = &self.batch else { return space(Length::Shrink, Length::Shrink).into() };
        // `summary` is what shows; `full` is the same text untruncated, for the tooltip.
        let failed = b.links.is_err();
        let (summary, full, colour, ready) = match &b.links {
            Ok(v) if v.is_empty() => {
                let s = "One link per line. Use [001-120] or [a-z] for ranges.".to_owned();
                (s.clone(), s, p.muted, false)
            }
            Ok(v) if v.len() == 1 => (format!("1 link: {}", middle_ellipsis(&v[0], 40)), format!("1 link: {}", v[0]), p.muted, true),
            Ok(v) => {
                let last = &v[v.len() - 1];
                (
                    format!(
                        "{} links: {}  \u{2026}  {}",
                        v.len(),
                        middle_ellipsis(&v[0], 26),
                        middle_ellipsis(last, 26)
                    ),
                    format!("{} links: {}  \u{2026}  {}", v.len(), v[0], last),
                    p.muted,
                    true,
                )
            }
            Err(e) => {
                let s = e.to_string();
                (s.clone(), s, p.danger, false)
            }
        };
        let summary_line: Element<'_, Message> = if failed {
            text(summary).size(12).color(colour).wrapping(text::Wrapping::WordOrGlyph).into()
        } else {
            tip_long(text(summary).size(12).color(colour).wrapping(text::Wrapping::None), full)
        };
        let mut add = button(text("Add all").size(13).font(bold())).padding([7, 18]).style(theme::accent);
        if ready {
            add = add.on_press(Message::BatchAdd);
        }
        column![
            row![glyph(Icon::Batch, 20.0, p.accent), text("Add a batch of links").size(16).font(bold())].spacing(8).align_y(Alignment::Center),
            text_editor(&b.content)
                .placeholder("https://example.com/photos/img[001-120].jpg")
                .on_action(Message::BatchEdit)
                .wrapping(text::Wrapping::WordOrGlyph)
                .height(140)
                .size(13)
                .padding(10),
            text("Mirror links (optional, one per line). If a connection keeps failing, TDM tries these.")
                .size(12)
                .color(p.muted),
            text_editor(&b.mirrors)
                .placeholder("https://mirror.example.com/same/file.zip")
                .on_action(Message::BatchMirrors)
                .wrapping(text::Wrapping::WordOrGlyph)
                .height(70)
                .size(13)
                .padding(10),
            summary_line,
            row![
                space(Length::Fill, Length::Shrink),
                button(text("Cancel").size(13)).padding([7, 18]).style(theme::flat(false)).on_press(Message::BatchClose),
                add,
            ]
            .spacing(8),
        ]
        .spacing(12)
        .max_width(560)
        .into()
    }

    fn verify_dialog(&self, p: &Palette) -> Element<'_, Message> {
        let Some(v) = &self.verify else { return space(Length::Shrink, Length::Shrink).into() };
        let digest: Element<'_, Message> = match &v.digest {
            None => column![
                text("Computing SHA-256 and MD5\u{2026}").size(12).color(p.muted),
                progress_bar(0.0..=1.0, 1.0)
                    .girth(4)
                    .style(theme::progress(None, Some((self.now.saturating_duration_since(self.epoch).as_secs_f32() / 1.2).fract()))),
            ]
            .spacing(6)
            .into(),
            // Each label sits on its own line, so the 64-character hash has the full width.
            Some(Ok(d)) => column![
                text("SHA-256").size(12).color(p.muted),
                text(d.sha256.clone()).size(12).font(iced::Font::MONOSPACE).wrapping(text::Wrapping::Glyph),
                text("MD5").size(12).color(p.muted),
                text(d.md5.clone()).size(12).font(iced::Font::MONOSPACE).wrapping(text::Wrapping::Glyph),
            ]
            .spacing(4)
            .into(),
            Some(Err(e)) => text(format!("Could not read the file: {e}")).size(12).color(p.danger).into(),
        };
        let verdict: Element<'_, Message> = match &v.digest {
            Some(Ok(d)) if !v.expected.trim().is_empty() => {
                let ok = tdm::checksum::matches_any(d, &v.expected);
                let (icon, label, c) = if ok {
                    (Icon::Check, "Match \u{2014} the file is intact", p.success)
                } else {
                    (Icon::Close, "Mismatch \u{2014} the file differs from the published hash", p.danger)
                };
                container(row![glyph(icon, 14.0, c), text(label).size(12).color(c)].spacing(6).align_y(Alignment::Center))
                    .padding([4, 10])
                    .style(theme::pill(c))
                    .into()
            }
            _ => space(Length::Shrink, Length::Shrink).into(),
        };
        column![
            row![glyph(Icon::Shield, 20.0, p.accent), text("Verify checksum").size(16).font(bold())].spacing(8).align_y(Alignment::Center),
            tip_long(
                text(middle_ellipsis(&v.name, 56)).size(13).color(p.muted).wrapping(text::Wrapping::None),
                v.name.clone(),
            ),
            digest,
            text_input("Paste the published SHA-256 or MD5 to compare", &v.expected)
                .on_input(Message::VerifyExpected)
                .style(theme::input)
                .size(12)
                .padding(8),
            verdict,
            row![
                space(Length::Fill, Length::Shrink),
                button(text("Close").size(13)).padding([7, 18]).style(theme::accent).on_press(Message::CloseVerify),
            ],
        ]
        .spacing(12)
        .max_width(560)
        .into()
    }

    /// Preferences: the pages, plus the switch that belongs to this shell
    /// rather than to a settings page.
    /// "Grab site": one start address, then every file the crawl finds is queued.
    fn grab_dialog(&self, p: &Palette) -> Element<'_, Message> {
        let Some(g) = &self.grab else { return space(Length::Shrink, Length::Shrink).into() };
        let (note, colour) = match (&g.error, g.working) {
            (Some(e), _) => (e.clone(), p.danger),
            (None, true) => ("Reading the site\u{2026} this can take a minute.".to_owned(), p.muted),
            (None, false) => (
                format!(
                    "Up to {} files, {} levels of pages, this site only. robots.txt is respected.",
                    tdm::grabber::MAX_PAGES,
                    tdm::grabber::MAX_DEPTH
                ),
                p.muted,
            ),
        };
        let grab_button = button(text("Grab").size(13)).padding([7, 18]).style(theme::accent);
        let grab_button = if g.working { grab_button } else { grab_button.on_press(Message::GrabStart) };
        column![
            row![glyph(Icon::Batch, 20.0, p.accent), text("Grab a website").size(16).font(bold())]
                .spacing(8)
                .align_y(Alignment::Center),
            text_input("https://example.com", &g.url)
                .on_input(Message::GrabUrl)
                .on_submit(Message::GrabStart)
                .style(theme::input)
                .size(13)
                .padding(8),
            text(note).size(12).color(colour).wrapping(text::Wrapping::WordOrGlyph),
            row![
                space(Length::Fill, Length::Shrink),
                button(text("Close").size(13)).padding([7, 18]).style(theme::flat(false)).on_press(Message::GrabClose),
                grab_button,
            ]
            .spacing(8),
        ]
        .spacing(12)
        .max_width(560)
        .into()
    }

    /// The prompt for a download the browser handed over, like IDM's: name,
    /// size and folder, then Download now or Add to queue.
    fn capture_dialog(&self, p: &Palette) -> Element<'_, Message> {
        let Some(item) = self.prompts.front() else { return space(Length::Shrink, Length::Shrink).into() };
        let name = capture_name(item);
        if is_media_name(&name) {
            return self.media_capture_dialog(p, item, &name);
        }
        let size = item.size.map_or("unknown size".to_owned(), |s| fmt_bytes(s as f64));
        let folder = self.settings.destination_for(&name).parent().map(|d| d.display().to_string()).unwrap_or_default();
        let more = self.prompts.len() - 1;
        column![
            row![glyph(Icon::Add, 20.0, p.accent), text("Download file").size(16).font(bold())]
                .spacing(8)
                .align_y(Alignment::Center),
            tip_long(
                text(middle_ellipsis(&name, 48)).size(15).font(bold()).wrapping(text::Wrapping::None),
                name,
            ),
            tip_long(
                text(middle_ellipsis(&item.url, 72)).size(11).color(p.muted).wrapping(text::Wrapping::None),
                item.url.clone(),
            ),
            column![
                text(format!("Size: {size}")).size(12).color(p.muted),
                tip_long(
                    text(format!("Save to: {}", middle_ellipsis(&folder, 52))).size(12).color(p.muted).wrapping(text::Wrapping::None),
                    folder.clone(),
                ),
            ]
            .spacing(4),
            text(if more > 0 { format!("{more} more waiting") } else { String::new() }).size(11).color(p.muted),
            row![
                space(Length::Fill, Length::Shrink),
                button(text("Cancel").size(13)).padding([7, 16]).style(theme::flat(false)).on_press(Message::CaptureCancel),
                button(text("Add to queue").size(13)).padding([7, 16]).style(theme::flat(false)).on_press(Message::CaptureQueue),
                button(text("Download now").size(13)).padding([7, 16]).style(theme::accent).on_press(Message::CaptureStart),
            ]
            .spacing(8),
        ]
        .spacing(10)
        .max_width(520)
        .into()
    }

    /// The prompt for an image or video: an editable name (the stem only, with
    /// the extension fixed after it), and a save folder that can be changed.
    fn media_capture_dialog(&self, p: &Palette, item: &Incoming, default: &str) -> Element<'_, Message> {
        let size = item.size.map_or("unknown size".to_owned(), |s| fmt_bytes(s as f64));
        let more = self.prompts.len() - 1;
        let ext = std::path::Path::new(default).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let shown = self.prompt_name.clone().unwrap_or_else(|| stem_of(default));
        let folder_path = self.prompt_folder(default);
        let folder = folder_path.display().to_string();
        // Only an edit can be invalid; the untouched stem always is.
        let problem = self.prompt_name.as_deref().and_then(stem_error);
        let (hint, hint_colour) = match (problem, self.prompt_name.as_deref()) {
            (Some(reason), _) => (reason.to_owned(), p.danger),
            (None, Some(typed)) => {
                let cleaned = tdm::native_host::sanitize_filename(typed.trim());
                if cleaned != typed.trim() {
                    (format!("Will be saved as {}.{ext}", middle_ellipsis(&cleaned, 44)), p.muted)
                } else {
                    (String::new(), p.muted)
                }
            }
            (None, None) => (String::new(), p.muted),
        };
        let valid = problem.is_none();
        let name_row = row![
            text_input("", &shown)
                .on_input(Message::CaptureName)
                .on_submit_maybe(valid.then_some(Message::CaptureStart))
                .style(theme::input)
                .size(13)
                .padding(8),
            text(format!(".{ext}")).size(13).color(p.muted),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let location = row![
            tip_long(
                text(format!("Location: {}", middle_ellipsis(&folder, 44))).size(12).color(p.muted).wrapping(text::Wrapping::None),
                folder.clone(),
            ),
            space(Length::Fill, Length::Shrink),
            button(text("Change").size(12)).padding([4, 10]).style(theme::flat(false)).on_press(Message::CaptureFolder),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        column![
            row![glyph(Icon::Add, 20.0, p.accent), text("Download file").size(16).font(bold())]
                .spacing(8)
                .align_y(Alignment::Center),
            name_row,
            text(hint).size(12).color(hint_colour).wrapping(text::Wrapping::WordOrGlyph),
            tip_long(
                text(middle_ellipsis(&item.url, 72)).size(11).color(p.muted).wrapping(text::Wrapping::None),
                item.url.clone(),
            ),
            column![text(format!("Size: {size}")).size(12).color(p.muted), location].spacing(4),
            text(if more > 0 { format!("{more} more waiting") } else { String::new() }).size(11).color(p.muted),
            row![
                space(Length::Fill, Length::Shrink),
                button(text("Cancel").size(13)).padding([7, 16]).style(theme::flat(false)).on_press(Message::CaptureCancel),
                button(text("Add to queue").size(13))
                    .padding([7, 16])
                    .style(theme::flat(false))
                    .on_press_maybe(valid.then_some(Message::CaptureQueue)),
                button(text("Download now").size(13))
                    .padding([7, 16])
                    .style(theme::accent)
                    .on_press_maybe(valid.then_some(Message::CaptureStart)),
            ]
            .spacing(8),
        ]
        .spacing(10)
        .max_width(520)
        .into()
    }

    /// The add-TDM-to-your-browser page: detected browsers, one button each,
    /// then the four steps the browser's extensions page needs.
    fn browser_dialog(&self, p: &Palette) -> Element<'_, Message> {
        let connected = tdm::ipc::hello_within(CONNECTED_WITHIN);
        let mut list = column![].spacing(8);
        if self.browsers.is_empty() {
            list = list.push(text("No supported browser was found on this PC.").size(13).color(p.muted));
        }
        for &b in &self.browsers {
            let (status, tint) = if connected { ("Connected", p.success) } else { ("Not added yet", p.muted) };
            let dir = b.extension_dir();
            let built = dir.join("manifest.json").is_file();
            let mut block = column![
                row![
                    text(b.label()).size(14).font(bold()).width(Length::Fill),
                    container(text(status).size(11).color(tint)).padding([2, 8]).style(theme::pill(tint)),
                    button(text(format!("Add to {}", b.label())).size(13))
                        .padding([7, 14])
                        .style(theme::accent)
                        .on_press(Message::AddToBrowser(b)),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                // The folder path gets its own full-width line, so it can wrap
                // without running into the status pill.
                {
                    let dir_s = dir.display().to_string();
                    tip_long(
                        text(middle_ellipsis(&dir_s, 68)).size(11).font(iced::Font::MONOSPACE).color(p.muted).wrapping(text::Wrapping::WordOrGlyph),
                        dir_s,
                    )
                },
                row![
                    button(text("Copy path").size(12))
                        .padding([6, 10])
                        .style(theme::flat(false))
                        .on_press(Message::CopyExtensionPath(b)),
                    button(text("Open folder").size(12))
                        .padding([6, 10])
                        .style(theme::flat(false))
                        .on_press(Message::OpenExtensionFolder(b)),
                ]
                .spacing(8),
            ]
            .spacing(6);
            if !built {
                block = block.push(
                    column![
                        text("This folder is not built yet. Run the build once, then reopen this page.")
                            .size(12)
                            .color(p.danger),
                        button(text("Copy build command").size(12))
                            .padding([6, 10])
                            .style(theme::flat(false))
                            .on_press(Message::CopyBuildCommand),
                        text(build_command()).size(11).font(iced::Font::MONOSPACE).color(p.muted).wrapping(text::Wrapping::WordOrGlyph),
                    ]
                    .spacing(4),
                );
            }
            list = list.push(block);
        }
        let step = |n: &'static str, title: &'static str, detail: &'static str| {
            row![
                container(text(n).size(12)).padding([2, 9]).style(theme::pill(p.accent)),
                column![text(title).size(13), text(detail).size(11).color(p.muted)].spacing(2),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
        };
        let steps = column![
            step("1", "Turn on Developer mode", "The switch in the top-right of the page that opens."),
            step("2", "Click Load unpacked", "It sits at the top-left of that page."),
            step("3", "Press Ctrl+V, then click Select Folder", "The TDM folder path is already on your clipboard."),
            step("4", "Done", "This page turns green once TDM is connected."),
        ]
        .spacing(12);
        column![
            row![glyph(Icon::Add, 20.0, p.accent), text("Add TDM to your browser").size(16).font(bold())]
                .spacing(8)
                .align_y(Alignment::Center),
            text("TDM found these browsers on this PC. Pick one, then follow the steps.").size(12).color(p.muted),
            list,
            steps,
            text("Private windows: after step 4, open TDM's Details and turn on Allow in Incognito.").size(11).color(p.muted),
            row![
                space(Length::Fill, Length::Shrink),
                button(text("Close").size(13)).padding([7, 18]).style(theme::accent).on_press(Message::CloseBrowserGuide),
            ],
        ]
        .spacing(14)
        .max_width(560)
        .into()
    }

    fn options_dialog(&self) -> Element<'_, Message> {
        let pages = tdm::ui::options::view(&self.settings).map(Message::Options);
        let body = column![
            text("Preferences").size(16).font(bold()),
            container(pages).height(Length::Fill),
            row![
                checkbox(self.advert.lock().map(|a| a.enabled).unwrap_or(false))
                    .label("Capture downloads from the browser extension")
                    .on_toggle(Message::ToggleCapture)
                    .size(16)
                    .text_size(13),
                space(Length::Fill, Length::Shrink),
                button(text("Done").size(13).font(bold())).padding([7, 18]).style(theme::accent).on_press(Message::SettingsOpen(false)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(12)
        .max_width(720);
        container(body).max_height(560).into()
    }

    /// Toasts stack bottom-right, slide in from the right and fade before
    /// they expire.
    fn toast_layer(&self, p: &Palette) -> Element<'_, Message> {
        let mut col = column![].spacing(8).align_x(Alignment::End);
        for t in &self.toasts {
            let age = self.now.saturating_duration_since(t.born);
            let enter = ease(age.as_secs_f32() / TOAST_ENTER.as_secs_f32());
            let left = t.life().saturating_sub(age);
            let a = enter.min(left.as_secs_f32() / TOAST_FADE.as_secs_f32()).clamp(0.0, 1.0);
            let (icon, c) = match t.kind {
                ToastKind::Info => (Icon::Check, p.accent),
                ToastKind::Success => (Icon::Finished, p.success),
                ToastKind::Warning => (Icon::Shield, p.warning),
                ToastKind::Error => (Icon::Close, p.danger),
            };
            let icon = if t.offer.is_some() { Icon::Add } else { icon };
            let mut line = row![
                glyph(icon, 16.0, alpha(c, a)),
                text(t.text.clone()).size(12).color(alpha(p.text, a)).width(Length::Fill).wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(8)
            .align_y(Alignment::Center);
            if t.offer.is_some() {
                line = line.push(
                    button(text("Download").size(12).font(bold())).padding([4, 10]).style(theme::accent).on_press(Message::ToastAction(t.id)),
                );
            }
            line = line.push(
                button(glyph(Icon::Close, 12.0, alpha(p.muted, a))).padding(4).style(theme::flat(false)).on_press(Message::DismissToast(t.id)),
            );
            let (bg, border, shadow) = (alpha(p.card, a), alpha(c, 0.5 * a), alpha(p.shadow, a));
            let card = container(line).padding([10, 12]).width(320).style(move |_| container::Style {
                background: Some(bg.into()),
                border: iced::Border { color: border, width: 1.0, radius: 12.0.into() },
                shadow: iced::Shadow { color: shadow, offset: Vector::new(0.0, 6.0), blur_radius: 20.0 },
                ..Default::default()
            });
            let dx = 60.0 * (1.0 - enter);
            col = col.push(float(card).translate(move |_, _| Vector::new(dx, 0.0)));
        }
        container(col)
            .align_right(Length::Fill)
            .align_bottom(Length::Fill)
            .padding(iced::Padding { top: 0.0, right: 16.0, bottom: 40.0, left: 16.0 })
            .into()
    }

    fn count(&self, status: Status) -> usize {
        self.downloads.iter().filter(|e| e.snapshot.status == status).count()
    }
}

/// Ask the OS to shut down in 60 seconds, so the user can still cancel with
/// `shutdown /a` (Windows) or `shutdown -c` (Linux). Returns whether the
/// command was started.
fn power_off() -> bool {
    #[cfg(windows)]
    let result = {
        // Absolute path, as `reg()` in register.rs does: a bare name would resolve
        // against the current directory first.
        let shutdown = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join("shutdown.exe");
        std::process::Command::new(shutdown)
            .args(["/s", "/t", "60", "/c", "Turbo Download Manager finished its queue."])
            .spawn()
    };
    #[cfg(unix)]
    let result = std::process::Command::new("shutdown").args(["-h", "+1"]).spawn();
    result.is_ok()
}

/// The web and FTP links from a mirror box, one per line. Anything else on a
/// line (a stray word, a blank) is dropped rather than queued as a bad mirror.
fn parse_mirrors(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| {
            let l = l.to_ascii_lowercase();
            l.starts_with("http://") || l.starts_with("https://") || l.starts_with("ftp://")
        })
        .map(str::to_owned)
        .collect()
}

/// The command that builds the extension folders in `extension/dist`.
fn build_command() -> String {
    let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extension").join("build.mjs");
    format!("node \"{}\"", script.display())
}

/// Make `window` the foreground window on Windows.
///
/// Windows only lets the process that owns the foreground window hand it on.
/// Attaching this thread's input to the current foreground thread makes the
/// request count as coming from that thread, which is the standard way to
/// bring a background window forward. Runs on the UI thread, which owns the
/// window.
#[cfg(windows)]
fn force_foreground(window: &dyn iced::window::Window) {
    use raw_window_handle::RawWindowHandle;
    use std::ffi::c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetForegroundWindow() -> *mut c_void;
        fn GetWindowThreadProcessId(hwnd: *mut c_void, process_id: *mut u32) -> u32;
        fn GetCurrentThreadId() -> u32;
        fn AttachThreadInput(thread: u32, attach_to: u32, attach: i32) -> i32;
        fn ShowWindow(hwnd: *mut c_void, command: i32) -> i32;
        fn BringWindowToTop(hwnd: *mut c_void) -> i32;
        fn SetForegroundWindow(hwnd: *mut c_void) -> i32;
    }
    const SW_RESTORE: i32 = 9;

    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(win) = handle.as_raw() else { return };
    let hwnd = win.hwnd.get() as *mut c_void;

    // SAFETY: every call takes the window's own HWND, or thread ids that
    // come from the system. The input attachment is undone before returning.
    unsafe {
        let foreground = GetForegroundWindow();
        let foreground_thread = GetWindowThreadProcessId(foreground, std::ptr::null_mut());
        let this_thread = GetCurrentThreadId();
        let attached = foreground_thread != 0 && foreground_thread != this_thread
            && AttachThreadInput(this_thread, foreground_thread, 1) != 0;
        ShowWindow(hwnd, SW_RESTORE);
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        if attached {
            AttachThreadInput(this_thread, foreground_thread, 0);
        }
    }
}

#[cfg(not(windows))]
fn force_foreground(_window: &dyn iced::window::Window) {}

/// Bring the window up in front of whatever the user is doing, as IDM does
/// when a download starts in the browser.
///
/// Windows refuses focus changes from a background process, so `gain_focus`
/// alone only makes the taskbar button blink. Pinning the window above the
/// others for a moment is the reliable way to get it on screen. It is then
/// released so the window behaves normally again.
fn raise_window() -> Task<Message> {
    window::latest().then(|id| match id {
        Some(id) => Task::batch([
            window::minimize(id, false),
            window::set_level(id, window::Level::AlwaysOnTop),
            window::gain_focus(id),
            // The Windows call that really moves the window to the front.
            window::run(id, |w| force_foreground(w)).then(|_| Task::none()),
            window::request_user_attention(id, Some(window::UserAttention::Critical)),
            Task::perform(
                async { tokio::time::sleep(std::time::Duration::from_millis(1500)).await },
                move |_| Message::ReleaseLevel(id),
            ),
        ]),
        None => Task::none(),
    })
}

/// The size and position the main window opens with. A saved size is clamped
/// to what the window can use, and a saved position is used only when it is
/// on screen, so an unplugged monitor cannot leave the window out of reach.
fn startup_window(saved: Option<WindowGeometry>) -> (iced::Size, window::Position) {
    let Some(g) = saved else {
        return (iced::Size::new(900.0, 560.0), window::Position::Default);
    };
    let size = iced::Size::new(g.width.clamp(560.0, 4000.0), g.height.clamp(320.0, 3000.0));
    let position = match (g.x, g.y) {
        (Some(x), Some(y)) if on_screen(x) && on_screen(y) => window::Position::Specific(iced::Point::new(x, y)),
        _ => window::Position::Default,
    };
    (size, position)
}

/// A coordinate that can belong to a visible window on a sane desktop.
fn on_screen(v: f32) -> bool {
    (-500.0..=10000.0).contains(&v)
}

/// The geometry to start recording from when only one of size or position is known yet.
fn default_geometry() -> WindowGeometry {
    WindowGeometry { width: 900.0, height: 560.0, x: None, y: None }
}

/// Window events the app listens for. Everything else is dropped here, so
/// the per-frame redraw events never reach `update`.
fn window_event(event: iced::event::Event, _status: iced::event::Status, _id: window::Id) -> Option<Message> {
    use iced::event::Event;
    match event {
        Event::Window(window::Event::Resized(size)) => Some(Message::Resized(size)),
        Event::Window(window::Event::Moved(point)) => Some(Message::WindowMoved(point)),
        Event::Window(window::Event::Focused) => Some(Message::WindowFocus(true)),
        Event::Window(window::Event::Unfocused) => Some(Message::WindowFocus(false)),
        _ => None,
    }
}

/// How long a transient failure waits before retry number `retries + 1`:
/// 5 s, 10 s, 20 s and so on, capped at 5 minutes.
fn retry_delay(retries: u32) -> Duration {
    Duration::from_secs((5u64 << retries.min(16)).min(300))
}

/// The wait before an automatic retry, or `None` when the failure should stand.
fn retry_wait(transient: bool, auto_retry: bool, max: u32, retries: u32) -> Option<Duration> {
    (transient && auto_retry && retries < max.min(AUTO_RETRY_MAX)).then(|| retry_delay(retries))
}

fn tip<'a>(content: impl Into<Element<'a, Message>>, label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    tooltip(
        content,
        container(text(label).size(12)).padding([4, 8]).style(theme::card),
        tooltip::Position::Bottom,
    )
    .gap(4)
    .snap_within_viewport(true)
    .into()
}

/// A tooltip for a full value that the content shows shortened. The text
/// wraps inside a 460px box, so a long path or name cannot run off the screen.
fn tip_long<'a>(content: impl Into<Element<'a, Message>>, full: String) -> Element<'a, Message> {
    tooltip(
        content,
        container(text(full).size(12).wrapping(text::Wrapping::WordOrGlyph))
            .padding([4, 8])
            .max_width(460)
            .style(theme::card),
        tooltip::Position::Bottom,
    )
    .gap(4)
    .snap_within_viewport(true)
    .into()
}

/// The app mark in the top bar. Built once: a new handle would upload its
/// pixels to the GPU again on every frame.
fn app_mark() -> image::Handle {
    static MARK: OnceLock<image::Handle> = OnceLock::new();
    MARK.get_or_init(|| image::Handle::from_rgba(32, 32, assets::app_icon(32).to_vec())).clone()
}

fn icon_button<'a>(icon: Icon, colour: Color, msg: Message) -> button::Button<'a, Message> {
    button(glyph(icon, 16.0, colour)).padding(8).style(theme::flat(false)).on_press(msg)
}

fn bold() -> iced::Font {
    iced::Font { weight: iced::font::Weight::Semibold, ..iced::Font::DEFAULT }
}

/// A check box drawn by hand, filled in proportion to `on` (0..=1), so it can
/// fade with a row's tick animation.
fn check_box<'a>(on: f32, p: &Palette) -> Element<'a, Message> {
    let on = on.clamp(0.0, 1.0);
    let accent = p.accent;
    let edge = blend(p.muted, accent, on);
    container(glyph(Icon::Check, 14.0, alpha(p.on_accent, on)))
        .center(20)
        .style(move |_| container::Style {
            background: Some(alpha(accent, on).into()),
            border: iced::Border { color: edge, width: 1.5, radius: 6.0.into() },
            ..Default::default()
        })
        .into()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Status pill wording and tint.
fn status_badge(status: Status, p: &Palette) -> (&'static str, Color) {
    match status {
        Status::Finished => ("Done", p.success),
        Status::Failed => ("Failed", p.danger),
        Status::Paused => ("Paused", p.warning),
        Status::Running => ("Downloading", p.accent),
        Status::Queued => ("Queued", p.muted),
    }
}

/// Reduce an error to a few words that still say what went wrong. A raw
/// error can be a hundred characters and would wrap one row over several
/// lines.
fn short_error(e: &str) -> String {
    let first = e.split(" for url").next().unwrap_or(e);
    let first = first.split(" (os error").next().unwrap_or(first);
    let first = first.trim().trim_end_matches(&[':', '.'][..]);
    // Keep the most specific clause rather than the generic prefix.
    let text = first.rsplit(": ").next().unwrap_or(first);
    let mut out: String = text.chars().take(60).collect();
    if text.chars().count() > 60 {
        out.push('\u{2026}');
    }
    out
}

fn time_left(s: &Snapshot, speed: f64) -> String {
    match s.total {
        Some(total) if speed > 1.0 && total > s.done => {
            let secs = ((total - s.done) as f64 / speed) as u64;
            if secs >= 3600 {
                format!("{}h {}m left", secs / 3600, (secs % 3600) / 60)
            } else if secs >= 60 {
                format!("{}m {}s left", secs / 60, secs % 60)
            } else {
                format!("{secs}s left")
            }
        }
        _ => "\u{2014}".to_owned(),
    }
}

/// The file name a captured download is saved under.
fn capture_name(item: &Incoming) -> String {
    tdm::media::container_name(
        &item
            .filename
            .as_deref()
            .map(tdm::native_host::sanitize_filename)
            .unwrap_or_else(|| tdm::native_host::filename_from_url(&item.url)),
    )
}

/// Whether a download is an image or a video, judged by the extension of its
/// default name. Audio and other types keep the plain prompt.
fn is_media_name(name: &str) -> bool {
    const MEDIA: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "heic", "avif", "tif", "tiff", "ico",
        "mp4", "m4v", "mkv", "webm", "mov", "avi", "wmv", "flv", "3gp", "mpg", "mpeg",
    ];
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| MEDIA.contains(&e.to_ascii_lowercase().as_str()))
}

/// A file name without its extension: `clip.mkv` gives `clip`.
fn stem_of(name: &str) -> String {
    std::path::Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Why a typed file-name stem cannot be used, or `None` when it can.
fn stem_error(stem: &str) -> Option<&'static str> {
    let s = stem.trim();
    if s.is_empty() {
        Some("Enter a file name")
    } else if s == "." || s == ".." {
        Some("Not a valid file name")
    } else if s.contains(|c: char| c == '/' || c == '\\') {
        Some("A file name cannot contain / or \\")
    } else if s.len() > 200 {
        Some("The name is longer than 200 bytes")
    } else {
        None
    }
}

/// Where a captured file goes. With no edited stem it is `dir/default_name`.
/// An edited stem is cleaned, then given the extension of `default_name`, so
/// the user can never change the extension. The result is always a single
/// file name directly inside `dir`.
fn capture_target(default_name: &str, stem_edit: Option<&str>, dir: &std::path::Path) -> Result<PathBuf, &'static str> {
    let Some(stem) = stem_edit else { return Ok(dir.join(default_name)) };
    if let Some(reason) = stem_error(stem) {
        return Err(reason);
    }
    let cleaned = tdm::native_host::sanitize_filename(stem.trim());
    let name = match std::path::Path::new(default_name).extension() {
        Some(ext) => format!("{cleaned}.{}", ext.to_string_lossy()),
        None => cleaned,
    };
    let target = dir.join(&name);
    let mut parts = std::path::Path::new(&name).components();
    let single_normal = matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none();
    if !name.is_empty() && single_normal && target.parent() == Some(dir) {
        Ok(target)
    } else {
        Err("Not a valid file name")
    }
}

fn fmt_bytes(n: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    // A rate can arrive as -0.0 or a tiny negative from float arithmetic, and
    // "-0 B/s" in the status bar looks like a defect.
    let mut v = if n.is_finite() && n > 0.0 { n } else { 0.0 };
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{v:.0} {}", UNITS[u]) } else { format!("{v:.1} {}", UNITS[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_format_with_units_and_never_negative() {
        assert_eq!(fmt_bytes(-0.0), "0 B");
        assert_eq!(fmt_bytes(1536.0), "1.5 KB");
        assert_eq!(fmt_bytes(5.0 * 1024.0 * 1024.0 * 1024.0), "5.0 GB");
    }

    #[test]
    fn media_is_images_and_video_only() {
        assert!(is_media_name("a.JPG"));
        assert!(is_media_name("v.webm"));
        assert!(!is_media_name("song.mp3"));
        assert!(!is_media_name("x.zip"));
        assert!(!is_media_name("app.ts"));
        assert!(!is_media_name("noext"));
    }

    #[test]
    fn a_captured_name_keeps_the_default_extension() {
        let dir = std::path::Path::new("/d");
        assert_eq!(capture_target("x.mkv", None, dir), Ok(PathBuf::from("/d/x.mkv")));
        assert_eq!(capture_target("x.mkv", Some("clip"), dir), Ok(PathBuf::from("/d/clip.mkv")));
        // The typed text cannot change the extension: the dot is only a stem character.
        assert_eq!(capture_target("x.mkv", Some("clip.exe"), dir), Ok(PathBuf::from("/d/clip.exe.mkv")));
    }

    #[test]
    fn invalid_stems_are_refused() {
        let dir = std::path::Path::new("/d");
        let long = "x".repeat(201);
        for bad in ["", "   ", "a/b", "a\\b", ".", "..", "../x", long.as_str()] {
            assert!(capture_target("x.mkv", Some(bad), dir).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_captured_target_always_sits_directly_in_the_folder() {
        let dir = std::path::Path::new("/d");
        for stem in ["clip", "a.b", "<<<", "CON", "  spaced  ", "x:y"] {
            let p = capture_target("x.mp4", Some(stem), dir).unwrap();
            assert_eq!(p.parent(), Some(dir), "{stem:?}");
        }
    }

    #[test]
    fn popping_a_prompt_forgets_its_edits() {
        let mut app = test_app();
        app.prompts.push_back(Incoming { url: "https://e.test/a.mp4".into(), filename: None, headers: Vec::new(), size: None });
        app.prompt_name = Some("clip".into());
        app.prompt_dir = Some(PathBuf::from("/d"));
        let seq = app.prompt_seq;
        assert!(app.pop_prompt().is_some());
        assert!(app.prompt_name.is_none() && app.prompt_dir.is_none());
        assert_eq!(app.prompt_seq, seq + 1);
    }

    #[test]
    fn a_claimed_destination_gets_the_next_free_number() {
        let taken = ["/d/a.zip", "/d/a (2).zip", "/d/noext"].map(PathBuf::from);
        let is_taken = |p: &std::path::Path| taken.iter().any(|t| t == p);
        assert_eq!(unique_dest(PathBuf::from("/d/b.zip"), is_taken), PathBuf::from("/d/b.zip"));
        assert_eq!(unique_dest(PathBuf::from("/d/a.zip"), is_taken), PathBuf::from("/d/a (3).zip"));
        assert_eq!(unique_dest(PathBuf::from("/d/noext"), is_taken), PathBuf::from("/d/noext (2)"));
    }

    #[test]
    fn a_renamed_file_skips_names_already_taken() {
        let taken = ["/d/photo.png", "/d/photo (2).png"].map(PathBuf::from);
        let is_taken = |p: &std::path::Path| taken.iter().any(|t| t == p);
        let target = PathBuf::from("/d").join(filetype::fixed_name("photo.jpg", Kind::Png));
        assert_eq!(target, PathBuf::from("/d/photo.png"));
        assert_eq!(unique_dest(target, is_taken), PathBuf::from("/d/photo (3).png"));
    }

    #[test]
    fn rename_refuses_to_overwrite_and_moves_a_free_name() {
        let dir = std::env::temp_dir().join(format!("tdm-rename-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let from = dir.join("a.jpg");
        let taken = dir.join("a.png");
        std::fs::write(&from, b"x").unwrap();
        std::fs::write(&taken, b"y").unwrap();
        assert!(rename_finished(&from, &taken).is_err(), "an existing file is not replaced");
        assert!(from.exists() && taken.exists());

        let free = dir.join("b.png");
        assert_eq!(rename_finished(&from, &free).ok(), Some(free.clone()));
        assert!(!from.exists() && free.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rename_takes_the_next_free_name_when_one_is_claimed() {
        let dir = std::env::temp_dir().join(format!("tdm-rename-free-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let from = dir.join("c.jpg");
        let want = dir.join("c.png");
        std::fs::write(&from, b"x").unwrap();
        // Another download already owns `c.png` in the list.
        let others = vec![want.clone()];
        let done = rename_to_free_name(&from, &want, &others).unwrap();
        assert_eq!(done, dir.join("c (2).png"));
        assert!(done.exists() && !from.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pause_everything_cancels_a_waiting_retry() {
        let mut app = test_app();
        let ids = add_paused(&mut app, 1);
        let ix = app.index_of(ids[0]).unwrap();
        // Failed with a retry due now, as a transient failure would leave it.
        app.downloads[ix].snapshot.status = Status::Failed;
        app.downloads[ix].retries = 1;
        app.downloads[ix].retry_at = Some(Instant::now());
        let _ = app.run_command(Cmd::PauseAll);
        let _ = app.tick();
        let e = &app.downloads[ix];
        assert_eq!(e.snapshot.status, Status::Failed, "a paused-everything retry must not requeue");
        assert!(e.retry_at.is_none());
    }

    #[test]
    fn a_web_page_or_a_mismatch_opens_the_type_prompt_once() {
        let mut app = test_app();
        let page = app.push(
            DownloadRecord {
                url: "https://example.com/page".into(),
                dest: PathBuf::from("/nonexistent-tdm-test/page.png"),
                category_id: None,
                headers: Vec::new(),
                mirrors: Vec::new(),
                total: None,
                complete: false,
                kind: None,
                content_type: None,
            },
            Status::Finished,
        );
        let ix = app.index_of(page).unwrap();
        app.downloads[ix].snapshot.kind = Some(Kind::Html);
        app.check_file_type(ix);
        let prompt = app.type_prompt.as_ref().expect("a web page opens the dialog");
        assert_eq!(prompt.verdict, Verdict::WebPage);
        assert_eq!(app.toasts.len(), 1);

        // A second case while the dialog is open adds a toast and leaves the first dialog in place.
        let other = app.push(
            DownloadRecord {
                url: "https://example.com/archive".into(),
                dest: PathBuf::from("/nonexistent-tdm-test/archive.zip"),
                category_id: None,
                headers: Vec::new(),
                mirrors: Vec::new(),
                total: None,
                complete: false,
                kind: None,
                content_type: None,
            },
            Status::Finished,
        );
        let ix = app.index_of(other).unwrap();
        app.downloads[ix].snapshot.kind = Some(Kind::Png);
        app.check_file_type(ix);
        assert_eq!(app.toasts.len(), 2);
        assert_eq!(app.type_prompt.as_ref().map(|t| t.id), Some(page));
    }

    #[test]
    fn errors_shorten_to_their_specific_clause() {
        assert_eq!(short_error("error sending request for url (https://x/): connection refused"), "error sending request");
        assert_eq!(short_error("io: Access is denied. (os error 5)"), "Access is denied");
    }

    /// An App without the window, the capture server or a saved list, for
    /// driving `update` directly.
    fn test_app() -> App {
        let settings = Settings::default();
        let now = Instant::now();
        let limiter = Arc::new(Limiter::new(settings.speed_limit));
        let advert = Arc::new(Mutex::new(Advert {
            capture_extensions: settings.capture_list(),
            exception_hosts: settings.exception_hosts.clone(),
            enabled: true,
        }));
        App {
            url: String::new(),
            search: String::new(),
            downloads: Vec::new(),
            next_id: 1,
            selected: None,
            marked: BTreeSet::new(),
            select_mode: false,
            select_anim: Animation::new(false).duration(SELECT_FADE).easing(Easing::EaseOut),
            bar_anim: Animation::new(false).duration(SELECT_FADE).easing(Easing::EaseOut),
            ctrl_down: false,
            bulk_delete_files: false,
            renaming: BTreeSet::new(),
            mark_flash: None,
            row_menu: None,
            filter: Filter::All,
            browsers: Vec::new(),
            browser_guide: false,
            grab: None,
            prompts: VecDeque::new(),
            prompt_name: None,
            prompt_dir: None,
            prompt_seq: 0,
            queue_busy: false,
            window_width: 900.0,
            open_menu: None,
            settings_open: false,
            settings,
            store: Store::at(std::env::temp_dir().join("tdm-ui-tests")),
            limiter,
            inbox: Arc::new(Mutex::new(Vec::new())),
            advert,
            capture_ready: false,
            confirm_delete: None,
            type_prompt: None,
            batch: None,
            verify: None,
            toasts: Vec::new(),
            next_toast: 1,
            last_clipboard: None,
            spark: VecDeque::new(),
            now,
            epoch: now,
            theme_anim: Animation::new(false),
            overlay_anim: Animation::new(true),
            focused: true,
            pending_names: BTreeSet::new(),
            window_geometry: None,
        }
    }

    fn add_paused(app: &mut App, n: usize) -> Vec<u64> {
        (0..n)
            .map(|i| {
                app.push(
                    DownloadRecord {
                        url: format!("https://example.com/{i}.bin"),
                        dest: PathBuf::from(format!("/nonexistent-tdm-test/{i}.bin")),
                        category_id: None,
                        headers: Vec::new(),
                        mirrors: Vec::new(),
                        total: None,
                        complete: false,
                        kind: None,
                        content_type: None,
                    },
                    Status::Paused,
                )
            })
            .collect()
    }

    #[test]
    fn one_download_keeps_the_plain_dialog() {
        assert_eq!(confirm_tier(1, 0, false), Tier::Simple);
        assert_eq!(confirm_tier(1, 1, true), Tier::Simple);
    }

    #[test]
    fn a_few_idle_downloads_are_counted_without_typing() {
        assert_eq!(confirm_tier(2, 0, false), Tier::Counted);
        assert_eq!(confirm_tier(9, 0, false), Tier::Counted);
    }

    #[test]
    fn ten_or_more_downloads_need_the_count_typed() {
        assert_eq!(confirm_tier(10, 0, false), Tier::Typed);
    }

    #[test]
    fn a_running_download_in_the_batch_needs_the_count_typed() {
        assert_eq!(confirm_tier(2, 1, false), Tier::Typed);
    }

    #[test]
    fn deleting_files_from_disk_needs_the_count_typed_even_for_two() {
        assert_eq!(confirm_tier(2, 0, true), Tier::Typed);
    }

    #[test]
    fn the_typed_count_must_match_exactly() {
        assert!(typed_ok(12, "12"));
        assert!(typed_ok(12, " 12 "));
        assert!(!typed_ok(12, "1"));
        assert!(!typed_ok(12, "121"));
        assert!(!typed_ok(12, ""));
    }

    #[test]
    fn a_removed_download_leaves_no_mark_behind() {
        let mut app = test_app();
        let ids = add_paused(&mut app, 3);
        let _ = app.update(Message::Mark(ids[0]));
        let _ = app.update(Message::Mark(ids[1]));
        app.downloads.retain(|e| e.id != ids[0]);
        app.prune_selection();
        assert_eq!(app.marked.iter().copied().collect::<Vec<_>>(), vec![ids[1]]);
    }

    #[test]
    fn changing_the_filter_clears_the_marks() {
        let mut app = test_app();
        let ids = add_paused(&mut app, 2);
        let _ = app.update(Message::MarkAllVisible(true));
        assert_eq!(app.marked.len(), ids.len());
        let _ = app.update(Message::SelectFilter(Filter::Finished));
        assert!(app.marked.is_empty());
    }

    #[test]
    fn deleting_one_download_removes_it() {
        let mut app = test_app();
        let ids = add_paused(&mut app, 2);
        let _ = app.update(Message::Remove(ids[0]));
        assert_eq!(app.confirm_delete.as_ref().map(|c| c.ids.clone()), Some(vec![ids[0]]));
        let _ = app.update(Message::ConfirmDelete);
        assert!(app.confirm_delete.is_none());
        assert_eq!(app.downloads.iter().map(|e| e.id).collect::<Vec<_>>(), vec![ids[1]]);
    }

    #[test]
    fn a_batch_delete_waits_for_the_typed_count() {
        let mut app = test_app();
        add_paused(&mut app, 10);
        let _ = app.update(Message::MarkAllVisible(true));
        let _ = app.update(Message::DeleteMarked);
        let _ = app.update(Message::ConfirmDelete);
        assert_eq!(app.downloads.len(), 10, "nothing is removed before the count is typed");
        let _ = app.update(Message::ConfirmTyped("10".into()));
        let _ = app.update(Message::ConfirmDelete);
        assert!(app.downloads.is_empty());
        assert!(app.marked.is_empty());
        assert!(!app.select_mode);
    }

    #[test]
    fn a_resume_with_no_free_slot_stays_queued_through_a_tick() {
        let mut app = test_app();
        let ids = add_paused(&mut app, 1);
        app.settings.max_active = 0;
        let ix = app.index_of(ids[0]).unwrap();
        // Ran once before, so the tick would adopt the handle's snapshot.
        app.downloads[ix].started = true;
        app.downloads[ix].snapshot.total = Some(100);
        let _ = app.update(Message::TogglePause(ids[0]));
        let _ = app.tick();
        let snap = &app.downloads[ix].snapshot;
        assert_eq!(snap.status, Status::Queued, "no slot is free, so the resume must stand");
        assert_eq!(snap.total, Some(100), "the handle's snapshot was not adopted");
    }

    #[test]
    fn retry_waits_double_and_stop_at_five_minutes() {
        let secs = |n| retry_delay(n).as_secs();
        assert_eq!([secs(0), secs(1), secs(2), secs(3)], [5, 10, 20, 40]);
        assert_eq!(secs(6), 300);
        assert_eq!(secs(40), 300);
    }

    #[test]
    fn only_transient_failures_within_the_budget_retry() {
        // 404 and friends are never transient, whatever the budget.
        assert_eq!(retry_wait(false, true, 3, 0), None);
        // Turned off in the settings.
        assert_eq!(retry_wait(true, false, 3, 0), None);
        // Budget used up: three retries allowed, so the fourth attempt stands.
        assert!(retry_wait(true, true, 3, 2).is_some());
        assert_eq!(retry_wait(true, true, 3, 3), None);
        // A hand-edited budget above the cap is clamped to ten.
        assert!(retry_wait(true, true, 500, 9).is_some());
        assert_eq!(retry_wait(true, true, 500, 10), None);
    }

    #[test]
    fn saved_window_is_clamped_and_only_placed_when_on_screen() {
        let (size, pos) = startup_window(Some(WindowGeometry { width: 100.0, height: 9000.0, x: Some(-20000.0), y: Some(5.0) }));
        assert_eq!((size.width, size.height), (560.0, 3000.0));
        assert!(matches!(pos, window::Position::Default), "an off-screen x must not be used");

        let (_, pos) = startup_window(Some(WindowGeometry { width: 900.0, height: 600.0, x: Some(40.0), y: Some(60.0) }));
        assert!(matches!(pos, window::Position::Specific(p) if p == iced::Point::new(40.0, 60.0)));

        let (size, pos) = startup_window(None);
        assert_eq!((size.width, size.height), (900.0, 560.0));
        assert!(matches!(pos, window::Position::Default));
    }

    #[test]
    fn arrow_keys_and_home_end_walk_the_visible_list() {
        let mut app = test_app();
        let ids: Vec<u64> = (0..3)
            .map(|n| {
                app.push(
                    DownloadRecord {
                        url: format!("https://example.com/{n}.bin"),
                        dest: PathBuf::from(format!("/nonexistent-tdm-test/{n}.bin")),
                        category_id: None,
                        headers: Vec::new(),
                        mirrors: Vec::new(),
                        total: None,
                        complete: false,
                        kind: None,
                        content_type: None,
                    },
                    Status::Queued,
                )
            })
            .collect();
        let _ = app.move_selection(1);
        assert_eq!(app.selected, Some(ids[0]), "first press selects the first row");
        let _ = app.move_selection(1);
        assert_eq!(app.selected, Some(ids[1]));
        let _ = app.move_selection(5);
        assert_eq!(app.selected, Some(ids[2]), "stops at the last row");
        let _ = app.move_selection(-9);
        assert_eq!(app.selected, Some(ids[0]), "stops at the first row");
        let _ = app.select_end(false);
        assert_eq!(app.selected, Some(ids[2]), "End goes to the last row");
        let _ = app.select_end(true);
        assert_eq!(app.selected, Some(ids[0]), "Home goes to the first row");
    }

    #[test]
    fn a_row_press_depends_on_select_mode_and_ctrl() {
        assert_eq!(row_press(true, false), RowAction::Mark);
        assert_eq!(row_press(true, true), RowAction::Mark);
        assert_eq!(row_press(false, true), RowAction::Toggle);
        assert_eq!(row_press(false, false), RowAction::Select);
    }

    #[test]
    fn ctrl_click_toggles_membership_without_select_mode() {
        let mut app = test_app();
        let ids = add_paused(&mut app, 2);
        app.ctrl_down = true;
        let _ = app.update(Message::RowPress(ids[0]));
        assert!(app.marked.contains(&ids[0]));
        assert!(!app.select_mode, "Ctrl+click does not enter select mode");
        assert_eq!(app.selected, Some(ids[0]));
        let _ = app.update(Message::RowPress(ids[0]));
        assert!(app.marked.is_empty(), "a second Ctrl+click unticks it");
        assert!(!app.select_mode);
    }

    #[test]
    fn a_plain_click_selects_and_leaves_marks_alone() {
        let mut app = test_app();
        let ids = add_paused(&mut app, 2);
        let _ = app.update(Message::RowPress(ids[0]));
        let _ = app.update(Message::RowPress(ids[1]));
        assert_eq!(app.selected, Some(ids[1]));
        assert!(app.marked.is_empty());
    }

    #[test]
    fn the_bar_shows_for_marks_made_outside_select_mode() {
        let mut app = test_app();
        let ids = add_paused(&mut app, 1);
        assert!(!app.bar_shown());
        app.ctrl_down = true;
        let _ = app.update(Message::RowPress(ids[0]));
        assert!(app.bar_shown());
        assert!(app.bar_anim.value(), "the bar is sliding in");
    }

    #[test]
    fn the_bulk_tick_reaches_the_dialog_and_remove_does_not() {
        let mut app = test_app();
        let ids = add_paused(&mut app, 2);
        let _ = app.update(Message::MarkAllVisible(true));
        let _ = app.update(Message::BulkDeleteFiles(true));
        let _ = app.update(Message::DeleteMarked);
        assert_eq!(app.confirm_delete.as_ref().map(|c| c.also_delete_file), Some(true));
        let _ = app.update(Message::CancelDelete);
        let _ = app.update(Message::Remove(ids[0]));
        assert_eq!(app.confirm_delete.as_ref().map(|c| c.also_delete_file), Some(false));
    }

    #[test]
    fn clearing_the_marks_resets_the_bulk_tick() {
        let mut app = test_app();
        add_paused(&mut app, 2);
        let _ = app.update(Message::MarkAllVisible(true));
        let _ = app.update(Message::BulkDeleteFiles(true));
        let _ = app.update(Message::ClearMarks);
        assert!(!app.bulk_delete_files);
    }

    #[test]
    fn changing_the_search_clears_the_marks() {
        let mut app = test_app();
        add_paused(&mut app, 2);
        let _ = app.update(Message::MarkAllVisible(true));
        let _ = app.update(Message::SearchChanged("0".into()));
        assert!(app.marked.is_empty());
    }

    #[test]
    fn a_download_being_renamed_is_not_deleted() {
        let mut app = test_app();
        let ids = add_paused(&mut app, 2);
        app.renaming.insert(ids[0]);
        let _ = app.update(Message::Remove(ids[0]));
        assert!(app.confirm_delete.is_none(), "the dialog does not open for it");
        assert!(app.toasts.iter().any(|t| t.text.contains("rename")));
    }
}
