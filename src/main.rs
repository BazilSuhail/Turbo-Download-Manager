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

use std::collections::VecDeque;
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
use tdm::ipc::{Advert, Incoming, Inbox};
use tdm::model::{OptionsTab, Settings};
use tdm::store::{DownloadList, DownloadRecord, Store};
use tdm::ui::assets::{self, Icon};
use tdm::ui::menu::{Cmd, Item, MENU_BAR};
use tdm::ui::theme::{self, alpha, Palette};
use tdm::{Download, Snapshot, Status, next_to_start};

const TICK: Duration = Duration::from_millis(500);
/// Running downloads only need the progress shimmer, which reads fine at a
/// fraction of the display rate.
const SHIMMER_TICK: Duration = Duration::from_millis(50);
const TOAST_LIFE: Duration = Duration::from_millis(4500);
/// A toast that offers an action stays long enough to be acted on.
const TOAST_ACTION_LIFE: Duration = Duration::from_secs(9);
const TOAST_FADE: Duration = Duration::from_millis(400);
const SPARK_LEN: usize = 48;
/// A Hello this recent means the browser's extension is talking to TDM.
const CONNECTED_WITHIN: Duration = Duration::from_secs(10 * 60);
const URL_INPUT: &str = "url";
const SEARCH_INPUT: &str = "search";

/// Left-hand tree selection: everything, one category, or a status folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    All,
    Category(u32),
    Unfinished,
    Finished,
}

#[derive(Debug, Clone)]
enum Message {
    UrlChanged(String),
    AddUrl,
    SearchChanged(String),
    Tick,
    /// A display frame (or a shimmer tick): advance animations.
    Frame(Instant),
    MenuOpen(Option<usize>),
    Command(Cmd),
    Select(u64),
    SelectFilter(Filter),
    TogglePause(u64),
    Remove(u64),
    /// Show or hide one row's action strip.
    RowMenu(Option<u64>),
    OpenFile(u64),
    ShowInFolder(u64),
    CopyLink(u64),
    Verify(u64),
    VerifyExpected(String),
    VerifyDone(u64, Result<String, String>),
    CloseVerify,
    BatchEdit(text_editor::Action),
    BatchAdd,
    BatchClose,
    SettingsOpen(bool),
    ToggleCapture(bool),
    Finished(u64, Result<(), String>),
    Discarded(u64, Result<(), String>),
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
    CaptureStart,
    CaptureQueue,
    CaptureCancel,
    AddToBrowser(tdm::register::Browser),
    ToastAction(u64),
    DismissToast(u64),
    Key(keyboard::Event),
}

/// A pending delete, awaiting confirmation.
struct ConfirmDelete {
    id: u64,
    /// Opt-in: the list entry goes either way, the bytes on disk only if this
    /// is ticked.
    also_delete_file: bool,
}

struct Batch {
    content: text_editor::Content,
    /// The expansion of `content`, kept current on every edit so the view
    /// does not re-expand ten thousand links per frame.
    links: Result<Vec<String>, batch::Error>,
}

