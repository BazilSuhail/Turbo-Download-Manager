//! The Preferences dialog.
//!
//! Two rules this module keeps:
//!
//! * Nothing renders as live unless changing it changes behaviour. Pages whose
//!   feature does not exist yet (site logins, dial-up, sounds) are drawn
//!   disabled and labelled `(not yet implemented)`. A control that looks live
//!   and silently does nothing is worse than one that admits it.
//! * Personal values (paths, proxy host, credentials, User-Agent) are neutral
//!   in `Settings::default()` — see the test there.
//!
//! The view is a pure function of `Settings`, including which tab is open, so
//! the shell needs no extra state: one message variant and one `update` call.

use iced::widget::{button, checkbox, column, container, pick_list, row, scrollable, text, text_input, Space};
use iced::{Element, Length};

use crate::model::{
    self, DuplicateAction, OptionsTab, ProxyMode, Settings,
};
use crate::scheduler::{TimeOfDay, Weekday};
use crate::ui::theme;

/// Suffix for a control we render but cannot honour yet.
const NOT_WIRED: &str = "(not yet implemented)";
const LABEL_W: f32 = 170.0;

#[derive(Debug, Clone)]
pub enum Message {
    SelectTab(OptionsTab),

    // General
    LaunchOnStartup(bool),
    MonitorClipboard(bool),
    LightTheme(bool),
    BringToFront(bool),
    CaptureBrowser(usize, bool),

    // File types
    CaptureExtensions(String),
    ResetCaptureExtensions,
    ExceptionHosts(String),
    ResetExceptionHosts,

    // Save to
    DownloadDir(String),
    /// Open the native folder picker (handled by the app).
    BrowseDownloadDir,
    TempDir(String),
    RememberLastPath(bool),
    UseServerFileDate(bool),

    // Downloads
    ShowStartDialog(bool),
    QueueOnly(bool),
    ShowCompleteDialog(bool),
    StartWhileShowingInfo(bool),
    QueuePanelOnDownloadLater(bool),
    QueuePanelOnBatchClose(bool),
    IgnoreModificationTime(bool),
    CheckFileType(bool),
    FlashOnComplete(bool),
    Duplicate(DuplicateAction),
    UserAgent(String),

    // Connection
    MaxConnections(u32),
    LimitEnabled(bool),
    LimitMbytes(u32),
    LimitHours(u32),
    WarnBeforeStopping(bool),
    Retries(u32),
    TimeoutSecs(u32),

    /// What happens when the queue runs out of work.
    AfterQueueChoice(model::AfterQueue),

    // Speed windows (time-of-day caps)
    AddSpeedWindow,
    RemoveSpeedWindow(usize),
    SpeedWindowStart(usize, u32),
    SpeedWindowEnd(usize, u32),
    /// KB/s; 0 means unlimited inside the window.
    SpeedWindowKbps(usize, u32),

    // Proxy / Socks
    ProxyMode(ProxyMode),
    PacUrl(String),
    ProxyHost(String),
    /// Free text: a non-number leaves the port unchanged rather than panicking.
    ProxyPort(String),
    ProxyUser(String),
    ProxyPassword(String),
    ProxyUseHttp(bool),
    ProxyUseHttps(bool),
    ProxyUseFtp(bool),
    /// Copy the user's Windows proxy into the fields above.
    ProxyGetSystem,
    BrowserProxyOnError(bool),

    // Categories
    /// Space-separated extensions for the category at this index.
    CategoryMask(usize, String),
    CategoryPath(usize, String),
    CategoryRemove(usize),
    DraftCategoryName(String),
    DraftCategoryMask(String),
    CategoryAdd,

    // Queue schedule
    ScheduleEnabled(bool),
    /// Free text: `HH:MM`, or empty for "no automatic start/stop". Anything
    /// else leaves the value alone rather than guessing.
    ScheduleStart(String),
    ScheduleStop(String),
    ScheduleDay(Weekday, bool),
}

