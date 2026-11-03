//! Crash reports: what Weir keeps when its window or part of its service
//! panics, so the person can see what happened and send it on.
//!
//! [`install`] sets a panic hook in each program. It writes a report in
//! full to `crash.log` in Weir's state folder (`~/.local/state/weir`), and
//! keeps it waiting to be shown (`crash-pending.json`). The window shows
//! it at once when the service says a part of it stopped
//! (`Notification::Crashed`), or else the next time it starts
//! ([`pending`]). [`Report::text`] can hide what is personal in it: the
//! user name, the names of files and folders in the home folder, and web
//! addresses past their site.
//!
//! Nothing here sends anything anywhere.

use crate::recorder::LocalTime;
use crate::report::Desktop;
use crate::safe::{SafeItem, SafeMode, EARLY_CRASH_SECS};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

/// Log lines a report ends with.
const LOG_LINES: usize = 20;
/// Reports kept waiting to be shown; older ones are only in the log.
const PENDING_KEPT: usize = 10;
/// Past this, the crash log keeps only its newer half.
const LOG_MOST: u64 = 2 << 20;
/// Frames of Weir's own code a report lists.
const FRAMES: usize = 16;
/// The longest link GitHub takes for a new issue, near enough.
const LINK_MOST: usize = 7_000;

/// One crash, as it was.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(rename = "CrashReport")]
pub struct Report {
    /// Weir's version.
    pub version: String,
    /// The system, such as `Linux: Nobara Linux 44, KDE (Plasma 6), Wayland`.
    pub system: String,
    /// Local time, `2026-11-03 21:42`.
    pub time: String,
    /// The program that panicked: `window` or `service`.
    pub program: String,
    /// The part of it: what it was doing, such as `request set_strip`, or
    /// its thread's name.
    pub part: String,
    /// The whole program ended, rather than one part of it.
    pub whole: bool,
    /// What the panic said.
    pub message: String,
    /// Where in the code, such as `crates/gui/src/app/bus.rs:453:53`.
    pub at: String,
    /// Weir's own functions on the way there, innermost first.
    pub ours: Vec<String>,
    /// The whole backtrace, for the crash log.
    pub backtrace: String,
    /// The settings that most often matter, as name and value.
    pub settings: Vec<(String, String)>,
    /// The last lines of the program's log, oldest first.
    pub log: Vec<String>,
    /// What [`Report::text`] hides, as it was when it happened.
    pub private: Private,
    /// Seconds from the program's start to the crash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_s: Option<u64>,
    /// This report, told apart from every other, so that safe mode answers
    /// it once.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// The program was in safe mode.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub safe_mode: bool,
    /// What safe mode brought back last before the crash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brought_back: Option<SafeItem>,
}

/// What is personal, to hide.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(rename = "CrashPrivate")]
pub struct Private {
    /// The home folder.
    pub home: Option<String>,
    /// The user name.
    pub user: Option<String>,
}

/// What [`Report::text`] hid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hidden {
    /// The user name, at least once.
    pub user: bool,
    /// File and folder names in the home folder.
    pub names: usize,
    /// Web addresses cut back to their site.
    pub links: usize,
}

impl Hidden {
    /// Whether anything was hidden.
    pub fn any(&self) -> bool {
        self.user || self.names > 0 || self.links > 0
    }
}

/// Lock `m`, whether or not a thread panicked while holding it. Weir's
/// locks guard state that is whole between statements, so what a panic
/// left is still usable, and one panic must not make every later use of
/// the lock panic too.
pub fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// [`lock`] as a method: `state.locked()`.
pub trait Locked<T: ?Sized> {
    /// The lock, whether or not a thread panicked while holding it.
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T: ?Sized> Locked<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> {
        lock(self)
    }
}