struct Verify {
    id: u64,
    name: String,
    expected: String,
    /// `None` while hashing.
    digest: Option<Result<String, String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToastKind {
    Info,
    Success,
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
}

struct App {
    url: String,
    search: String,
    downloads: Vec<Entry>,
    next_id: u64,
    selected: Option<u64>,
    row_menu: Option<u64>,
    filter: Filter,
    /// Browsers installed when the app started; listed in the sidebar.
    browsers: Vec<tdm::register::Browser>,
    /// The add-TDM-to-your-browser page.
    browser_guide: bool,
    /// Browser downloads waiting for Download now or Add to queue.
    prompts: VecDeque<Incoming>,
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
}

fn main() -> iced::Result {
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

    iced::application(App::boot, App::update, App::view)
        .title("Turbo Download Manager")
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window::Settings {
            size: iced::Size::new(900.0, 560.0),
            min_size: Some(iced::Size::new(560.0, 320.0)),
            icon: window::icon::from_rgba(assets::app_icon(64), 64, 64).ok(),
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
    let stem = dest.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = dest.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2u64..)
        .map(|n| dest.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|p| !taken(p))
        .expect("some number is free")
}

/// Copy a state file that failed to parse aside, before the next save
/// replaces it with this session's defaults.
fn keep_aside(path: &std::path::Path) {
    let mut bad = path.as_os_str().to_owned();
    bad.push(".corrupt");
    if let Err(e) = std::fs::copy(path, &bad) {
        eprintln!("could not keep a copy of {}: {e}", path.display());
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
                eprintln!("settings could not be loaded ({e}); using defaults for this session");
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
                eprintln!("browser capture unavailable: {e}");
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
            row_menu: None,
            filter: Filter::All,
            browsers: tdm::register::Browser::detected(),
            browser_guide: false,
            prompts: VecDeque::new(),
            open_menu: None,
            settings_open: false,
            theme_anim: Animation::new(settings.light_theme)
                .duration(Duration::from_millis(350))
                .easing(Easing::EaseInOut),
            overlay_anim: Animation::new(true),
            settings,
            store,
            limiter,
            inbox,
            advert,
            capture_ready,
            confirm_delete: None,
            batch: None,
            verify: None,
            toasts: Vec::new(),
            next_toast: 1,
            last_clipboard: None,
            spark: VecDeque::with_capacity(SPARK_LEN),
            now,
            epoch: now,
        };
        match app.store.load_list() {
            Ok(list) => {
                for rec in list.downloads {
                    app.push(rec, Status::Paused);
                }
            }
            // The next save would otherwise replace the whole list with an empty one.
            Err(e) => {
                eprintln!("the download list could not be loaded ({e})");
                keep_aside(&app.store.list_path());
            }
        }
        // Idempotent, and slow enough (registry, files) to keep off the UI
        // thread. Failure only means the browser cannot reach us yet.
        std::thread::spawn(|| {
            if let Err(e) = tdm::register::install_default() {
                eprintln!("browser registration failed: {e}");
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
        let mut snapshot = handle.snapshot();
        snapshot.status = if rec.complete { Status::Finished } else { status };
        snapshot.total = rec.total;
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
        });
        id
    }

    /// Add a link typed, pasted, expanded from a batch or offered by a toast.
    fn add_link(&mut self, url: String) {
        // A manifest URL saved under its own name would leave the user with a
        // .m3u8 text file instead of a video.
        let name = tdm::media::container_name(&tdm::native_host::filename_from_url(&url));
        let dest = self.free_dest(self.settings.destination_for(&name));
        let category_id = self.settings.category_for(&name).map(|c| c.id);
        let status = self.added_status();
        self.push(
            DownloadRecord { url, dest, category_id, headers: Vec::new(), total: None, complete: false },
            status,
        );
    }

    /// `dest`, or a numbered sibling when a file, a resume sidecar or another
    /// download already claims it. Writing into a claimed path would
    /// overwrite the user's file, or have two downloads share one.
    fn free_dest(&self, dest: PathBuf) -> PathBuf {
        unique_dest(dest, |p| {
            let mut sidecar = p.as_os_str().to_owned();
            sidecar.push(".tdmstate");
            p.exists()
                || std::path::Path::new(&sidecar).exists()
                || self.downloads.iter().any(|e| e.handle.dest() == p)
        })
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
                    total: e.snapshot.total,
                    complete: e.snapshot.status == Status::Finished,
                })
                .collect(),
        };
        if let Err(e) = self.store.save_list(&list) {
            eprintln!("could not save the download list: {e}");
        }
    }

    fn save_settings(&self) {
        if let Err(e) = self.store.save_settings(&self.settings) {
            eprintln!("could not save settings: {e}");
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
                async move { handle.run().await.map_err(|e| e.to_string()) },
                move |r| Message::Finished(id, r),
            ));
        }
        Task::batch(tasks)
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

    fn any_overlay(&self) -> bool {
        self.confirm_delete.is_some()
            || self.settings_open
            || self.batch.is_some()
            || self.verify.is_some()
            || self.browser_guide
            || !self.prompts.is_empty()
    }

    /// Something is mid-transition and needs every frame.
    fn animating(&self) -> bool {
        self.theme_anim.is_animating(self.now)
            || self.overlay_anim.is_animating(self.now)
            || !self.toasts.is_empty()
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
        self.now = Instant::now();
        match message {
            Message::UrlChanged(url) => {
                self.url = url;
                Task::none()
            }
            Message::SearchChanged(s) => {
                self.search = s;
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
                Task::none()
            }
            Message::TogglePause(id) => {
                let Some(ix) = self.index_of(id) else { return Task::none() };
                match self.downloads[ix].snapshot.status {
                    Status::Running => {
                        self.downloads[ix].handle.pause();
                        Task::none()
                    }
                    Status::Paused | Status::Failed => {
                        self.downloads[ix].snapshot.status = Status::Queued;
                        self.downloads[ix].snapshot.error = None;
                        self.start_ready()
                    }
                    _ => Task::none(),
                }
            }
            Message::Remove(id) => {
                // Deleting bytes off the user's disk is not something to do on
                // a single click, and the file stays unless the user opts in.
                if self.index_of(id).is_some() {
                    self.confirm_delete = Some(ConfirmDelete { id, also_delete_file: false });
                    self.open_overlay();
                }
                Task::none()
            }
            Message::ConfirmDeleteToggleFile(on) => {
                if let Some(c) = &mut self.confirm_delete {
                    c.also_delete_file = on;
                }
                Task::none()
            }
            Message::CancelDelete => {
                self.confirm_delete = None;
                Task::none()
            }
            Message::ConfirmDelete => {
                let Some(confirm) = self.confirm_delete.take() else { return Task::none() };
                let Some(ix) = self.index_of(confirm.id) else { return Task::none() };
                let entry = self.downloads.remove(ix);
                entry.handle.pause();
                if self.selected == Some(confirm.id) {
                    self.selected = None;
                }
                self.save();
                if !confirm.also_delete_file {
                    return Task::none();
                }
                let (id, handle) = (confirm.id, entry.handle);
                // Not `discard_partial`: that races the download's own worker,
                // which still holds the file open. `stop_and_discard` waits for
                // it to let go first.
                Task::perform(
                    async move { handle.stop_and_discard().await.map_err(|e| e.to_string()) },
                    move |r| Message::Discarded(id, r),
                )
            }
            Message::Discarded(id, result) => {
                if let Err(e) = result {
                    eprintln!("could not delete the partial download {id}: {e}");
                    self.toast(ToastKind::Error, format!("Could not delete the file: {}", short_error(&e)));
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
                        match tokio::task::spawn_blocking(move || tdm::checksum::sha256_file(&path)).await {
                            Ok(Ok(d)) => Ok(d),
                            Ok(Err(e)) => Err(e.to_string()),
                            Err(e) => Err(e.to_string()),
                        }
                    },
                    move |r| Message::VerifyDone(id, r),
                )
            }
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
            Message::BatchAdd => {
                let Some(Batch { links: Ok(links), .. }) = self.batch.take() else { return Task::none() };
                let n = links.len();
                for url in links {
                    self.add_link(url);
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
            Message::Options(m) => {
                let was_autostart = self.settings.general.launch_on_startup;
                tdm::ui::options::update(&mut self.settings, m);
                if self.settings.general.launch_on_startup != was_autostart
                    && let Err(e) = tdm::register::set_launch_at_login(self.settings.general.launch_on_startup)
                {
                    eprintln!("could not change launch at login: {e}");
                }
                self.sync_theme();
                // Settings the running engine can honour immediately.
                self.limiter.set(self.settings.speed_limit);
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
                self.start_ready()
            }
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
                if let Some(ix) = self.index_of(id) {
                    let e = &mut self.downloads[ix];
                    e.snapshot = e.handle.snapshot();
                    if let Err(err) = result {
                        e.snapshot.error = Some(err);
                    }
                    e.speed = 0.0;
                    let (status, name, error) = (e.snapshot.status, e.name.clone(), e.snapshot.error.clone());
                    match (status, error) {
                        (Status::Finished, _) if self.settings.downloads.show_complete_dialog => {
                            self.toast(ToastKind::Success, format!("Finished: {name}"));
                        }
                        (Status::Failed, Some(err)) => {
                            self.toast(ToastKind::Error, format!("{name}: {}", short_error(&err)));
                        }
                        _ => {}
                    }
                }
                self.save();
                self.start_ready()
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
                        self.toast_with(ToastKind::Info, format!("Download copied link? {name}"), Some(link.to_owned()));
                    }
                }
                Task::none()
            }
            Message::Clipboard(None) => Task::none(),
            Message::PasteUrl(clip) => {
                if let Some(clip) = clip.map(|c| c.trim().to_owned()).filter(|c| !c.is_empty()) {
                    self.url = clip;
                }
                iced::widget::operation::focus(URL_INPUT)
            }
            Message::CaptureStart => self.accept_capture(true),
            Message::CaptureQueue => self.accept_capture(false),
            Message::CaptureCancel => {
                self.prompts.pop_front();
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
            Message::AddToBrowser(browser) => {
                let dir = browser.extension_dir();
                if !dir.join("manifest.json").is_file() {
                    self.toast(ToastKind::Error, "TDM's extension files are missing from this install. Reinstall TDM.");
                    return Task::none();
                }
                if let Err(e) = tdm::register::set_up() {
                    self.toast(ToastKind::Error, format!("Could not register TDM: {e}"));
                    return Task::none();
                }
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
        let Some(item) = self.prompts.pop_front() else { return Task::none() };
        let name = capture_name(&item);
        let dest = self.free_dest(self.settings.destination_for(&name));
        let category_id = self.settings.category_for(&name).map(|c| c.id);
        let rec = DownloadRecord { url: item.url, dest, category_id, headers: item.headers, total: item.size, complete: false };
        self.push(rec, if start { Status::Queued } else { Status::Paused });
        self.save();
        if start { self.start_ready() } else { Task::none() }
    }

    fn tick(&mut self) -> Task<Message> {
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
            // Only adopt the engine's snapshot once this entry has actually
            // been run in this session. A restored entry's handle has never
            // run, so its snapshot is the default — Queued — and copying that
            // over a restored Finished or Paused row would hand it straight to
            // `start_ready`, re-downloading a completed file from byte 0 over
            // itself.
            if entry.started {
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
        let keyboard::Event::KeyPressed { key, modifiers, .. } = event else { return Task::none() };
        if let Key::Named(Named::Escape) = key {
            // Close only the topmost thing; `||` stops at the first one taken.
            let closed = self.open_menu.take().is_some()
                || self.row_menu.take().is_some()
                || self.verify.take().is_some()
                || self.batch.take().is_some()
                || self.confirm_delete.take().is_some();
            if !closed && !self.prompts.is_empty() {
                return self.update(Message::CaptureCancel);
            }
            if !closed && self.browser_guide {
                return self.update(Message::CloseBrowserGuide);
            }
            if !closed && self.settings_open {
                return self.update(Message::SettingsOpen(false));
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
            Key::Character("v") if modifiers.command() && modifiers.shift() => self.run_command(Cmd::PasteUrl),
            Key::Named(Named::Delete) => self.run_command(Cmd::Delete),
            Key::Named(Named::Space) => match self.selected {
                Some(id) => self.update(Message::TogglePause(id)),
                None => Task::none(),
            },
            _ => Task::none(),
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        if let Err(e) = open::that_detached(&path) {
            self.toast(ToastKind::Error, format!("Could not open {}: {}", path.display(), short_error(&e.to_string())));
        }
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
                self.batch = Some(Batch { content: text_editor::Content::new(), links: Ok(Vec::new()) });
                self.open_overlay();
                Task::none()
            }
            Cmd::Exit => self.shutdown(),
            Cmd::PauseAll => {
                for e in &self.downloads {
                    e.handle.pause();
                }
                Task::none()
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
            Cmd::OpenFile => sel.map_or(Task::none(), |id| self.update(Message::OpenFile(id))),
            Cmd::ShowInFolder => sel.map_or(Task::none(), |id| self.update(Message::ShowInFolder(id))),
            Cmd::VerifySha256 => match sel {
                Some(id) if self.selected_status() == Some(Status::Finished) => {
                    self.update(Message::Verify(id))
                }
                _ => Task::none(),
            },
            Cmd::Options => self.update(Message::SettingsOpen(true)),
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
                    }
                }
                self.start_ready()
            }
            Cmd::StopQueue => {
                for e in &mut self.downloads {
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
                Cmd::OpenFile | Cmd::VerifySha256 => status == Some(Status::Finished),
                Cmd::ShowInFolder => status.is_some(),
                Cmd::DeleteCompleted => any(Status::Finished),
                Cmd::StartQueue => any(Status::Paused) || any(Status::Failed) || any(Status::Queued),
                Cmd::StopQueue | Cmd::PauseAll => any(Status::Running) || any(Status::Queued),
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
    fn shutdown(&mut self) -> Task<Message> {
        for entry in &self.downloads {
            entry.handle.pause();
        }
        self.save();
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
        let dialog = if !self.prompts.is_empty() {
            Some((self.capture_dialog(&p), Message::CaptureCancel))
        } else if self.verify.is_some() {
            Some((self.verify_dialog(&p), Message::CloseVerify))
        } else if self.batch.is_some() {
            Some((self.batch_dialog(&p), Message::BatchClose))
        } else if self.confirm_delete.is_some() {
            Some((self.delete_dialog(&p), Message::CancelDelete))
        } else if self.settings_open {
            Some((self.options_dialog(), Message::SettingsOpen(false)))
        } else if self.browser_guide {
            Some((self.browser_dialog(&p), Message::CloseBrowserGuide))
        } else {
            None
        };
        if let Some((content, dismiss)) = dialog {
            layers = layers.push(self.modal(content, dismiss));
        }
        if !self.toasts.is_empty() {
            layers = layers.push(self.toast_layer(&p));
        }
        layers.into()
    }

    fn header(&self, p: &Palette) -> Element<'_, Message> {
        let mark = image(image::Handle::from_rgba(32, 32, assets::app_icon(32))).width(20).height(20);
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
                    .style(theme::bare_input)
                    .size(13)
                    .padding([6, 4]),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        )
        .padding([0, 10])
        .max_width(260)
        .style(move |t: &Theme| container::Style {
            border: iced::Border { color: Palette::of(t).border, width: 1.0, radius: 99.0.into() },
            background: Some(Palette::of(t).card.into()),
            ..Default::default()
        });
        let toolbar = row![
            scrollable(tools).direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            )),
            space(Length::Fixed(8.0), Length::Shrink),
            search,
        ]
        .align_y(Alignment::Center);

        let link_bar = row![
            text_input("Paste a link and press Enter", &self.url)
                .id(URL_INPUT)
                .on_input(Message::UrlChanged)
                .on_submit(Message::AddUrl)
                .style(theme::input)
                .size(13)
                .padding([8, 12]),
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
                    let mut line = row![text(*label).size(13).color(colour)].spacing(12);
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
        let panel = container(col).width(250).style(theme::card);
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
            button(
                row![
                    space(Length::Fixed(indent), Length::Shrink),
                    glyph(icon, 16.0, if selected { p.accent } else { p.muted }),
                    text(label).size(13).width(Length::Fill).wrapping(text::Wrapping::None),
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
                        text(b.label()).size(13).width(Length::Fill).wrapping(text::Wrapping::None),
                        container(text(status).size(11).color(tint)).padding([1, 7]).style(theme::pill(tint)),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center)
                    .padding([3, 8]),
                );
            }
        }

        container(scrollable(col).height(Length::Fill))
            .width(186)
            .height(Length::Fill)
            .padding([4, 8])
            .style(theme::surface)
            .into()
    }

    fn download_list(&self, p: &Palette) -> Element<'_, Message> {
        let visible = self.visible();
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
        container(content).width(Length::Fill).height(Length::Fill).style(theme::chrome).into()
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

        let icon = e.category_id.map_or(Icon::File, |id| assets::category_icon(Some(id)));
        let badge = container(glyph(icon, 18.0, p.accent))
            .center(36)
            .style(move |t: &Theme| container::Style {
                background: Some(alpha(Palette::of(t).accent, 0.12).into()),
                border: iced::Border { radius: 10.0.into(), ..Default::default() },
                ..Default::default()
            });

        let mut detail = match s.total {
            Some(t) if s.status != Status::Finished => format!("{} of {}", fmt_bytes(s.done as f64), fmt_bytes(t as f64)),
            Some(t) => fmt_bytes(t as f64),
            None if s.done > 0 => fmt_bytes(s.done as f64),
            None => "size unknown".to_owned(),
        };
        if s.total.is_some() && s.status != Status::Finished {
            detail.push_str(&format!("  ·  {:.0}%", frac * 100.0));
        }
        if running && e.speed > 0.0 {
            detail.push_str(&format!("  ·  {}/s  ·  {}", fmt_bytes(e.speed), time_left(s, e.speed)));
        }
        if let (Status::Failed, Some(err)) = (s.status, &s.error) {
            detail = short_error(err);
        }

        let shimmer = running.then(|| (self.now.saturating_duration_since(self.epoch).as_secs_f32() / 1.8).fract());
        let bar_colour = match s.status {
            Status::Finished => Some(p.success),
            Status::Failed => Some(p.danger),
            Status::Paused | Status::Queued => Some(alpha(p.muted, 0.7)),
            Status::Running => None,
        };
        let bar = progress_bar(0.0..=1.0, frac).girth(6).style(theme::progress(bar_colour, shimmer));

        let info = column![
            row![
                text(e.name.clone()).size(13).font(bold()).width(Length::Fill).wrapping(text::Wrapping::None),
                container(text(label).size(11).color(p.text)).padding([2, 9]).style(theme::pill(tint)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            bar,
            text(detail).size(11).color(if s.status == Status::Failed { p.danger } else { p.muted }).wrapping(text::Wrapping::None),
        ]
        .spacing(5)
        .width(Length::Fill);

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

        let line = row![badge, info, toggle, more].spacing(10).align_y(Alignment::Center);
        let mut card = column![line].spacing(8);
        if self.row_menu == Some(e.id) {
            card = card.push(self.row_actions(e, p));
        }

        let row_button = button(card)
            .width(Length::Fill)
            .padding([10, 12])
            .style(theme::row_button(selected))
            .on_press(Message::Select(e.id));
        mouse_area(row_button)
            .on_right_press(Message::RowMenu(Some(e.id)))
            .on_double_click(if s.status == Status::Finished { Message::OpenFile(e.id) } else { Message::TogglePause(e.id) })
            .into()
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
        if !self.capture_ready {
            bar = bar.push(text("browser capture unavailable").size(12).color(p.danger));
        }
        bar = bar
            .push(spark)
            .push(container(text(format!("{}/s", fmt_bytes(total))).size(12).font(bold())).width(80).align_right(80));
        container(bar).padding([5, 12]).width(Length::Fill).style(theme::surface).into()
    }

    // --------------------------------------------------------------- overlays

    /// A dimmed backdrop that fades in, with the dialog settling from a
    /// slight zoom. Clicking the backdrop dismisses.
    fn modal<'a>(&self, content: Element<'a, Message>, dismiss: Message) -> Element<'a, Message> {
        let t = ease(self.overlay_anim.interpolate(0.0, 1.0, self.now));
        let backdrop = mouse_area(
            container(space(Length::Fill, Length::Fill)).style(move |_| container::Style {
                background: Some(Color { a: 0.5 * t, ..Color::BLACK }.into()),
                ..Default::default()
            }),
        )
        .on_press(dismiss);
        let card = container(content).padding(20).style(theme::card);
        let card = float(card).scale(1.0 + 0.04 * (1.0 - t));
        stack![
            opaque(backdrop),
            container(opaque(card)).center(Length::Fill).padding(16),
        ]
        .into()
    }

    /// Two wordings: a finished download offers to remove the file from disk;
    /// an unfinished one offers to drop its partial data.
    fn delete_dialog(&self, p: &Palette) -> Element<'_, Message> {
        let Some(confirm) = &self.confirm_delete else { return space(Length::Shrink, Length::Shrink).into() };
        let Some(entry) = self.entry(confirm.id) else { return space(Length::Shrink, Length::Shrink).into() };
        let finished = entry.snapshot.status == Status::Finished;
        column![
            row![glyph(Icon::Delete, 20.0, p.danger), text("Delete download").size(16).font(bold())].spacing(8).align_y(Alignment::Center),
            text(format!("Remove \u{201c}{}\u{201d} from the list?", entry.name)).size(13),
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
        .into()
    }

    fn batch_dialog(&self, p: &Palette) -> Element<'_, Message> {
        let Some(b) = &self.batch else { return space(Length::Shrink, Length::Shrink).into() };
        let (summary, colour, ready) = match &b.links {
            Ok(v) if v.is_empty() => ("One link per line. Use [001-120] or [a-z] for ranges.".to_owned(), p.muted, false),
            Ok(v) if v.len() == 1 => (format!("1 link: {}", v[0]), p.muted, true),
            Ok(v) => (format!("{} links: {}  \u{2026}  {}", v.len(), v[0], v[v.len() - 1]), p.muted, true),
            Err(e) => (e.to_string(), p.danger, false),
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
                .height(140)
                .size(13)
                .padding(10),
            text(summary).size(12).color(colour),
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
                text("Computing SHA-256\u{2026}").size(12).color(p.muted),
                progress_bar(0.0..=1.0, 1.0)
                    .girth(4)
                    .style(theme::progress(None, Some((self.now.saturating_duration_since(self.epoch).as_secs_f32() / 1.2).fract()))),
            ]
            .spacing(6)
            .into(),
            Some(Ok(d)) => text(d.clone()).size(12).font(iced::Font::MONOSPACE).into(),
            Some(Err(e)) => text(format!("Could not read the file: {e}")).size(12).color(p.danger).into(),
        };
        let verdict: Element<'_, Message> = match &v.digest {
            Some(Ok(d)) if !v.expected.trim().is_empty() => {
                let ok = tdm::checksum::matches(d, &v.expected);
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
            row![glyph(Icon::Shield, 20.0, p.accent), text("Verify SHA-256").size(16).font(bold())].spacing(8).align_y(Alignment::Center),
            text(v.name.clone()).size(13).color(p.muted),
            digest,
            text_input("Paste the published SHA-256 to compare", &v.expected)
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
    /// The prompt for a download the browser handed over, like IDM's: name,
    /// size and folder, then Download now or Add to queue.
    fn capture_dialog(&self, p: &Palette) -> Element<'_, Message> {
        let Some(item) = self.prompts.front() else { return space(Length::Shrink, Length::Shrink).into() };
        let name = capture_name(item);
        let size = item.size.map_or("unknown size".to_owned(), |s| fmt_bytes(s as f64));
        let folder = self.settings.destination_for(&name).parent().map(|d| d.display().to_string()).unwrap_or_default();
        let more = self.prompts.len() - 1;
        column![
            row![glyph(Icon::Add, 20.0, p.accent), text("Download file").size(16).font(bold())]
                .spacing(8)
                .align_y(Alignment::Center),
            text(name).size(15).font(bold()),
            text(item.url.clone()).size(11).color(p.muted),
            row![
                text(format!("Size: {size}")).size(12).color(p.muted),
                text(format!("Save to: {folder}")).size(12).color(p.muted),
            ]
            .spacing(16),
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
            list = list.push(
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
            );
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
            let enter = ease(age.as_secs_f32() / 0.25);
            let left = t.life().saturating_sub(age);
            let a = enter.min(left.as_secs_f32() / TOAST_FADE.as_secs_f32()).clamp(0.0, 1.0);
            let (icon, c) = match t.kind {
                ToastKind::Info => (Icon::Check, p.accent),
                ToastKind::Success => (Icon::Finished, p.success),
                ToastKind::Error => (Icon::Close, p.danger),
            };
            let icon = if t.offer.is_some() { Icon::Add } else { icon };
            let mut line = row![
                glyph(icon, 16.0, alpha(c, a)),
                text(t.text.clone()).size(12).color(alpha(p.text, a)).width(Length::Fill),
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

/// Un-minimise, raise and focus the window; also flash it in case the OS
/// refuses the focus change.
fn raise_window() -> Task<Message> {
    window::latest().then(|id| match id {
        Some(id) => Task::batch([
            window::minimize(id, false),
            window::gain_focus(id),
            window::request_user_attention(id, Some(window::UserAttention::Informational)),
        ]),
        None => Task::none(),
    })
}

fn tip<'a>(content: impl Into<Element<'a, Message>>, label: &'a str) -> Element<'a, Message> {
    tooltip(
        content,
        container(text(label).size(12)).padding([4, 8]).style(theme::card),
        tooltip::Position::Bottom,
    )
    .gap(4)
    .into()
}

fn icon_button<'a>(icon: Icon, colour: Color, msg: Message) -> button::Button<'a, Message> {
    button(glyph(icon, 16.0, colour)).padding(8).style(theme::flat(false)).on_press(msg)
}

fn bold() -> iced::Font {
    iced::Font { weight: iced::font::Weight::Semibold, ..iced::Font::DEFAULT }
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
    fn a_claimed_destination_gets_the_next_free_number() {
        let taken = ["/d/a.zip", "/d/a (2).zip", "/d/noext"].map(PathBuf::from);
        let is_taken = |p: &std::path::Path| taken.iter().any(|t| t == p);
        assert_eq!(unique_dest(PathBuf::from("/d/b.zip"), is_taken), PathBuf::from("/d/b.zip"));
        assert_eq!(unique_dest(PathBuf::from("/d/a.zip"), is_taken), PathBuf::from("/d/a (3).zip"));
        assert_eq!(unique_dest(PathBuf::from("/d/noext"), is_taken), PathBuf::from("/d/noext (2)"));
    }

    #[test]
    fn errors_shorten_to_their_specific_clause() {
        assert_eq!(short_error("error sending request for url (https://x/): connection refused"), "error sending request");
        assert_eq!(short_error("io: Access is denied. (os error 5)"), "Access is denied");
    }
}