/// Apply one change. Numeric settings are clamped to the ranges in `model`, so
/// no message can push the engine outside them.
pub fn update(s: &mut Settings, message: Message) {
    use Message::*;
    match message {
        SelectTab(t) => s.options_tab = t,

        LaunchOnStartup(v) => s.general.launch_on_startup = v,
        MonitorClipboard(v) => s.general.monitor_clipboard = v,
        LightTheme(v) => s.light_theme = v,
        BringToFront(v) => s.bring_to_front_on_capture = v,
        CaptureBrowser(i, v) => {
            if let Some(b) = s.general.capture_browsers.get_mut(i) {
                b.1 = v;
            }
        }

        CaptureExtensions(v) => s.capture_extensions = words(&v),
        ResetCaptureExtensions => {
            s.capture_extensions = Settings::default().capture_extensions
        }
        ExceptionHosts(v) => s.exception_hosts = words(&v),
        ResetExceptionHosts => s.exception_hosts = Settings::default().exception_hosts,

        DownloadDir(v) => s.download_dir = v.into(),
        // The app opens the folder picker; nothing changes here.
        BrowseDownloadDir => {}
        TempDir(v) => s.save_to.temp_dir = v.into(),
        RememberLastPath(v) => s.save_to.remember_last_path = v,
        UseServerFileDate(v) => s.save_to.use_server_file_date = v,

        ShowStartDialog(v) => s.downloads.show_start_dialog = v,
        QueueOnly(v) => s.downloads.queue_only = v,
        ShowCompleteDialog(v) => s.downloads.show_complete_dialog = v,
        StartWhileShowingInfo(v) => s.downloads.start_while_showing_info = v,
        QueuePanelOnDownloadLater(v) => s.downloads.queue_panel_on_download_later = v,
        QueuePanelOnBatchClose(v) => s.downloads.queue_panel_on_batch_close = v,
        IgnoreModificationTime(v) => s.downloads.ignore_modification_time = v,
        CheckFileType(v) => s.downloads.check_file_type = v,
        FlashOnComplete(v) => s.downloads.flash_on_complete = v,
        Duplicate(v) => s.downloads.duplicate_action = v,
        UserAgent(v) => s.downloads.user_agent = v,

        MaxConnections(v) => s.max_segments = clamp(v, model::CONNECTIONS),
        LimitEnabled(v) => s.connection.limit_enabled = v,
        LimitMbytes(v) => s.connection.limit_mbytes = clamp(v, model::LIMIT_MBYTES),
        LimitHours(v) => s.connection.limit_hours = clamp(v, model::LIMIT_HOURS),
        WarnBeforeStopping(v) => s.connection.warn_before_stopping = v,
        Retries(v) => s.connection.retries = clamp(v, model::RETRIES),
        TimeoutSecs(v) => s.connection.timeout_secs = clamp(v, model::TIMEOUT_SECS),

        AfterQueueChoice(v) => s.after_queue = v,

        AddSpeedWindow => s.speed_windows.push(model::SpeedWindow {
            start: TimeOfDay { hour: 22, minute: 0 },
            end: TimeOfDay { hour: 7, minute: 0 },
            limit: Some(512 * 1024),
        }),
        RemoveSpeedWindow(i) => {
            if i < s.speed_windows.len() {
                s.speed_windows.remove(i);
            }
        }
        SpeedWindowStart(i, h) => {
            if let Some(w) = s.speed_windows.get_mut(i) {
                w.start = TimeOfDay { hour: clamp(h, 0..=23) as u8, minute: 0 };
            }
        }
        SpeedWindowEnd(i, h) => {
            if let Some(w) = s.speed_windows.get_mut(i) {
                w.end = TimeOfDay { hour: clamp(h, 0..=23) as u8, minute: 0 };
            }
        }
        SpeedWindowKbps(i, k) => {
            if let Some(w) = s.speed_windows.get_mut(i) {
                let k = clamp(k, 0..=SPEED_KBPS_MAX);
                w.limit = (k > 0).then(|| u64::from(k) * 1024);
            }
        }

        ProxyMode(v) => s.proxy.mode = v,
        PacUrl(v) => s.proxy.pac_url = v,
        ProxyHost(v) => s.proxy.host = v,
        ProxyPort(raw) => {
            if let Some(p) = parse_port(&raw) {
                s.proxy.port = p;
            }
        }
        ProxyUser(v) => s.proxy.user = v,
        ProxyPassword(v) => s.proxy.password = v,
        ProxyUseHttp(v) => s.proxy.use_http = v,
        ProxyUseHttps(v) => s.proxy.use_https = v,
        ProxyUseFtp(v) => s.proxy.use_ftp = v,
        ProxyGetSystem => {
            if let Some((host, port)) = model::system_proxy_endpoint() {
                s.proxy.host = host;
                s.proxy.port = port;
                s.proxy.use_http = true;
                s.proxy.use_https = true;
                s.proxy.mode = model::ProxyMode::Manual;
            }
        }
        CategoryMask(i, raw) => {
            if let Some(c) = s.categories.get_mut(i) {
                c.mask = split_mask(&raw);
            }
        }
        CategoryPath(i, raw) => {
            if let Some(c) = s.categories.get_mut(i) {
                c.path = raw.into();
            }
        }
        CategoryRemove(i) => {
            if i < s.categories.len() {
                s.categories.remove(i);
            }
        }
        DraftCategoryName(v) => s.draft_category.0 = v,
        DraftCategoryMask(v) => s.draft_category.1 = v,
        CategoryAdd => {
            let (name, mask) = std::mem::take(&mut s.draft_category);
            let name = name.trim().to_owned();
            if name.is_empty() {
                // Put the mask back rather than discard what was typed.
                s.draft_category.1 = mask;
                return;
            }
            // Ids only have to be unique; the highest in use plus one is.
            let id = s.categories.iter().map(|c| c.id).max().unwrap_or(0) + 1;
            let path = s.download_dir.join(&name);
            s.categories.push(model::Category { id, name, mask: split_mask(&mask), path });
        }

        ScheduleEnabled(v) => s.schedule.enabled = v,
        ScheduleStart(raw) => {
            if let Some(t) = parse_time(&raw) {
                s.schedule.start = t;
            }
        }
        ScheduleStop(raw) => {
            if let Some(t) = parse_time(&raw) {
                s.schedule.stop = t;
            }
        }
        ScheduleDay(day, on) => s.schedule.days = s.schedule.days.with(day, on),

        BrowserProxyOnError(v) => s.proxy.use_browser_proxy_on_error = v,
    }
}

fn clamp(v: u32, range: std::ops::RangeInclusive<u32>) -> u32 {
    v.clamp(*range.start(), *range.end())
}