static PROGRAM: OnceLock<&'static str> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();
/// Whether the program is in safe mode, and what came back last.
static SAFE: Mutex<(bool, Option<SafeItem>)> = Mutex::new((false, None));
static LOG: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static SETTINGS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static FRESH: Mutex<Vec<Report>> = Mutex::new(Vec::new());
/// Where panics happened in this run: a bug that trips again and again,
/// in a loop that carries on past it, is reported once.
static PLACES: Mutex<Vec<String>> = Mutex::new(Vec::new());
type Waker = Box<dyn Fn() + Send + Sync>;
static WAKER: Mutex<Option<Waker>> = Mutex::new(None);

thread_local! {
    /// What this thread is doing, for a report's part.
    static DOING: RefCell<Option<Cow<'static, str>>> = const { RefCell::new(None) };
    /// What this thread is, for threads Weir did not start and name, such
    /// as PipeWire's real-time one.
    static PART: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// Set the panic hook that keeps reports, for `program` (`window` or
/// `service`). Once, at the start.
pub fn install(program: &'static str) {
    let _ = PROGRAM.set(program);
    let _ = START.set(Instant::now());
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let place = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default();
        if let Ok(mut places) = PLACES.try_lock() {
            if places.contains(&place) {
                return;
            }
            places.push(place);
        }
        let report = Report::of(info);
        keep(&state_dir(), &report);
        if let Ok(mut fresh) = FRESH.try_lock() {
            fresh.push(report);
        }
        if let Ok(waker) = WAKER.try_lock() {
            if let Some(wake) = waker.as_ref() {
                wake();
            }
        }
        before(info);
    }));
}

/// While the value lives, a report from this thread names `what` as the
/// part that panicked.
pub struct Doing(Option<Cow<'static, str>>);

/// Say what this thread is doing, until the value returned is dropped.
/// A `&'static str` costs nothing.
pub fn doing(what: impl Into<Cow<'static, str>>) -> Doing {
    let before = DOING
        .try_with(|d| {
            d.try_borrow_mut()
                .ok()
                .and_then(|mut d| d.replace(what.into()))
        })
        .ok()
        .flatten();
    Doing(before)
}

impl Drop for Doing {
    fn drop(&mut self) {
        let before = self.0.take();
        let _ = DOING.try_with(|d| {
            if let Ok(mut d) = d.try_borrow_mut() {
                *d = before;
            }
        });
    }
}

/// Name this thread for reports, for good: for threads Weir did not
/// start, such as PipeWire's real-time one. Costs nothing, so it can be
/// said every cycle.
pub fn name_part(name: &'static str) {
    let _ = PART.try_with(|p| p.set(Some(name)));
}

/// `f`'s result, or what it said when it panicked: for work done for
/// someone else, such as a request, where a panic is a bug to report but
/// not one to end Weir over. The report is kept all the same.
///
/// Free when nothing panics. Whatever `f` was changing may be left half
/// done.
pub fn caught<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|p| {
        p.downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "it panicked".to_string())
    })
}

/// Keep a line of the log for reports.
pub fn remember(level: &str, message: &str) {
    let t = LocalTime::now();
    let line = format!(
        "{:02}:{:02}:{:02} {level:<5} {message}",
        t.hour, t.minute, t.second
    );
    let mut log = lock(&LOG);
    log.push_back(line);
    while log.len() > LOG_LINES {
        log.pop_front();
    }
}

/// The settings a report names, replacing those named before.
pub fn note_settings(settings: Vec<(String, String)>) {
    *lock(&SETTINGS) = settings;
}

/// Tell reports whether the program is in safe mode, and what it brought
/// back last.
pub fn note_safe_mode(mode: &SafeMode) {
    *lock(&SAFE) = (mode.on, mode.last_brought_back().cloned());
}

/// Call `wake` when a report is kept, so the program can pass it on at
/// once.
pub fn on_crash(wake: impl Fn() + Send + Sync + 'static) {
    *lock(&WAKER) = Some(Box::new(wake));
}

