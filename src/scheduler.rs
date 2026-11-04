//! Download queues and their schedules.
//!
//! Each queue owns an ordered item list and one schedule.
//!
//! Two modelling decisions worth stating:
//!
//! * Start/stop are a time of day, never a timestamp: a nightly window has no
//!   date. We store [`TimeOfDay`] and no date type at all.
//! * This module is pure. No timer, no shutdown, no UI. The caller drives it
//!   from its existing tick: ask [`due`] what should happen, ask
//!   [`Scheduler::next_to_start`] which downloads to hand to the engine, and
//!   perform the [`OnComplete`] action itself.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Default start time: late evening, when bandwidth is usually idle (23:00).
pub const DEFAULT_START: TimeOfDay = TimeOfDay { hour: 23, minute: 0 };
/// Default stop time: before the working day starts (07:30).
pub const DEFAULT_STOP: TimeOfDay = TimeOfDay { hour: 7, minute: 30 };
/// "Start again every …", in minutes.
pub const DEFAULT_PERIODIC_MINUTES: u32 = 120;
/// Retries per download before the queue gives up on it.
pub const DEFAULT_RETRIES: u32 = 10;
/// Queue id of the built-in "Main download queue".
pub const MAIN_QUEUE_ID: u32 = 1;
/// Queue id of the built-in "Synchronization queue".
pub const SYNC_QUEUE_ID: u32 = 2;

/// A time of day, nothing more. Ordering is lexicographic on `(hour, minute)`,
/// which is chronological because both fields are in range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TimeOfDay {
    pub hour: u8,
    pub minute: u8,
}

impl TimeOfDay {
    /// `None` when out of range; there is no clock arithmetic to wrap.
    pub fn new(hour: u8, minute: u8) -> Option<Self> {
        (hour < 24 && minute < 60).then_some(Self { hour, minute })
    }

    /// Clamped into range. Used before every comparison so a hand-edited or
    /// corrupt JSON value can never make a window unreachable.
    pub fn clamped(self) -> Self {
        Self { hour: self.hour.min(23), minute: self.minute.min(59) }
    }