/// Extensions as typed: space- or comma-separated, lowercase, no dots — the
/// form `Category::matches` expects.
fn split_mask(raw: &str) -> Vec<String> {
    raw.split([' ', ',', ';', '\t'])
        .map(|p| p.trim().trim_start_matches('*').trim_start_matches('.'))
        .filter(|p| !p.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// A time typed by hand. `Some(None)` clears it, `None` means "not a time
/// yet" so a half-typed `2` does not reset the field.
pub fn parse_time(raw: &str) -> Option<Option<TimeOfDay>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(None);
    }
    raw.parse::<TimeOfDay>().ok().map(Some)
}

/// A port typed by hand. `None` for anything that is not a port number, so the
/// caller keeps the previous value instead of unwrapping a parse error. An
/// empty field reads as 0, which is how "unset" is stored.
pub fn parse_port(raw: &str) -> Option<u16> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(0);
    }
    raw.parse().ok()
}

fn words(v: &str) -> Vec<String> {
    v.split_whitespace().map(str::to_owned).collect()
}

/// The whole dialog: tab list on the left, the selected page on the right.
pub fn view(s: &Settings) -> Element<'_, Message> {
    let page = match s.options_tab {
        OptionsTab::General => general(s),
        OptionsTab::FileTypes => file_types(s),
        OptionsTab::SaveTo => save_to(s),
        OptionsTab::Downloads => downloads(s),
        OptionsTab::Connection => connection(s),
        OptionsTab::Proxy => proxy(s),
        OptionsTab::SitesLogins => sites_logins(),
        OptionsTab::DialUp => dial_up(),
        OptionsTab::Sounds => sounds(),
    };

    row![
        container(scrollable(tab_list(s.options_tab)))
            .width(Length::Fixed(130.0))
            .height(Length::Fill),
        // The gap on the right is the scroll bar's lane: without it the bar is
        // drawn over the last control in a row, and a half-covered button is
        // one the user cannot reliably hit.
        container(
            scrollable(container(page).padding(iced::Padding::default().right(14.0)))
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .padding(iced::Padding::default().left(16.0)),
    ]
    .into()
}

fn tab_list(current: OptionsTab) -> Element<'static, Message> {
    let mut col = column![].spacing(2).padding(iced::Padding::default().right(8.0));
    for tab in TABS {
        col = col.push(
            button(text(tab_title(tab)).size(13))
                .width(Length::Fill)
                .padding([7, 12])
                .style(theme::side_button(tab == current))
                .on_press(Message::SelectTab(tab)),
        );
    }
    col.into()
}

const TABS: [OptionsTab; 9] = [
    OptionsTab::General,
    OptionsTab::FileTypes,
    OptionsTab::SaveTo,
    OptionsTab::Downloads,
    OptionsTab::Connection,
    OptionsTab::Proxy,
    OptionsTab::SitesLogins,
    OptionsTab::DialUp,
    OptionsTab::Sounds,
];

pub fn tab_title(tab: OptionsTab) -> &'static str {
    match tab {
        OptionsTab::General => "General",
        OptionsTab::FileTypes => "Capture",
        OptionsTab::SaveTo => "Folders",
        OptionsTab::Downloads => "Downloads",
        OptionsTab::Connection => "Connection",
        OptionsTab::Proxy => "Proxy",
        OptionsTab::SitesLogins => "Site logins",
        OptionsTab::DialUp => "Dial-up / VPN",
        OptionsTab::Sounds => "Sounds",
    }
}

/// The two themes, as the selector lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThemeName(bool);

impl std::fmt::Display for ThemeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0 { "TDM Light" } else { "TDM Dark" })
    }
}

// ---------------------------------------------------------------- the pages

fn general(s: &Settings) -> Element<'_, Message> {
    let mut col = page("Appearance");
    col = col.push(
        row![
            text("Theme").size(13).width(Length::Fixed(LABEL_W)),
            pick_list([ThemeName(false), ThemeName(true)], Some(ThemeName(s.light_theme)), |t| {
                Message::LightTheme(t.0)
            })
            .text_size(13)
            .padding(6),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    col = col.push(space(8.0));
    col = col.push(text("Browser & system").size(15));
    col = col.push(check(
        "Bring the window to the front when the browser hands over a download",
        s.bring_to_front_on_capture,
        Message::BringToFront,
    ));
    col = col.push(check(
        "Watch the clipboard and offer to download copied links",
        s.general.monitor_clipboard,
        Message::MonitorClipboard,
    ));
    col = col.push(check(
        "Start TDM when I sign in to Windows",
        s.general.launch_on_startup,
        Message::LaunchOnStartup,
    ));
    col = col.push(space(8.0));
    col = col.push(text("Take over downloads from these browsers:").size(13));
    for (i, (name, on)) in s.general.capture_browsers.iter().enumerate() {
        col = col.push(
            container(check(name, *on, move |v| Message::CaptureBrowser(i, v)))
                .padding(iced::Padding::default().left(16.0)),
        );
    }
    col.into()
}

fn file_types(s: &Settings) -> Element<'_, Message> {
    let mut col = page("What the browser hands over");
    col = col.push(text("Take over downloads of these file types:").size(13));
    col = col.push(
        text_input("ZIP RAR EXE ...", &s.capture_extensions.join(" "))
            .on_input(Message::CaptureExtensions)
            .style(theme::input)
            .padding(6),
    );
    col = col.push(reset_row(Message::ResetCaptureExtensions));
    col = col.push(space(8.0));
    col = col.push(text("Never take over downloads from these sites:").size(13));
    col = col.push(
        text_input("*.example.com  download.example.com", &s.exception_hosts.join(" "))
            .on_input(Message::ExceptionHosts)
            .style(theme::input)
            .padding(6),
    );
    col = col.push(note("Separate names with spaces."));
    col = col.push(reset_row(Message::ResetExceptionHosts));
    col.into()
}