/// Reports kept in this run since this was last asked.
pub fn take_fresh() -> Vec<Report> {
    std::mem::take(&mut *lock(&FRESH))
}

/// Weir's state folder, where the crash log and the service's log live.
pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(std::env::temp_dir)
        .join("weir")
}

/// Where every report is written in full.
pub fn log_path() -> PathBuf {
    state_dir().join("crash.log")
}

/// Reports not yet shown, oldest first, from earlier runs too.
pub fn pending() -> Vec<Report> {
    pending_in(&state_dir())
}

/// The newest report from `program` (`window` or `service`) if it calls
/// for safe mode ([`Report::calls_for_safe_mode`]) and has not been
/// answered yet. Each report is answered once: the next start after this
/// one is a normal one again.
pub fn safe_mode_wanted(program: &str) -> Option<Report> {
    safe_mode_wanted_in(&state_dir(), program)
}

fn safe_mode_wanted_in(dir: &Path, program: &str) -> Option<Report> {
    let newest = pending_in(dir)
        .into_iter()
        .rev()
        .find(|r| r.program == program)?;
    let answered = dir.join(format!("safe-mode-answered-{program}"));
    if newest.id.is_empty()
        || std::fs::read_to_string(&answered).is_ok_and(|id| id.trim() == newest.id)
        || !newest.calls_for_safe_mode()
    {
        return None;
    }
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(&answered, &newest.id);
    Some(newest)
}

/// The reports waiting have been shown.
pub fn seen() {
    let _ = std::fs::remove_file(state_dir().join("crash-pending.json"));
}