    pub fn minutes_since_midnight(self) -> u32 {
        let t = self.clamped();
        t.hour as u32 * 60 + t.minute as u32
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

impl FromStr for TimeOfDay {
    type Err = ();

    /// `"HH:MM"`, with optional seconds that are ignored (schedules are
    /// minute-resolution, but people paste `HH:MM:SS`).
    fn from_str(s: &str) -> Result<Self, ()> {
        let mut parts = s.trim().split(':');
        let hour = parts.next().ok_or(())?.trim().parse::<u8>().map_err(|_| ())?;
        let minute = parts.next().ok_or(())?.trim().parse::<u8>().map_err(|_| ())?;
        if let Some(sec) = parts.next()
            && sec.trim().parse::<u8>().is_err()
        {
            return Err(());
        }
        if parts.next().is_some() {
            return Err(());
        }
        Self::new(hour, minute).ok_or(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Weekday {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl Weekday {
    /// 0 = Sunday … 6 = Saturday, matching the mask's bit numbering.
    pub fn index(self) -> u8 {
        self as u8
    }

    pub fn from_index(i: u8) -> Option<Self> {
        use Weekday::*;
        [Sunday, Monday, Tuesday, Wednesday, Thursday, Friday, Saturday]
            .get(i as usize)
            .copied()
    }

    pub fn previous(self) -> Self {
        // (i + 6) % 7 — always in range, so the fallback is unreachable.
        Self::from_index((self.index() + 6) % 7).unwrap_or(Weekday::Sunday)
    }
}

/// Days a schedule runs on. **Bit 0 = Sunday, bit 1 = Monday, … bit 6 =
/// Saturday**, matching the C library's and Windows' weekday numbering. Default `0x7F`
/// = every day. Bit 7 is unused and ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayMask(pub u8);

impl DayMask {
    pub fn all() -> Self {
        Self(0x7F)
    }

    pub fn none() -> Self {
        Self(0)
    }

    pub fn contains(self, day: Weekday) -> bool {
        self.0 & (1 << day.index()) != 0
    }

    pub fn with(self, day: Weekday, on: bool) -> Self {
        let bit = 1 << day.index();
        Self(if on { self.0 | bit } else { self.0 & !bit })
    }
}

impl Default for DayMask {
    fn default() -> Self {
        Self::all()
    }
}

/// What to do once the queue drains. Only one of these can usefully win, so
/// this is an enum rather than independent flags. Execution is the caller's job — nothing here
/// touches the machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnComplete {
    #[default]
    Nothing,
    /// Hang up a dial-up connection.
    HangUpModem,
    /// Quit the application.
    ExitApp,
    /// Turn the computer off; `force` skips waiting for other applications.
    ShutDown { force: bool },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// The schedule is armed at all: both "start at" and the queue-level
    /// on/off.
    #[serde(default)]
    pub enabled: bool,
    /// "One-time downloading" rather than "Periodic synchronization". Only
    /// affects whether the caller re-arms after a run; `due` ignores it.
    #[serde(default)]
    pub one_time: bool,
    /// `None` = no automatic start.
    #[serde(default)]
    pub start: Option<TimeOfDay>,
    /// `None` = no automatic stop.
    #[serde(default)]
    pub stop: Option<TimeOfDay>,
    #[serde(default)]
    pub days: DayMask,
    /// 0 means "do not repeat" — a repeat-enabled flag collapsed into the
    /// value, so no caller can divide by zero or spin.
    /// shortcut: a separate repeat flag is only needed to remember a disabled
    /// interval across UI toggles; add it when the settings dialog lands.
    #[serde(default)]
    pub periodic_interval_minutes: u32,
    #[serde(default)]
    pub on_complete: OnComplete,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            enabled: false,
            one_time: false,
            start: Some(DEFAULT_START),
            stop: Some(DEFAULT_STOP),
            days: DayMask::all(),
            periodic_interval_minutes: DEFAULT_PERIODIC_MINUTES,
            on_complete: OnComplete::Nothing,
        }
    }
}

impl Schedule {
    /// `None` when repeating is off. Never `Some(0)`, so a caller that sleeps
    /// for this value cannot busy-loop.
    pub fn repeat_interval_minutes(&self) -> Option<u32> {
        (self.periodic_interval_minutes > 0).then_some(self.periodic_interval_minutes)
    }
}

/// The local clock in the form [`due`] takes.
///
/// Local, not UTC: a schedule of 23:00–07:30 means the user's night, and an
/// offset would silently shift every window. Windows hands us the weekday
/// with `0 = Sunday`, which is exactly [`DayMask`]'s bit numbering.
#[cfg(windows)]
pub fn local_now() -> (Weekday, TimeOfDay) {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocalTime(out: *mut SystemTime);
    }
    let mut t = SystemTime::default();
    // SAFETY: `t` is a live, correctly sized SYSTEMTIME we own.
    unsafe { GetLocalTime(&mut t) };
    let day = Weekday::from_index(t.day_of_week as u8).unwrap_or(Weekday::Sunday);
    let time = TimeOfDay::new(t.hour as u8, t.minute as u8).unwrap_or_default();
    (day, time)
}

/// Local time from the C library, which applies `TZ` and `/etc/localtime`.
/// `tm_wday` also counts from `0 = Sunday`.
#[cfg(unix)]
pub fn local_now() -> (Weekday, TimeOfDay) {
    use std::ffi::{c_char, c_int, c_long};
    // The POSIX prefix every libc we target (glibc, musl) shares,
    // followed by the BSD/GNU extras glibc appends. The buffer
    // must be at least as large as the real `struct tm`, which this is.
    #[repr(C)]
    struct Tm {
        sec: c_int,
        min: c_int,
        hour: c_int,
        mday: c_int,
        mon: c_int,
        year: c_int,
        wday: c_int,
        yday: c_int,
        isdst: c_int,
        gmtoff: c_long,
        zone: *const c_char,
    }
    unsafe extern "C" {
        fn tzset();
        // `time_t` is `long` on every 64-bit unix and on 32-bit glibc.
        fn localtime_r(t: *const c_long, out: *mut Tm) -> *mut Tm;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as c_long;
    let mut tm = Tm {
        sec: 0, min: 0, hour: 0, mday: 0, mon: 0, year: 0, wday: 0, yday: 0, isdst: 0,
        gmtoff: 0, zone: std::ptr::null(),
    };
    // SAFETY: both pointers refer to live locals of the declared types;
    // `tzset` must precede `localtime_r`, which is not required to call it.
    let ok = unsafe {
        tzset();
        !localtime_r(&now, &mut tm).is_null()
    };
    if !ok {
        return (Weekday::Sunday, TimeOfDay::default());
    }
    let day = Weekday::from_index(tm.wday as u8).unwrap_or(Weekday::Sunday);
    let time = TimeOfDay::new(tm.hour as u8, tm.min as u8).unwrap_or_default();
    (day, time)
}

/// What the clock says the queue should be doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Start,
    Stop,
    Nothing,
}

/// The one piece of real logic here: should this queue be running at `now`?
///
/// `Start` means "inside the window", `Stop` means "outside a window that has
/// a stop time", `Nothing` means "the clock has no opinion" — so the result is
/// idempotent and safe to apply on every tick.
///
/// **Overnight rule.** When `start > stop` the window spans midnight, and the
/// naive `start <= now && now < stop` is empty — the bug this function exists
/// to avoid. The window belongs to the weekday it *started* on, so we are
/// inside it when either
///
/// * `now >= start` and today is in the mask (the evening half), or
/// * `now < stop` and *yesterday* is in the mask (the morning half).
///
/// With the defaults 23:00–07:30 and all days enabled: 23:30 Start,
/// 03:00 Start, 08:00 Stop.
///
/// A window whose start equals its stop is zero-length and yields `Nothing`,
/// never a run that can't end.
pub fn due(schedule: &Schedule, now: (Weekday, TimeOfDay)) -> Action {
    if !schedule.enabled {
        return Action::Nothing;
    }
    let (day, time) = (now.0, now.1.clamped());
    match (schedule.start.map(TimeOfDay::clamped), schedule.stop.map(TimeOfDay::clamped)) {
        (None, None) => Action::Nothing,
        // Start with no stop: fire on an enabled day, then say nothing.
        (Some(start), None) => {
            if schedule.days.contains(day) && time >= start {
                Action::Start
            } else {
                Action::Nothing
            }
        }
        // Stop with no start: the queue only ever runs manually.
        (None, Some(stop)) => {
            if schedule.days.contains(day) && time >= stop {
                Action::Stop
            } else {
                Action::Nothing
            }
        }
        (Some(start), Some(stop)) if start == stop => Action::Nothing,
        (Some(start), Some(stop)) => {
            let inside = if start < stop {
                schedule.days.contains(day) && time >= start && time < stop
            } else {
                (schedule.days.contains(day) && time >= start)
                    || (schedule.days.contains(day.previous()) && time < stop)
            };
            if inside { Action::Start } else { Action::Stop }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueState {
    /// Not running and not armed.
    #[default]
    Idle,
    /// Armed, waiting for its window.
    Waiting,
    Running,
    /// Held by the user; the clock cannot move it.
    Paused,
    /// Drained. Where the caller applies `OnComplete`.
    Done,
}

impl QueueState {
    /// Fold one tick into the state. `armed` is the schedule being enabled,
    /// `queue_empty` means nothing is left to run.
    pub fn next(self, action: Action, armed: bool, queue_empty: bool) -> Self {
        use QueueState::*;
        match (self, action) {
            // Only the user leaves Paused — see `resume`.
            (Paused, _) => Paused,
            (_, Action::Start) if queue_empty => Done,
            (_, Action::Start) => Running,
            (Running, Action::Stop) => {
                if armed {
                    Waiting
                } else {
                    Idle
                }
            }
            (Running, Action::Nothing) if queue_empty => Done,
            (Running, _) => Running,
            (_, _) if armed => Waiting,
            (Done, _) => Done,
            (_, _) => Idle,
        }
    }

    pub fn pause(self) -> Self {
        match self {
            QueueState::Running | QueueState::Waiting => QueueState::Paused,
            other => other,
        }
    }

    pub fn resume(self) -> Self {
        match self {
            QueueState::Paused => QueueState::Idle,
            other => other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queue {
    /// Referenced by each download's queue id. 0 is reserved for "not in a
    /// queue" and is never a queue's own id.
    pub id: u32,
    pub name: String,
    /// "Download N files at the same time". 0 means the queue starts nothing.
    #[serde(default)]
    pub max_concurrent: usize,
    #[serde(default)]
    pub schedule: Schedule,
    /// Download ids, in run order. This ordering is the queue.
    #[serde(default)]
    pub items: Vec<u64>,
    #[serde(default = "default_retries")]
    pub retries_per_file: u32,
}

fn default_retries() -> u32 {
    DEFAULT_RETRIES
}

impl Queue {
    pub fn new(id: u32, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            max_concurrent: 1,
            schedule: Schedule::default(),
            items: Vec::new(),
            retries_per_file: DEFAULT_RETRIES,
        }
    }

    /// Built-in queues have fixed names; user queues are `Queue # N`.
    pub fn user_queue_name(n: u32) -> String {
        format!("Queue # {n}")
    }

    /// Both moves are no-ops at the list boundary: no wrap-around, no panic.
    pub fn move_up(&mut self, id: u64) -> bool {
        match self.items.iter().position(|&i| i == id) {
            Some(pos) if pos > 0 => {
                self.items.swap(pos - 1, pos);
                true
            }
            _ => false,
        }
    }

    pub fn move_down(&mut self, id: u64) -> bool {
        match self.items.iter().position(|&i| i == id) {
            Some(pos) if pos + 1 < self.items.len() => {
                self.items.swap(pos, pos + 1);
                true
            }
            _ => false,
        }
    }

    pub fn push(&mut self, id: u64) {
        if !self.items.contains(&id) {
            self.items.push(id);
        }
    }

    pub fn remove_item(&mut self, id: u64) -> bool {
        let before = self.items.len();
        self.items.retain(|&i| i != id);
        self.items.len() != before
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scheduler {
    #[serde(default)]
    pub queues: Vec<Queue>,
}

impl Scheduler {
    /// The two built-in queues: Main (on demand) and Synchronization.
    pub fn with_builtins() -> Self {
        Self {
            queues: vec![
                Queue::new(MAIN_QUEUE_ID, "Main download queue"),
                Queue::new(SYNC_QUEUE_ID, "Synchronization queue"),
            ],
        }
    }

    pub fn get(&self, id: u32) -> Option<&Queue> {
        self.queues.iter().find(|q| q.id == id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Queue> {
        self.queues.iter_mut().find(|q| q.id == id)
    }

    /// Lowest free id >= 1, so ids stay stable and 0 keeps meaning "no queue".
    pub fn next_free_id(&self) -> u32 {
        (1..=u32::MAX).find(|id| self.get(*id).is_none()).unwrap_or(u32::MAX)
    }

    /// Rejects id 0 and duplicates rather than corrupting membership.
    pub fn add_queue(&mut self, queue: Queue) -> bool {
        if queue.id == 0 || self.get(queue.id).is_some() {
            return false;
        }
        self.queues.push(queue);
        true
    }

    /// Returns the removed queue so the caller can reset its members'
    /// `queue_id` to 0 ("not in a queue").
    pub fn remove_queue(&mut self, id: u32) -> Option<Queue> {
        let pos = self.queues.iter().position(|q| q.id == id)?;
        Some(self.queues.remove(pos))
    }

    /// Download ids to hand to the engine now, in queue order then item order.
    ///
    /// Each queue contributes at most `max_concurrent` minus the members of
    /// that queue already in `running`. Whether a queue's clock allows it is
    /// the caller's call: filter with [`due`] before this, or ignore the
    /// schedule for a manually started queue.
    /// Downloads that may start **right now**, honouring each queue's schedule.
    ///
    /// This is the method callers should use. [`Scheduler::next_to_start`] is
    /// the unchecked primitive underneath it: it answers "what has room?"
    /// without asking "is this queue even supposed to be running?", so calling
    /// it directly from a tick would start a queue outside its window — the
    /// exact mistake a 23:00–07:30 schedule exists to prevent.
    pub fn ready_now(&self, now: (Weekday, TimeOfDay), running: &[u64]) -> Vec<u64> {
        let mut out = Vec::new();
        for queue in &self.queues {
            // A queue with no schedule enabled runs on demand, like Main.
            if queue.schedule.enabled && due(&queue.schedule, now) != Action::Start {
                continue;
            }
            let active = queue.items.iter().filter(|i| running.contains(i)).count();
            let free = queue.max_concurrent.saturating_sub(active);
            out.extend(
                queue
                    .items
                    .iter()
                    .filter(|i| !running.contains(i))
                    .take(free)
                    .copied(),
            );
        }
        out
    }

    /// Queues whose window has just closed and whose downloads should stop.
    pub fn should_stop(&self, now: (Weekday, TimeOfDay)) -> Vec<u32> {
        self.queues
            .iter()
            .filter(|q| q.schedule.enabled && due(&q.schedule, now) == Action::Stop)
            .map(|q| q.id)
            .collect()
    }

    /// Whether download `id` may start at `now` under its own queue's window.
    /// A download in no queue, or in a queue with no window enabled, is not
    /// held back here; the global schedule is the caller's separate check.
    pub fn window_allows(&self, id: u64, now: (Weekday, TimeOfDay)) -> bool {
        self.queues
            .iter()
            .find(|q| q.items.contains(&id))
            .is_none_or(|q| !q.schedule.enabled || due(&q.schedule, now) == Action::Start)
    }

    pub fn next_to_start(&self, running: &[u64]) -> Vec<u64> {
        let mut out = Vec::new();
        for queue in &self.queues {
            let active = queue.items.iter().filter(|i| running.contains(i)).count();
            let mut free = queue.max_concurrent.saturating_sub(active);
            for &id in &queue.items {
                if free == 0 {
                    break;
                }
                // `!out.contains` guards the one id that a hand-edited file
                // could list in two queues; the engine must not start it twice.
                if !running.contains(&id) && !out.contains(&id) {
                    out.push(id);
                    free -= 1;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod lead_tests {
    //! Tests for the schedule-aware wrappers the lead added over the agent's
    //! primitives, covering the footgun the agent itself flagged.
    use super::*;

    fn at(h: u8, m: u8) -> TimeOfDay {
        TimeOfDay { hour: h, minute: m }
    }

    fn overnight_queue() -> Scheduler {
        let mut s = Scheduler { queues: Vec::new() };
        let mut q = Queue::new(10, "Night");
        q.max_concurrent = 2;
        q.items = vec![1, 2, 3, 4];
        // Schedule::default() is 23:00 -> 07:30, all days.
        q.schedule.enabled = true;
        s.queues.push(q);
        s
    }

    #[test]
    fn ready_now_starts_inside_the_overnight_window() {
        let s = overnight_queue();
        let started = s.ready_now((Weekday::Monday, at(23, 30)), &[]);
        assert_eq!(started, vec![1, 2], "max_concurrent of 2 not respected");
        let early = s.ready_now((Weekday::Tuesday, at(3, 0)), &[]);
        assert_eq!(early, vec![1, 2], "the window should still be open at 03:00");
    }

    #[test]
    fn ready_now_starts_nothing_outside_the_window() {
        let s = overnight_queue();
        assert!(s.ready_now((Weekday::Monday, at(12, 0)), &[]).is_empty());
        assert!(s.ready_now((Weekday::Monday, at(8, 0)), &[]).is_empty());
    }

    /// The precise mistake the agent warned about: the unchecked primitive
    /// happily starts work at noon. `ready_now` must not.
    #[test]
    fn the_unchecked_primitive_ignores_the_schedule_but_ready_now_does_not() {
        let s = overnight_queue();
        let noon = (Weekday::Monday, at(12, 0));
        assert!(!s.next_to_start(&[]).is_empty(), "primitive changed behaviour");
        assert!(s.ready_now(noon, &[]).is_empty(), "ready_now leaked the footgun");
    }

    #[test]
    fn a_queue_with_no_schedule_runs_on_demand() {
        let mut s = overnight_queue();
        s.queues[0].schedule.enabled = false;
        assert_eq!(s.ready_now((Weekday::Monday, at(12, 0)), &[]).len(), 2);
    }

    #[test]
    fn running_downloads_count_against_the_limit() {
        let s = overnight_queue();
        let night = (Weekday::Monday, at(23, 30));
        assert_eq!(s.ready_now(night, &[1]).len(), 1, "one slot should remain");
        assert!(s.ready_now(night, &[1, 2]).is_empty(), "no slot should remain");
    }

    #[test]
    fn window_allows_gates_only_downloads_in_a_windowed_queue() {
        let mut s = overnight_queue();
        let noon = (Weekday::Monday, at(12, 0));
        let night = (Weekday::Monday, at(23, 30));
        // Download 1 is in the windowed queue: closed at noon, open at night.
        assert!(!s.window_allows(1, noon));
        assert!(s.window_allows(1, night));
        // Download 99 is in no queue, so the queue window never holds it back.
        assert!(s.window_allows(99, noon));
        // A queue with its window off runs on demand.
        s.queues[0].schedule.enabled = false;
        assert!(s.window_allows(1, noon));
    }

    #[test]
    fn should_stop_reports_the_queue_when_its_window_closes() {
        let s = overnight_queue();
        // 08:00 is past the 07:30 stop.
        assert_eq!(s.should_stop((Weekday::Monday, at(8, 0))), vec![10]);
        assert!(s.should_stop((Weekday::Monday, at(23, 30))).is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::Weekday::*;
    use super::*;

    fn overnight() -> Schedule {
        Schedule { enabled: true, ..Schedule::default() }
    }

    #[test]
    fn day_mask_bit_order_is_sunday_first() {
        assert_eq!(DayMask::all().0, 0x7F);
        assert_eq!(DayMask::none().with(Sunday, true).0, 0b000_0001);
        assert_eq!(DayMask::none().with(Monday, true).0, 0b000_0010);
        assert_eq!(DayMask::none().with(Saturday, true).0, 0b100_0000);
        assert!(DayMask(0b000_0100).contains(Tuesday));
        assert!(!DayMask(0b000_0100).contains(Wednesday));
        assert_eq!(Wednesday.previous(), Tuesday);
        assert_eq!(Sunday.previous(), Saturday);
    }

    #[test]
    fn overnight_window_spans_midnight() {
        let s = overnight();
        assert_eq!(s.start, Some(TimeOfDay { hour: 23, minute: 0 }));
        // Evening half.
        assert_eq!(due(&s, (Monday, TimeOfDay { hour: 23, minute: 30 })), Action::Start);
        // Morning half, the next calendar day.
        assert_eq!(due(&s, (Tuesday, TimeOfDay { hour: 3, minute: 0 })), Action::Start);
        // Just before the stop, and just after.
        assert_eq!(due(&s, (Tuesday, TimeOfDay { hour: 7, minute: 29 })), Action::Start);
        assert_eq!(due(&s, (Tuesday, TimeOfDay { hour: 8, minute: 0 })), Action::Stop);
        // Mid-afternoon is outside.
        assert_eq!(due(&s, (Monday, TimeOfDay { hour: 14, minute: 0 })), Action::Stop);
    }

    #[test]
    fn overnight_window_belongs_to_the_day_it_started_on() {
        // Monday only: the run starts Monday 23:00 and ends Tuesday 07:30.
        let s = Schedule { days: DayMask::none().with(Monday, true), ..overnight() };
        assert_eq!(due(&s, (Monday, TimeOfDay { hour: 23, minute: 30 })), Action::Start);
        assert_eq!(due(&s, (Tuesday, TimeOfDay { hour: 3, minute: 0 })), Action::Start);
        // Tuesday evening is not in the mask, so nothing starts.
        assert_eq!(due(&s, (Tuesday, TimeOfDay { hour: 23, minute: 30 })), Action::Stop);
        assert_eq!(due(&s, (Wednesday, TimeOfDay { hour: 3, minute: 0 })), Action::Stop);
    }

    #[test]
    fn same_day_window() {
        let s = Schedule {
            start: TimeOfDay::new(9, 0),
            stop: TimeOfDay::new(17, 0),
            ..overnight()
        };
        assert_eq!(due(&s, (Friday, TimeOfDay { hour: 8, minute: 59 })), Action::Stop);
        assert_eq!(due(&s, (Friday, TimeOfDay { hour: 9, minute: 0 })), Action::Start);
        assert_eq!(due(&s, (Friday, TimeOfDay { hour: 16, minute: 59 })), Action::Start);
        assert_eq!(due(&s, (Friday, TimeOfDay { hour: 17, minute: 0 })), Action::Stop);
        // Not in the mask: never inside, even at noon.
        let s = Schedule { days: DayMask::none().with(Monday, true), ..s };
        assert_eq!(due(&s, (Friday, TimeOfDay { hour: 12, minute: 0 })), Action::Stop);
    }

    #[test]
    fn degenerate_and_disabled_schedules_do_nothing() {
        let same = Schedule { stop: Some(DEFAULT_START), ..overnight() };
        assert_eq!(due(&same, (Monday, TimeOfDay { hour: 23, minute: 0 })), Action::Nothing);
        assert_eq!(due(&same, (Monday, TimeOfDay { hour: 5, minute: 0 })), Action::Nothing);

        let off = Schedule { enabled: false, ..Schedule::default() };
        assert_eq!(due(&off, (Monday, TimeOfDay { hour: 23, minute: 30 })), Action::Nothing);

        let none = Schedule { start: None, stop: None, ..overnight() };
        assert_eq!(due(&none, (Monday, TimeOfDay { hour: 23, minute: 30 })), Action::Nothing);

        let start_only = Schedule { stop: None, ..overnight() };
        assert_eq!(due(&start_only, (Monday, TimeOfDay { hour: 23, minute: 30 })), Action::Start);
        assert_eq!(due(&start_only, (Monday, TimeOfDay { hour: 5, minute: 0 })), Action::Nothing);
    }

    #[test]
    fn out_of_range_times_are_clamped_not_panicked() {
        let s = Schedule {
            start: Some(TimeOfDay { hour: 99, minute: 200 }),
            stop: TimeOfDay::new(7, 30),
            ..overnight()
        };
        // 99:200 clamps to 23:59, so this is still an overnight window.
        assert_eq!(due(&s, (Monday, TimeOfDay { hour: 23, minute: 59 })), Action::Start);
        // A nonsense "now" clamps to 23:00, which is before that start.
        assert_eq!(due(&s, (Monday, TimeOfDay { hour: 250, minute: 0 })), Action::Stop);
        assert_eq!(TimeOfDay::new(24, 0), None);
        assert_eq!(TimeOfDay::new(0, 60), None);
    }

    #[test]
    fn time_of_day_parses_formats_and_orders() {
        assert_eq!("23:00".parse::<TimeOfDay>(), Ok(DEFAULT_START));
        assert_eq!("07:30:00".parse::<TimeOfDay>(), Ok(DEFAULT_STOP));
        assert_eq!(DEFAULT_STOP.to_string(), "07:30");
        assert!(DEFAULT_STOP < DEFAULT_START);
        assert!(TimeOfDay { hour: 9, minute: 5 } < TimeOfDay { hour: 9, minute: 6 });
        for bad in ["", "abc", "25:00", "1:2:3:4", "12", "12:xx", "-1:00"] {
            assert!(bad.parse::<TimeOfDay>().is_err(), "{bad} parsed");
        }
    }

    #[test]
    fn zero_interval_means_no_repeat() {
        let mut s = Schedule::default();
        assert_eq!(s.repeat_interval_minutes(), Some(120));
        s.periodic_interval_minutes = 0;
        assert_eq!(s.repeat_interval_minutes(), None);
    }

    #[test]
    fn reordering_is_a_no_op_at_both_boundaries() {
        let mut q = Queue::new(1, "Main download queue");
        q.items = vec![10, 20, 30];
        assert!(!q.move_up(10));
        assert_eq!(q.items, vec![10, 20, 30]);
        assert!(!q.move_down(30));
        assert_eq!(q.items, vec![10, 20, 30]);
        assert!(q.move_down(10));
        assert_eq!(q.items, vec![20, 10, 30]);
        assert!(q.move_up(10));
        assert_eq!(q.items, vec![10, 20, 30]);
        // Unknown id, and an empty list.
        assert!(!q.move_up(999));
        q.items.clear();
        assert!(!q.move_up(10) && !q.move_down(10));
    }

    #[test]
    fn next_to_start_respects_max_concurrent_and_order() {
        let mut s = Scheduler::with_builtins();
        {
            let main = s.get_mut(MAIN_QUEUE_ID).expect("built-in queue 1");
            main.max_concurrent = 2;
            main.items = vec![1, 2, 3, 4];
        }
        {
            let sync = s.get_mut(SYNC_QUEUE_ID).expect("built-in queue 2");
            sync.max_concurrent = 1;
            sync.items = vec![5, 6];
        }
        // Nothing running: two from the first queue, one from the second.
        assert_eq!(s.next_to_start(&[]), vec![1, 2, 5]);
        // One already running in the main queue: only one more slot, and it is
        // the next item in list order.
        assert_eq!(s.next_to_start(&[1]), vec![2, 5]);
        // Saturated.
        assert_eq!(s.next_to_start(&[1, 2, 5]), Vec::<u64>::new());
        // Saturated on its own members only — the main queue is full at two.
        assert_eq!(s.next_to_start(&[1, 2]), vec![5]);
        // max_concurrent 0 starts nothing, so only the main queue's slot is left.
        s.get_mut(SYNC_QUEUE_ID).expect("queue 2").max_concurrent = 0;
        assert_eq!(s.next_to_start(&[1]), vec![2]);
        assert_eq!(s.next_to_start(&[1, 2]), Vec::<u64>::new());
        // Order follows the item list, so a reorder changes what starts.
        let main = s.get_mut(MAIN_QUEUE_ID).expect("queue 1");
        main.move_up(4);
        main.move_up(4);
        assert_eq!(main.items, vec![1, 4, 2, 3]);
        assert_eq!(s.next_to_start(&[1]), vec![4]);
    }

    #[test]
    fn queue_bookkeeping() {
        let mut s = Scheduler::with_builtins();
        assert_eq!(s.next_free_id(), 3);
        assert!(s.add_queue(Queue::new(3, Queue::user_queue_name(3))));
        assert!(!s.add_queue(Queue::new(3, "dup")));
        assert!(!s.add_queue(Queue::new(0, "reserved")));
        assert_eq!(s.get(3).map(|q| q.name.as_str()), Some("Queue # 3"));
        assert_eq!(s.remove_queue(3).map(|q| q.id), Some(3));
        assert_eq!(s.remove_queue(3), None);
    }

    #[test]
    fn state_transitions() {
        use QueueState::*;
        assert_eq!(Idle.next(Action::Start, true, false), Running);
        assert_eq!(Idle.next(Action::Nothing, true, false), Waiting);
        assert_eq!(Idle.next(Action::Nothing, false, false), Idle);
        assert_eq!(Waiting.next(Action::Start, true, false), Running);
        assert_eq!(Running.next(Action::Nothing, true, true), Done);
        assert_eq!(Running.next(Action::Stop, true, false), Waiting);
        assert_eq!(Running.next(Action::Stop, false, false), Idle);
        assert_eq!(Running.next(Action::Nothing, true, false), Running);
        // An armed but empty queue is done, not spinning.
        assert_eq!(Waiting.next(Action::Start, true, true), Done);
        // The clock cannot move a paused queue.
        assert_eq!(Running.pause(), Paused);
        assert_eq!(Paused.next(Action::Start, true, false), Paused);
        assert_eq!(Paused.resume(), Idle);
        assert_eq!(Done.next(Action::Nothing, false, true), Done);
    }

    #[test]
    fn serde_round_trip() {
        let mut s = Scheduler::with_builtins();
        {
            let q = s.get_mut(MAIN_QUEUE_ID).expect("queue 1");
            q.items = vec![7, 8, 9];
            q.max_concurrent = 4;
            q.schedule.enabled = true;
            q.schedule.one_time = true;
            q.schedule.days = DayMask::none().with(Friday, true);
            q.schedule.on_complete = OnComplete::ShutDown { force: true };
        }
        let json = serde_json::to_string(&s).expect("encode");
        let back: Scheduler = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, s);

        // Partial JSON falls back to defaults instead of failing.
        let q: Queue = serde_json::from_str(r#"{"id":5,"name":"Queue # 5"}"#).expect("partial");
        assert_eq!(q.retries_per_file, DEFAULT_RETRIES);
        assert_eq!(q.schedule, Schedule::default());
        assert!(q.items.is_empty());
    }

    #[test]
    fn the_local_clock_is_in_range_and_agrees_with_the_mask() {
        let (day, time) = local_now();
        assert!(time.hour < 24 && time.minute < 60, "{time} out of range");
        // Round-tripping the weekday through its index is what `DayMask` and
        // the Win32 `wDayOfWeek` both rely on.
        assert_eq!(Weekday::from_index(day.index()), Some(day));
        assert!(DayMask::all().contains(day));
        assert!(!DayMask::none().contains(day));
    }
}