fn save_to(s: &Settings) -> Element<'_, Message> {
    let mut col = page("Folders & categories");
    col = col.push(field(
        "Default download folder",
        &s.download_dir.to_string_lossy(),
        "",
        Message::DownloadDir,
    ));
    col = col.push(
        row![
            text("Pick the download folder with the native dialog").size(13).style(theme::muted).width(Length::Fill),
            button(text("Browse").size(13)).padding([4, 12]).style(theme::flat(false)).on_press(Message::BrowseDownloadDir),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    col = col.push(check(
        "Remember the last folder I saved to",
        s.save_to.remember_last_path,
        Message::RememberLastPath,
    ));
    col = col.push(check(
        "Use the server's date for downloaded files",
        s.save_to.use_server_file_date,
        Message::UseServerFileDate,
    ));
    col = col.push(space(8.0));
    col = col.push(field(
        "Temporary folder",
        &s.save_to.temp_dir.to_string_lossy(),
        "(empty: keep partial data beside the file)",
        Message::TempDir,
    ));
    col = col.push(dead_button("Pick the temporary folder", "Browse"));
    col = col.push(note(
        "A temporary folder on a different drive turns every finished download \
         into a cross-drive copy.",
    ));
    col = col.push(space(8.0));
    col = col.push(text("Categories").size(15));
    col = col.push(note(
        "A download goes to the first category whose extensions match its file \
         name, and is saved in that category's folder.",
    ));
    for (i, c) in s.categories.iter().enumerate() {
        col = col.push(
            row![
                container(
                    text(crate::ui::fit::middle_ellipsis(&c.name, 12))
                        .size(12)
                        .wrapping(iced::widget::text::Wrapping::None)
                        .width(Length::Fixed(90.0)),
                )
                .width(Length::Fixed(90.0))
                .clip(true),
                text_input("ZIP RAR", &c.mask.join(" "))
                    .on_input(move |v| Message::CategoryMask(i, v))
                    .style(theme::input)
                    .size(12)
                    .padding(4)
                    .width(Length::FillPortion(2)),
                text_input("folder", &c.path.to_string_lossy())
                    .on_input(move |v| Message::CategoryPath(i, v))
                    .style(theme::input)
                    .size(12)
                    .padding(4)
                    .width(Length::FillPortion(3)),
                button(text("Remove").size(12))
                    .padding([4, 8])
                    .style(theme::flat(false))
                    .on_press(Message::CategoryRemove(i)),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        );
    }
    let (ref draft_name, ref draft_mask) = s.draft_category;
    col = col.push(
        row![
            text_input("New category", draft_name)
                .on_input(Message::DraftCategoryName)
                .style(theme::input)
                .size(12)
                .padding(4)
                .width(Length::Fixed(90.0)),
            text_input("PSD AI", draft_mask)
                .on_input(Message::DraftCategoryMask)
                .style(theme::input)
                .size(12)
                .padding(4)
                .width(Length::FillPortion(2)),
            text(if draft_name.trim().is_empty() {
                "a name is required"
            } else {
                "saved under the default folder"
            })
            .size(11)
            .style(theme::muted)
            .width(Length::FillPortion(3)),
            {
                let mut add = button(text("Add").size(12)).padding([4, 8]).style(theme::flat(false));
                if !draft_name.trim().is_empty() {
                    add = add.on_press(Message::CategoryAdd);
                }
                add
            },
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
    );
    col.into()
}

fn downloads(s: &Settings) -> Element<'_, Message> {
    let d = &s.downloads;
    let mut col = page("New downloads");
    col = col.push(check(
        "Confirm each new download before it starts",
        d.show_start_dialog,
        Message::ShowStartDialog,
    ));
    // Queue-only is a choice made in that confirmation, so it only applies
    // while the confirmation is on.
    let queue_only_label = "Only add them to the queue; don't start them";
    col = col.push(
        container(if d.show_start_dialog {
            check(queue_only_label, d.queue_only, Message::QueueOnly)
        } else {
            dead_check(queue_only_label, d.queue_only)
        })
        .padding(iced::Padding::default().left(16.0)),
    );
    col = col.push(check(
        "Show a notice when a download finishes",
        d.show_complete_dialog,
        Message::ShowCompleteDialog,
    ));
    col = col.push(check(
        "Start downloading while the file details are still shown",
        d.start_while_showing_info,
        Message::StartWhileShowingInfo,
    ));
    col = col.push(check(
        "Ask which queue to use for downloads started later",
        d.queue_panel_on_download_later,
        Message::QueuePanelOnDownloadLater,
    ));
    col = col.push(check(
        "Ask which queue to use after adding a batch",
        d.queue_panel_on_batch_close,
        Message::QueuePanelOnBatchClose,
    ));
    col = col.push(check(
        "Resume even if the server reports a newer file date",
        d.ignore_modification_time,
        Message::IgnoreModificationTime,
    ));
    col = col.push(check(
        "Check the file's real type after download",
        d.check_file_type,
        Message::CheckFileType,
    ));
    col = col.push(check(
        "Flash the taskbar when a download finishes",
        d.flash_on_complete,
        Message::FlashOnComplete,
    ));
    col = col.push(
        row![
            text("When a link is already in the list:").size(13).width(Length::Fixed(LABEL_W)),
            pick_list(&DuplicateAction::ALL[..], Some(d.duplicate_action), Message::Duplicate)
                .text_size(13)
                .padding(6),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    col = col.push(field(
        "User-Agent for links added by hand",
        &d.user_agent,
        "(empty: use the engine's own)",
        Message::UserAgent,
    ));

    // One queue, so its schedule lives with the other queue settings.
    col = col.push(space(8.0));
    col = col.push(text("Queue schedule").size(15));
    let sc = &s.schedule;
    col = col.push(check(
        "Run the queue only inside a time window",
        sc.enabled,
        Message::ScheduleEnabled,
    ));
    col = col.push(field(
        "Start at (HH:MM, empty for none)",
        &sc.start.map(|t| t.to_string()).unwrap_or_default(),
        "23:00",
        Message::ScheduleStart,
    ));
    col = col.push(field(
        "Stop at (HH:MM, empty for none)",
        &sc.stop.map(|t| t.to_string()).unwrap_or_default(),
        "07:30",
        Message::ScheduleStop,
    ));
    // Seven buttons do not fit beside a 250 px label, so the label goes above.
    col = col.push(text("On these days").size(13));
    let mut days = row![].spacing(4);
    for (day, label) in WEEKDAYS {
        let on = sc.days.contains(day);
        days = days.push(
            button(text(label).size(12))
                .padding([4, 8])
                .style(theme::flat(on))
                .on_press(Message::ScheduleDay(day, !on)),
        );
    }
    col = col.push(days.align_y(iced::Alignment::Center));
    col = col.push(note(
        "Outside the window the queue stops and nothing starts, including a \
         download resumed by hand. Progress is saved, so the next window \
         continues rather than restarts.",
    ));
    col.into()
}

/// Sunday first, matching the day mask's bit order.
const WEEKDAYS: [(Weekday, &str); 7] = [
    (Weekday::Sunday, "Sun"),
    (Weekday::Monday, "Mon"),
    (Weekday::Tuesday, "Tue"),
    (Weekday::Wednesday, "Wed"),
    (Weekday::Thursday, "Thu"),
    (Weekday::Friday, "Fri"),
    (Weekday::Saturday, "Sat"),
];

fn connection(s: &Settings) -> Element<'_, Message> {
    let c = &s.connection;
    let mut col = page("Connections & limits");
    col = col.push(stepper_row(
        "Connections per download",
        s.max_segments,
        model::CONNECTIONS,
        Message::MaxConnections,
    ));
    col = col.push(stepper_row("Retries per segment", c.retries, model::RETRIES, Message::Retries));
    col = col.push(stepper_row(
        "Connection timeout (seconds)",
        c.timeout_secs,
        model::TIMEOUT_SECS,
        Message::TimeoutSecs,
    ));
    col = col.push(space(8.0));
    col = col.push(check("Data cap", c.limit_enabled, Message::LimitEnabled));
    let limits = column![
        stepper_row("At most (MB)", c.limit_mbytes, model::LIMIT_MBYTES, Message::LimitMbytes),
        stepper_row("per (hours)", c.limit_hours, model::LIMIT_HOURS, Message::LimitHours),
        check(
            "Warn before the cap pauses downloads",
            c.warn_before_stopping,
            Message::WarnBeforeStopping,
        ),
    ]
    .spacing(8);
    // The cap's details only mean something while the cap is on.
    col = col.push(
        container(if c.limit_enabled {
            Element::from(limits)
        } else {
            column![
                dead_label(&format!("At most {} MB", c.limit_mbytes)),
                dead_label(&format!("per {} hours", c.limit_hours)),
                dead_check("Warn before the cap pauses downloads", c.warn_before_stopping),
            ]
            .spacing(8)
            .into()
        })
        .padding(iced::Padding::default().left(16.0)),
    );

    // Time-of-day caps. Each row is one window; the first open window wins.
    col = col.push(space(8.0));
    col = col.push(
        row![
            text("When the queue is done").size(13).width(Length::Fixed(LABEL_W)),
            pick_list(&model::AfterQueue::ALL[..], Some(s.after_queue), Message::AfterQueueChoice)
                .text_size(13)
                .padding(6),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    col = col.push(space(8.0));
    col = col.push(text("Speed windows").size(13));
    col = col.push(note(
        "While a window is open its limit replaces the global one. A window whose start is \
         later than its end runs past midnight. 0 KB/s means unlimited inside the window.",
    ));
    for (i, w) in s.speed_windows.iter().enumerate() {
        col = col.push(speed_window_row(i, w));
    }
    col = col.push(
        button(text("Add speed window").size(13))
            .padding([4, 12])
            .style(theme::flat(false))
            .on_press(Message::AddSpeedWindow),
    );
    col.into()
}

/// The largest per-window cap offered in the UI, in KB/s (about 1 GB/s).
const SPEED_KBPS_MAX: u32 = 1_000_000;

fn speed_window_row(index: usize, w: &model::SpeedWindow) -> Element<'static, Message> {
    let kbps = w.limit.map_or(0, |b| (b / 1024) as u32);
    column![
        row![
            mini_stepper("From hour", u32::from(w.start.hour), 0..=23, move |h| Message::SpeedWindowStart(index, h)),
            mini_stepper("To hour", u32::from(w.end.hour), 0..=23, move |h| Message::SpeedWindowEnd(index, h)),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
        row![
            mini_stepper("KB/s", kbps, 0..=SPEED_KBPS_MAX, move |k| Message::SpeedWindowKbps(index, k)),
            button(text("Remove").size(12))
                .padding([3, 10])
                .style(theme::flat(false))
                .on_press(Message::RemoveSpeedWindow(index)),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    ]
    .spacing(4)
    .into()
}

/// A labelled `-` / value / `+` control whose change message is built by a
/// closure, so a row can bind its own index. [`stepper_row`] takes a plain
/// function, which cannot do that.
fn mini_stepper(
    label: &'static str,
    value: u32,
    range: std::ops::RangeInclusive<u32>,
    on_change: impl Fn(u32) -> Message + 'static,
) -> Element<'static, Message> {
    let (lo, hi) = (*range.start(), *range.end());
    let value = value.clamp(lo, hi);
    let minus = button(text("-").size(13)).padding([2, 8]).style(theme::flat(false));
    let minus = if value > lo { minus.on_press(on_change(value - 1)) } else { minus };
    let plus = button(text("+").size(13)).padding([2, 8]).style(theme::flat(false));
    let plus = if value < hi { plus.on_press(on_change(value + 1)) } else { plus };
    row![
        text(label).size(12).style(theme::muted),
        minus,
        container(text(value.to_string()).size(13)).center_x(Length::Fixed(40.0)),
        plus,
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center)
    .into()
}

fn proxy(s: &Settings) -> Element<'_, Message> {
    let p = &s.proxy;
    let manual = p.mode == ProxyMode::Manual;
    let mut col = page("Proxy");
    col = col.push(check(
        "If a captured download fails, retry through the browser's proxy",
        p.use_browser_proxy_on_error,
        Message::BrowserProxyOnError,
    ));
    col = col.push(space(4.0));
    for mode in ProxyMode::ALL {
        let on = p.mode == mode;
        col = col.push(
            button(
                row![text(if on { "\u{25C9}" } else { "\u{25CB}" }).size(13), text(mode.title()).size(13)]
                    .spacing(8),
            )
            .padding([4, 6])
            .style(theme::flat(on))
            .on_press(Message::ProxyMode(mode)),
        );
    }
    col = col.push(space(4.0));
    if p.mode == ProxyMode::Pac {
        col = col.push(field("PAC script URL", &p.pac_url, "http://example.test/proxy.pac", Message::PacUrl));
    } else {
        col = col.push(dead_label("PAC script URL"));
    }
    if manual {
        col = col.push(field("Proxy host", &p.host, "", Message::ProxyHost));
        col = col.push(field(
            "Port",
            &if p.port == 0 { String::new() } else { p.port.to_string() },
            "0-65535",
            Message::ProxyPort,
        ));
        col = col.push(field("User name", &p.user, "", Message::ProxyUser));
        col = col.push(
            row![
                text("Password").size(13).width(Length::Fixed(LABEL_W)),
                text_input("", &p.password)
                    .secure(true)
                    .on_input(Message::ProxyPassword)
                    .style(theme::input)
                    .padding(6),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        col = col.push(note(
            "The password is kept for this session only — it is never written to \
             the settings file.",
        ));
        col = col.push(text("Use the proxy for:").size(13));
        col = col.push(check("http", p.use_http, Message::ProxyUseHttp));
        col = col.push(check("https", p.use_https, Message::ProxyUseHttps));
        col = col.push(check("ftp", p.use_ftp, Message::ProxyUseFtp));
        col = col.push(note(
            "ftp is stored but not applied: our FTP transfer is a raw socket, \
             which an HTTP proxy cannot carry.",
        ));
    } else {
        for label in ["Proxy host", "Port", "User name", "Password"] {
            col = col.push(dead_label(label));
        }
        col = col.push(dead_check("http", p.use_http));
        col = col.push(dead_check("https", p.use_https));
        col = col.push(dead_check("ftp", p.use_ftp));
    }
    col = col.push(
        row![
            text("Copy the proxy from Windows settings").size(13).width(Length::Fill),
            button(text("Use system proxy").size(13))
                .padding([4, 12])
                .style(theme::flat(false))
                .on_press(Message::ProxyGetSystem),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
    );
    col = col.push(space(4.0));
    // Always on: `ftp::FtpControl::open_data` sends PASV and there is no
    // active-mode path, so a live tick here would promise a choice we do not
    // offer.
    col = col.push(dead_check("Passive FTP", true));
    col = col.push(note("Passive mode is always used; active mode is not implemented."));
    col.into()
}

fn sites_logins() -> Element<'static, Message> {
    let mut col = page("Saved site logins");
    col = col.push(note(
        "Not implemented. Logins belong in the operating system's credential \
         store, which this build does not use yet, so there is nothing to edit.",
    ));
    col = col.push(dead_label("(no saved logins)"));
    col.into()
}

fn dial_up() -> Element<'static, Message> {
    let mut col = page("Dial-up / VPN");
    col = col.push(note(
        "Not implemented. The engine never opens a connection of its own, so \
         these would have no effect.",
    ));
    col = col.push(dead_check("Dial a connection before downloading", false));
    col.into()
}

fn sounds() -> Element<'static, Message> {
    let mut col = page("Sounds");
    col = col.push(note(
        "Not implemented. There is no audio output in this build, so a chosen \
         sound could not be played.",
    ));
    for event in ["Download finished", "Download failed", "Queue started", "Queue stopped"] {
        col = col.push(dead_check(event, false));
    }
    col.into()
}

// ------------------------------------------------------------- the widgets

fn page(heading: &str) -> iced::widget::Column<'_, Message> {
    column![text(heading).size(15)].spacing(8)
}

fn check<'a, F>(label: &'a str, value: bool, on_toggle: F) -> Element<'a, Message>
where
    F: Fn(bool) -> Message + 'a,
{
    checkbox(value)
        .label(label)
        .on_toggle(on_toggle)
        .size(16)
        .text_size(13)
        .text_wrapping(iced::widget::text::Wrapping::WordOrGlyph)
        .into()
}

/// A checkbox we cannot honour: it shows its state and says so, instead of
/// accepting a click that would change nothing.
fn dead_check<'a>(label: &str, value: bool) -> Element<'a, Message> {
    checkbox(value)
        .label(format!("{label}  {NOT_WIRED}"))
        .size(16)
        .text_size(13)
        .text_wrapping(iced::widget::text::Wrapping::WordOrGlyph)
        .into()
}

fn dead_label<'a>(label: &str) -> Element<'a, Message> {
    text(format!("{label}  {NOT_WIRED}")).size(13).style(theme::muted).into()
}

/// A button for a picker we do not have. Rendered without `on_press`, so it
/// cannot be clicked.
fn dead_button<'a>(label: &str, caption: &str) -> Element<'a, Message> {
    row![
        text(format!("{label}  {NOT_WIRED}")).size(13).style(theme::muted).width(Length::Fill),
        button(text(caption.to_owned()).size(13)).padding([4, 12]).style(theme::flat(false)),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .into()
}

fn field<'a, F>(label: &'a str, value: &str, placeholder: &'a str, on_input: F) -> Element<'a, Message>
where
    F: Fn(String) -> Message + 'a,
{
    row![
        text(label).size(13).width(Length::Fixed(LABEL_W)),
        text_input(placeholder, value).on_input(on_input).style(theme::input).padding(6),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .into()
}

/// `-` / value / `+`, clamped to `range` so neither button can leave it.
fn stepper_row<'a>(
    label: &'a str,
    value: u32,
    range: std::ops::RangeInclusive<u32>,
    on_change: fn(u32) -> Message,
) -> Element<'a, Message> {
    let (lo, hi) = (*range.start(), *range.end());
    let value = value.clamp(lo, hi);
    let mut minus = button(text("-").size(13)).padding([2, 10]).style(theme::flat(false));
    if value > lo {
        minus = minus.on_press(on_change(value - 1));
    }
    let mut plus = button(text("+").size(13)).padding([2, 10]).style(theme::flat(false));
    if value < hi {
        plus = plus.on_press(on_change(value + 1));
    }
    row![
        text(label).size(13).width(Length::Fixed(LABEL_W)),
        minus,
        container(text(value.to_string()).size(13)).center_x(Length::Fixed(56.0)),
        plus,
        text(format!("({lo}-{hi})")).size(11).style(theme::muted),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center)
    .into()
}

fn reset_row(message: Message) -> Element<'static, Message> {
    row![
        Space::new().width(Length::Fill),
        button(text("Default").size(13)).padding([4, 12]).style(theme::flat(false)).on_press(message),
    ]
    .into()
}

fn note(body: &str) -> Element<'_, Message> {
    text(body.to_owned()).size(11).style(theme::muted).into()
}