fn pending_in(dir: &Path) -> Vec<Report> {
    std::fs::read(dir.join("crash-pending.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Write `report` to the crash log in `dir`, and keep it to show.
fn keep(dir: &Path, report: &Report) {
    let _ = std::fs::create_dir_all(dir);
    let log = dir.join("crash.log");
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > LOG_MOST) {
        if let Ok(old) = std::fs::read(&log) {
            let _ = std::fs::write(&log, &old[old.len() / 2..]);
        }
    }
    let mut text = report.text(false).0;
    text.push_str("\nThe whole backtrace:\n");
    text.push_str(&report.backtrace);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
    {
        use std::io::Write;
        let _ = writeln!(f, "==== {} ====\n{text}\n", report.time);
    }
    let mut all = pending_in(dir);
    all.push(report.clone());
    let from = all.len().saturating_sub(PENDING_KEPT);
    if let Ok(json) = serde_json::to_vec(&all[from..]) {
        let path = dir.join("crash-pending.json");
        let part = dir.join("crash-pending.json.part");
        if std::fs::write(&part, json).is_ok() {
            let _ = std::fs::rename(&part, &path);
        }
    }
}

impl Report {
    /// The report of the panic `info`, on this thread.
    fn of(info: &std::panic::PanicHookInfo<'_>) -> Self {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("unnamed").to_string();
        let doing = DOING
            .try_with(|d| d.try_borrow().ok().and_then(|d| d.clone()))
            .ok()
            .flatten()
            .map(Cow::into_owned)
            .or_else(|| PART.try_with(Cell::get).ok().flatten().map(str::to_string));
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic with no message".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let t = LocalTime::now();
        Report {
            version: env!("CARGO_PKG_VERSION").to_string(),
            system: system(),
            time: format!("{} {:02}:{:02}", t.date(), t.hour, t.minute),
            program: PROGRAM.get().copied().unwrap_or("weir").to_string(),
            whole: thread_name == "main" && doing.is_none(),
            part: doing.unwrap_or(thread_name),
            message,
            at: info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_default(),
            ours: ours(&backtrace),
            backtrace,
            settings: SETTINGS.try_lock().map(|s| s.clone()).unwrap_or_default(),
            log: LOG
                .try_lock()
                .map(|l| l.iter().cloned().collect())
                .unwrap_or_default(),
            private: Private {
                home: dirs::home_dir().map(|h| h.display().to_string()),
                user: std::env::var("USER").ok().filter(|u| !u.is_empty()),
            },
            uptime_s: START.get().map(|s| s.elapsed().as_secs()),
            id: format!(
                "{}-{}-{}",
                PROGRAM.get().copied().unwrap_or("weir"),
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ),
            safe_mode: SAFE.try_lock().map(|s| s.0).unwrap_or(false),
            brought_back: SAFE.try_lock().ok().and_then(|s| s.1.clone()),
        }
    }

    /// Whether the program ended, or its sound did: the whole program, or
    /// the service's audio part, which then stays silent.
    pub fn ended(&self) -> bool {
        self.whole || self.part == "audio"
    }

    /// Whether the program's next start should be in safe mode: it ended
    /// soon after starting, or while in safe mode.
    pub fn calls_for_safe_mode(&self) -> bool {
        self.ended() && (self.safe_mode || self.uptime_s.is_some_and(|s| s < EARLY_CRASH_SECS))
    }

    /// The report as text, with what is personal hidden or not, and what
    /// was hidden.
    pub fn text(&self, hide: bool) -> (String, Hidden) {
        let s = Scrub::new(&self.private, hide);
        let mut t = format!(
            "Weir {} ({}) on {}\n",
            self.version, self.program, self.system
        );
        match self.uptime_s {
            Some(up) => t += &format!("Crashed: {}, {} after starting\n", self.time, span(up)),
            None => t += &format!("Crashed: {}\n", self.time),
        }
        if self.safe_mode {
            match &self.brought_back {
                Some(b) => t += &format!("Safe mode: on, {} came back last\n", s.text(&b.label())),
                None => t += "Safe mode: on, nothing brought back\n",
            }
        }
        t += &format!("Part: {}\n", s.text(&self.part));
        t += &format!("Panic: {}\n", s.text(&self.message));
        t += &format!("At: {}\n", s.text(&self.at));
        if !self.ours.is_empty() {
            t += "\nWeir's code, innermost first:\n";
            for f in &self.ours {
                t += &format!("  {f}\n");
            }
        }
        if !self.settings.is_empty() {
            let named: Vec<String> = self
                .settings
                .iter()
                .map(|(k, v)| format!("{k} {}", s.text(v)))
                .collect();
            t += &format!("\nSettings that matter: {}\n", named.join(", "));
        }
        if !self.log.is_empty() {
            t += "\nLast lines of the log:\n";
            for l in &self.log {
                t += &format!("  {}\n", s.text(l));
            }
        }
        (t, s.hidden())
    }

    /// A title for an issue: what went wrong, and in which file.
    pub fn title(&self, hide: bool) -> String {
        let s = Scrub::new(&self.private, hide);
        let message: String = s.text(&self.message).chars().take(80).collect();
        let file = self
            .at
            .split(':')
            .next()
            .unwrap_or("")
            .rsplit(['/', '\\'])
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("/");
        if file.is_empty() || !self.at.contains("crates") {
            format!("Crash: {message}")
        } else {
            format!("Crash: {message} in {file}")
        }
    }
}

/// `secs` in words: `12 s`, `4 min`, `3 h 20 min`.
fn span(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        _ => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
    }
}

/// A link to a new crash report on `repository` (such as
/// `https://github.com/Mourty/Weir`), with `title` and the report `body`
/// filled in, and whether `body` fit in it. When it does not, the link
/// asks for it to be pasted, and the caller copies it.
pub fn issue_link(repository: &str, title: &str, body: &str) -> (String, bool) {
    use crate::report::percent_encode;
    let base = format!(
        "{}/issues/new?template=crash.yml&title={}",
        repository.trim_end_matches('/'),
        percent_encode(title)
    );
    let full = format!("{base}&report={}", percent_encode(body));
    if full.len() <= LINK_MOST {
        return (full, true);
    }
    let paste = "(Weir copied the report: paste it here.)";
    (format!("{base}&report={}", percent_encode(paste)), false)
}

/// The system, as a report names it: `Linux: Nobara Linux 44, KDE
/// (Plasma 6), Wayland`.
pub fn system() -> String {
    let mut parts = Vec::new();
    if let Ok(release) = std::fs::read_to_string("/etc/os-release") {
        if let Some(name) = release.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")) {
            parts.push(name.trim_matches('"').to_string());
        }
    }
    let desktop = Desktop::from_env();
    parts.extend(desktop.name);
    parts.extend(desktop.session);
    if parts.is_empty() {
        "Linux".into()
    } else {
        format!("Linux: {}", parts.join(", "))
    }
}

/// Weir's own functions in `backtrace`, innermost first, without the
/// panic's own machinery.
fn ours(backtrace: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in backtrace.lines() {
        let line = line.trim();
        let Some((number, name)) = line.split_once(": ") else {
            continue;
        };
        if number.trim().parse::<u32>().is_err() {
            continue;
        }
        let name = plain_name(name.trim());
        let own = ["weir::", "weir_"].iter().any(|p| name.starts_with(p));
        if !own || name.starts_with("weir_protocol::crash") {
            continue;
        }
        if out.last() != Some(&name) {
            out.push(name);
        }
        if out.len() == FRAMES {
            break;
        }
    }
    out
}

/// A function's name as a backtrace gives it, plainly: methods
/// (`<weir::app::App>::bus_sources`, `<weir::app::App as
/// eframe::App>::update`) as `weir::app::App::bus_sources`, without
/// closures (`{{closure}}`, `{closure#0}`) or generics, which only add
/// noise.
fn plain_name(name: &str) -> String {
    let name = match name.strip_prefix('<').and_then(|n| n.split_once(">::")) {
        Some((ty, method)) => {
            let ty = ty.split(" as ").next().unwrap_or(ty);
            format!("{ty}::{method}")
        }
        None => name.to_string(),
    };
    let name = name.split("::{").next().unwrap_or(&name);
    name.split('<').next().unwrap_or(name).to_string()
}

/// Hides what is personal in a report's text.
struct Scrub<'a> {
    p: &'a Private,
    hide: bool,
    user: Cell<bool>,
    names: RefCell<BTreeSet<String>>,
    links: Cell<usize>,
}

/// Where a path in running text ends.
const PATH_ENDS: [char; 8] = ['\n', '"', '\'', '`', ')', ']', '>', '\t'];

impl<'a> Scrub<'a> {
    fn new(p: &'a Private, hide: bool) -> Self {
        Self {
            p,
            hide,
            user: Cell::new(false),
            names: RefCell::new(BTreeSet::new()),
            links: Cell::new(0),
        }
    }

    fn hidden(&self) -> Hidden {
        Hidden {
            user: self.user.get(),
            names: self.names.borrow().len(),
            links: self.links.get(),
        }
    }

    fn home(&self) -> Option<&str> {
        self.p
            .home
            .as_deref()
            .map(|h| h.trim_end_matches('/'))
            .filter(|h| h.len() > 1)
    }

    /// A whole path in the home folder: `~`, then Weir's own places and
    /// hidden folders as they are, and other names hidden.
    fn path(&self, path: &str) -> String {
        let Some(rest) = self.home().and_then(|h| path.strip_prefix(h)) else {
            return path.to_string();
        };
        if !(rest.is_empty() || rest.starts_with('/')) {
            return path.to_string();
        }
        self.user.set(true);
        let own = rest
            .split('/')
            .find(|c| !c.is_empty())
            .is_some_and(|first| first.starts_with('.'))
            || rest.to_lowercase().contains("weir");
        if own {
            format!("~{}", self.user_words(rest))
        } else {
            format!("~{}", self.names(rest))
        }
    }