fn space(h: f32) -> Space {
    Space::new().height(Length::Fixed(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_clamp_at_both_ends() {
        let mut s = Settings::default();
        update(&mut s, Message::MaxConnections(0));
        assert_eq!(s.max_segments, 1);
        update(&mut s, Message::MaxConnections(9_999));
        assert_eq!(s.max_segments, 32);
        update(&mut s, Message::MaxConnections(16));
        assert_eq!(s.max_segments, 16);
    }

    #[test]
    fn retries_clamp_at_both_ends() {
        let mut s = Settings::default();
        update(&mut s, Message::Retries(0));
        assert_eq!(s.connection.retries, 0);
        update(&mut s, Message::Retries(100));
        assert_eq!(s.connection.retries, 99);
    }

    #[test]
    fn timeouts_clamp_at_both_ends() {
        let mut s = Settings::default();
        update(&mut s, Message::TimeoutSecs(0));
        assert_eq!(s.connection.timeout_secs, 5);
        update(&mut s, Message::TimeoutSecs(10_000));
        assert_eq!(s.connection.timeout_secs, 600);
    }

    #[test]
    fn limits_clamp_at_both_ends() {
        let mut s = Settings::default();
        update(&mut s, Message::LimitMbytes(0));
        assert_eq!(s.connection.limit_mbytes, 1);
        update(&mut s, Message::LimitHours(0));
        assert_eq!(s.connection.limit_hours, 1);
        update(&mut s, Message::LimitHours(10_000));
        assert_eq!(s.connection.limit_hours, 720);
    }

    #[test]
    fn a_non_numeric_port_leaves_the_old_one_alone() {
        let mut s = Settings::default();
        update(&mut s, Message::ProxyPort("8080".into()));
        assert_eq!(s.proxy.port, 8080);
        for junk in ["", "  ", "abc", "-1", "70000", "80.5", "8 0"] {
            update(&mut s, Message::ProxyPort(junk.into()));
        }
        // Only the empty strings were accepted, and they mean "unset".
        assert_eq!(s.proxy.port, 0);
        update(&mut s, Message::ProxyPort("65535".into()));
        assert_eq!(s.proxy.port, 65535);
        update(&mut s, Message::ProxyPort("nonsense".into()));
        assert_eq!(s.proxy.port, 65535);
    }

    #[test]
    fn extension_and_host_lists_split_on_whitespace_and_reset() {
        let mut s = Settings::default();
        update(&mut s, Message::CaptureExtensions("  ZIP   RAR\tEXE ".into()));
        assert_eq!(s.capture_extensions, vec!["ZIP", "RAR", "EXE"]);
        update(&mut s, Message::ResetCaptureExtensions);
        assert_eq!(s.capture_extensions, Settings::default().capture_extensions);

        update(&mut s, Message::ExceptionHosts("a.test b.test".into()));
        assert_eq!(s.exception_hosts.len(), 2);
        update(&mut s, Message::ResetExceptionHosts);
        assert!(s.exception_hosts.is_empty());
    }

    #[test]
    fn toggling_a_browser_row_out_of_range_is_ignored() {
        let mut s = Settings::default();
        update(&mut s, Message::CaptureBrowser(1, false));
        assert!(!s.general.capture_browsers[1].1);
        update(&mut s, Message::CaptureBrowser(999, false));
        assert_eq!(s.general.capture_browsers.len(), 6);
    }

    #[test]
    fn every_tab_renders() {
        let mut s = Settings::default();
        s.proxy.mode = ProxyMode::Manual;
        s.connection.limit_enabled = true;
        for tab in TABS {
            s.options_tab = tab;
            let _ = view(&s);
        }
        // And again in the opposite gating state, which draws the dead controls.
        s.proxy.mode = ProxyMode::Pac;
        s.connection.limit_enabled = false;
        s.downloads.show_start_dialog = false;
        for tab in TABS {
            s.options_tab = tab;
            let _ = view(&s);
        }
    }

    #[test]
    fn selecting_a_tab_is_remembered_in_settings() {
        let mut s = Settings::default();
        update(&mut s, Message::SelectTab(OptionsTab::Sounds));
        assert_eq!(s.options_tab, OptionsTab::Sounds);
    }

    #[test]
    fn a_half_typed_time_does_not_wipe_the_field() {
        let mut s = Settings::default();
        update(&mut s, Message::ScheduleStart("2".into()));
        assert_eq!(s.schedule.start, Settings::default().schedule.start, "unchanged");
        update(&mut s, Message::ScheduleStart("21:45".into()));
        assert_eq!(s.schedule.start, TimeOfDay::new(21, 45));
        // Empty means "no automatic start", which is a real value.
        update(&mut s, Message::ScheduleStart("  ".into()));
        assert_eq!(s.schedule.start, None);
        // Out of range is not a time.
        update(&mut s, Message::ScheduleStop("25:00".into()));
        assert_eq!(s.schedule.stop, Settings::default().schedule.stop);
    }

    #[test]
    fn a_day_toggles_without_touching_the_others() {
        let mut s = Settings::default();
        update(&mut s, Message::ScheduleDay(Weekday::Monday, false));
        assert!(!s.schedule.days.contains(Weekday::Monday));
        assert!(s.schedule.days.contains(Weekday::Tuesday));
        update(&mut s, Message::ScheduleDay(Weekday::Monday, true));
        assert!(s.schedule.days.contains(Weekday::Monday));
    }

    #[test]
    fn a_category_mask_is_stored_in_the_form_matching_expects() {
        let mut s = Settings::default();
        update(&mut s, Message::CategoryMask(0, "  .PSD, *.ai ;EPS ".into()));
        assert_eq!(s.categories[0].mask, ["psd", "ai", "eps"]);
        // And it still matches a file, which is the point of the field.
        s.categories[0].name = "Art".into();
        assert_eq!(s.category_for("poster.PSD").map(|c| c.name.as_str()), Some("Art"));
    }

    #[test]
    fn a_new_category_needs_a_name_and_gets_its_own_id() {
        let mut s = Settings::default();
        let before = s.categories.len();
        let highest = s.categories.iter().map(|c| c.id).max().unwrap_or(0);

        // No name: nothing is added, and the typed mask is not thrown away.
        update(&mut s, Message::DraftCategoryMask("psd".into()));
        update(&mut s, Message::CategoryAdd);
        assert_eq!(s.categories.len(), before);
        assert_eq!(s.draft_category.1, "psd");

        update(&mut s, Message::DraftCategoryName(" Art ".into()));
        update(&mut s, Message::CategoryAdd);
        assert_eq!(s.categories.len(), before + 1);
        let added = s.categories.last().unwrap();
        assert_eq!(added.name, "Art");
        assert_eq!(added.mask, ["psd"]);
        assert!(added.id > highest, "a reused id would merge two categories");
        assert_eq!(added.path, s.download_dir.join("Art"));
        assert_eq!(s.draft_category, (String::new(), String::new()), "the row clears");
    }

    #[test]
    fn removing_a_category_leaves_the_rest_in_order() {
        let mut s = Settings::default();
        let names: Vec<String> = s.categories.iter().map(|c| c.name.clone()).collect();
        update(&mut s, Message::CategoryRemove(0));
        assert_eq!(s.categories.len(), names.len() - 1);
        assert_eq!(s.categories[0].name, names[1]);
        // Out of range is ignored rather than panicking.
        update(&mut s, Message::CategoryRemove(999));
        assert_eq!(s.categories.len(), names.len() - 1);
    }
}