    /// Each name in `rest` hidden: `<folder>`, and `<file>.ext` for a last
    /// name with an extension.
    fn names(&self, rest: &str) -> String {
        let mut out = String::new();
        let parts: Vec<&str> = rest.split_inclusive('/').collect();
        for (i, part) in parts.iter().enumerate() {
            let (name, sep) = match part.strip_suffix('/') {
                Some(n) => (n, "/"),
                None => (*part, ""),
            };
            if name.is_empty() {
                out.push_str(part);
                continue;
            }
            self.names.borrow_mut().insert(name.to_string());
            let last = i + 1 == parts.len() && sep.is_empty();
            match name.rsplit_once('.').filter(|(stem, ext)| {
                last && !stem.is_empty()
                    && (1..=5).contains(&ext.len())
                    && ext.chars().all(|c| c.is_ascii_alphanumeric())
            }) {
                Some((_, ext)) => out.push_str(&format!("<file>.{ext}")),
                None => out.push_str("<folder>"),
            }
            out.push_str(sep);
        }
        out
    }

    /// The user name, where it is a word of its own.
    fn user_words(&self, text: &str) -> String {
        let Some(user) = self.p.user.as_deref().filter(|u| u.chars().count() >= 3) else {
            return text.to_string();
        };
        let mut out = String::new();
        let mut rest = text;
        while let Some(at) = rest.find(user) {
            let before = rest[..at].chars().next_back();
            let after = rest[at + user.len()..].chars().next();
            let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            out.push_str(&rest[..at]);
            if word(before) || word(after) {
                out.push_str(user);
            } else {
                self.user.set(true);
                out.push_str("<user>");
            }
            rest = &rest[at + user.len()..];
        }
        out.push_str(rest);
        out
    }

    /// Running text, where paths and links may be anywhere.
    fn text(&self, text: &str) -> String {
        if !self.hide {
            return text.to_string();
        }
        let mut t = self.links_hidden(text);
        if let Some(home) = self.home() {
            let mut out = String::new();
            let mut rest = t.as_str();
            while let Some(at) = rest.find(home) {
                out.push_str(&rest[..at]);
                let from = &rest[at..];
                let end = from.find(PATH_ENDS).unwrap_or(from.len());
                let end = from[..end].find(": ").unwrap_or(end);
                let end = from[..end].find(", ").unwrap_or(end);
                let path = from[..end].trim_end_matches([',', '.', ' ']);
                out.push_str(&self.path(path));
                rest = &from[path.len()..];
            }
            out.push_str(rest);
            t = out;
        }
        self.user_words(&t)
    }

    /// Web addresses cut back to their site.
    fn links_hidden(&self, text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        loop {
            let at = ["https://", "http://"]
                .iter()
                .filter_map(|s| rest.find(s).map(|i| (i, s.len())))
                .min();
            let Some((at, scheme)) = at else {
                out.push_str(rest);
                return out;
            };
            out.push_str(&rest[..at]);
            let from = &rest[at..];
            let end = from
                .find(|c: char| c.is_whitespace() || PATH_ENDS.contains(&c))
                .unwrap_or(from.len());
            let link = &from[..end];
            let site_end = link[scheme..].find(['/', '?', '#']).map(|i| i + scheme);
            match site_end {
                Some(i) if i + 1 < link.len() => {
                    self.links.set(self.links.get() + 1);
                    out.push_str(&link[..i]);
                    out.push_str("/…");
                }
                _ => out.push_str(link),
            }
            rest = &from[end..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            version: "1.3.0".into(),
            system: "Linux: Test".into(),
            time: "2026-11-03 21:42".into(),
            program: "window".into(),
            part: "main".into(),
            whole: true,
            message: "couldn't open /home/sam/Downloads/Airhorn loud.wav: gone".into(),
            at: "crates/gui/src/app/bus.rs:453:53".into(),
            ours: vec!["weir::app::App::bus_sources".into()],
            settings: vec![
                ("font".into(), "desktop".into()),
                ("folder".into(), "/home/sam/Music/Weir recordings".into()),
            ],
            log: vec![
                "21:42:03 INFO  see https://github.com/Mourty/Weir/releases/tag/v1.3.0 for sam"
                    .into(),
                "21:42:04 WARN  reading /home/sam/.config/weir/hotkeys.json".into(),
            ],
            private: Private {
                home: Some("/home/sam".into()),
                user: Some("sam".into()),
            },
            ..Default::default()
        }
    }

    #[test]
    fn personal_details_are_hidden_and_shown_when_asked() {
        let r = report();
        let (hidden, what) = r.text(true);
        assert!(!hidden.contains("sam"), "{hidden}");
        assert!(!hidden.contains("Airhorn"), "{hidden}");
        assert!(hidden.contains("~/<folder>/<file>.wav: gone"), "{hidden}");
        assert!(hidden.contains("~/.config/weir/hotkeys.json"), "{hidden}");
        // Weir's own places stay readable.
        assert!(
            hidden.contains("folder ~/Music/Weir recordings"),
            "{hidden}"
        );
        assert!(hidden.contains("https://github.com/…"), "{hidden}");
        assert!(hidden.contains("for <user>"), "{hidden}");
        assert!(what.user && what.links == 1 && what.names > 0, "{what:?}");
        let (raw, what) = r.text(false);
        assert!(raw.contains("/home/sam/Downloads/Airhorn loud.wav"));
        assert!(!what.any());
    }

    #[test]
    fn a_user_name_inside_another_word_stays() {
        let r = report();
        let s = Scrub::new(&r.private, true);
        assert_eq!(s.text("samples of sam"), "samples of <user>");
        assert_eq!(s.text("/opt/other/x.wav"), "/opt/other/x.wav");
    }

    #[test]
    fn a_long_report_is_left_to_paste() {
        let repo = "https://github.com/Mourty/Weir";
        let (link, fits) = issue_link(repo, "Crash: x", "short");
        assert!(fits && link.contains("template=crash.yml") && link.contains("report=short"));
        assert!(link.starts_with("https://github.com/Mourty/Weir/issues/new?"));
        let (link, fits) = issue_link(repo, "Crash: x", &"y".repeat(9000));
        assert!(!fits && link.len() < 1000);
    }

    #[test]
    fn own_frames_are_picked_from_a_backtrace() {
        let bt = "   0: std::panicking::begin_panic\n             at /rustc/x/library/std/src/panicking.rs:1:1\n   1: weir_protocol::crash::install::{{closure}}\n   2: weir::app::App::bus_sources\n             at ./crates/gui/src/app/bus.rs:453:53\n   3: weir::app::App::bus_sources\n   4: weir_daemon::controller::Controller::handle::<T>\n   5: weir_daemon::crash_on_purpose::{closure#0}\n   6: std::rt::lang_start";
        assert_eq!(
            ours(bt),
            [
                "weir::app::App::bus_sources",
                "weir_daemon::controller::Controller::handle::",
                "weir_daemon::crash_on_purpose",
            ]
        );
    }

    #[test]
    fn methods_are_named_plainly() {
        // As NeonLightning's backtrace in issue #33 gave them.
        let bt = "   4: <f32>::clamp\n   5: <weir::app::App>::bus_sources\n             at /home/n/Weir/crates/gui/src/app/bus.rs:453:53\n   6: <weir::app::App>::bus_band::{closure#0}::{closure#0}\n   7: <<weir::app::App>::bus_band::{closure#0}::{closure#0} as core::ops::function::FnOnce<(&mut egui::ui::Ui,)>>::call_once::{shim:vtable#0}\n  12: <weir::app::App>::bus_band::{closure#0}\n  19: <weir::app::App>::bus_band\n  56: <weir::app::App as eframe::epi::App>::update\n  84: weir::main";
        assert_eq!(
            ours(bt),
            [
                "weir::app::App::bus_sources",
                "weir::app::App::bus_band",
                "weir::app::App::update",
                "weir::main"
            ]
        );
    }

    #[test]
    fn the_title_names_the_file() {
        let r = report();
        assert_eq!(
            r.title(true),
            "Crash: couldn't open ~/<folder>/<file>.wav: gone in app/bus.rs"
        );
    }

    #[test]
    fn reports_are_kept_and_the_log_halved() {
        let dir = std::env::temp_dir().join(format!("weir-crash-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for i in 0..PENDING_KEPT + 2 {
            let mut r = report();
            r.message = format!("number {i}");
            keep(&dir, &r);
        }
        let kept = pending_in(&dir);
        assert_eq!(kept.len(), PENDING_KEPT);
        assert_eq!(
            kept.last().unwrap().message,
            format!("number {}", PENDING_KEPT + 1)
        );
        let log = std::fs::read_to_string(dir.join("crash.log")).unwrap();
        assert!(log.contains("Panic: number 0") && log.contains("The whole backtrace"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn safe_mode_answers_a_crash_at_startup_once() {
        let dir = std::env::temp_dir().join(format!("weir-safe-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let crash = |id: &str, program: &str, whole: bool, up: u64, safe: bool| Report {
            id: id.into(),
            program: program.into(),
            whole,
            uptime_s: Some(up),
            safe_mode: safe,
            ..report()
        };

        // Late, or only a part: not for safe mode.
        keep(&dir, &crash("w1", "window", true, 300, false));
        assert_eq!(safe_mode_wanted_in(&dir, "window"), None);
        keep(&dir, &crash("w2", "window", false, 3, false));
        assert_eq!(safe_mode_wanted_in(&dir, "window"), None);

        // Early: once, and only for its own program.
        keep(&dir, &crash("w3", "window", true, 3, false));
        keep(&dir, &crash("s1", "service", true, 600, false));
        assert_eq!(safe_mode_wanted_in(&dir, "service"), None);
        assert_eq!(safe_mode_wanted_in(&dir, "window").unwrap().id, "w3");
        assert_eq!(safe_mode_wanted_in(&dir, "window"), None, "answered");

        // In safe mode, however late; the audio part counts as ending.
        let mut audio = crash("s2", "service", false, 900, true);
        audio.part = "audio".into();
        audio.brought_back = Some(SafeItem::Strip("Mic".into()));
        keep(&dir, &audio);
        let wanted = safe_mode_wanted_in(&dir, "service").unwrap();
        assert_eq!(wanted.brought_back, Some(SafeItem::Strip("Mic".into())));
        let (text, _) = wanted.text(true);
        assert!(text.contains("15 min after starting"), "{text}");
        assert!(
            text.contains("Safe mode: on, Strip \"Mic\" came back last"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panic_is_caught_with_what_it_said_and_named_by_what_it_was_doing() {
        assert_eq!(caught(|| 5), Ok(5));
        let n = 7;
        assert_eq!(
            caught(|| -> () { panic!("bad {n}") }),
            Err("bad 7".to_string())
        );
        let v: Vec<u8> = Vec::new();
        assert!(caught(|| v[3]).unwrap_err().contains("out of bounds"));
        {
            let _outer = doing("request set_strip");
            {
                let _inner = doing("hotkey Talk");
                assert_eq!(
                    DOING.with(|d| d.borrow().clone()).as_deref(),
                    Some("hotkey Talk")
                );
            }
            assert_eq!(
                DOING.with(|d| d.borrow().clone()).as_deref(),
                Some("request set_strip")
            );
        }
        assert_eq!(DOING.with(|d| d.borrow().clone()), None);
    }

    #[test]
    fn a_poisoned_lock_still_locks() {
        let m = std::sync::Arc::new(Mutex::new(1));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("while holding it");
        })
        .join();
        assert!(m.lock().is_err());
        *m.locked() += 1;
        assert_eq!(*lock(&m), 2);
    }
}
