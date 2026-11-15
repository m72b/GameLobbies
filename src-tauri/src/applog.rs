//! The log file: `<data dir>/logs/game-lobby.log`, a small logger on the `log` crate.
//!
//! - Rotation: the file is rotated at [`MAX_BYTES`]; [`MAX_FILES`] files are kept in total
//!   (`game-lobby.log`, `game-lobby.log.1`, `game-lobby.log.2`), so the log never exceeds
//!   about 3 MB.
//! - Every line is passed through [`crate::redact`] before it is written, as a second line of
//!   defence on top of the rule that secrets are never formatted into log messages.
//! - Our own crate logs from Info up (Debug in debug builds); dependencies from Warn up.
//! - Identical consecutive messages (a poll that fails every few seconds) are collapsed.
//! - A panic hook records panics here before the default hook runs.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const LOG_DIR: &str = "logs";
pub const LOG_FILE: &str = "game-lobby.log";
/// A file is rotated once it would grow beyond this many bytes.
pub const MAX_BYTES: u64 = 1_000_000;
/// Files kept in total, the live one included.
pub const MAX_FILES: usize = 3;

/// The folder the log files live in.
pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_DIR)
}

/// `n == 0` is the live file; `n >= 1` the n-th rotated one.
fn file_path(dir: &Path, n: usize) -> PathBuf {
    if n == 0 {
        dir.join(LOG_FILE)
    } else {
        dir.join(format!("{LOG_FILE}.{n}"))
    }
}

/// An append-only file that rotates by size.
struct RotatingFile {
    dir: PathBuf,
    max_bytes: u64,
    files: usize,
    file: Option<File>,
    size: u64,
}

impl RotatingFile {
    fn open(dir: &Path, max_bytes: u64, files: usize) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let mut log = Self {
            dir: dir.to_path_buf(),
            max_bytes,
            files: files.max(1),
            file: None,
            size: 0,
        };
        log.reopen()?;
        Ok(log)
    }

    fn reopen(&mut self) -> io::Result<()> {
        let path = file_path(&self.dir, 0);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        self.size = file.metadata().map(|m| m.len()).unwrap_or(0);
        self.file = Some(file);
        Ok(())
    }

    /// Shifts `log` -> `log.1` -> `log.2`, dropping the oldest, and starts a new live file.
    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        let _ = fs::remove_file(file_path(&self.dir, self.files - 1));
        for n in (0..self.files - 1).rev() {
            let from = file_path(&self.dir, n);
            if from.exists() {
                let _ = fs::rename(&from, file_path(&self.dir, n + 1));
            }
        }
        // With a single file there is nothing to shift to: start over.
        if self.files == 1 {
            let _ = fs::remove_file(file_path(&self.dir, 0));
        }
        self.reopen()
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        let bytes = line.len() as u64 + 1;
        if self.size > 0 && self.size + bytes > self.max_bytes {
            self.rotate()?;
        }
        if self.file.is_none() {
            self.reopen()?;
        }
        let file = self.file.as_mut().expect("log file is open");
        let mut buf = Vec::with_capacity(line.len() + 1);
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
        file.write_all(&buf)?;
        self.size += bytes;
        Ok(())
    }
}

struct Sink {
    file: RotatingFile,
    /// The previous message (level, target, text) and how many times it repeated since.
    last: Option<(String, String)>,
    repeats: u32,
}

impl Sink {
    fn emit(&mut self, level: log::Level, target: &str, message: &str) {
        let key = format!("{level} {target}");
        if self
            .last
            .as_ref()
            .is_some_and(|(k, m)| *k == key && m == message)
        {
            self.repeats += 1;
            return;
        }
        self.flush_repeats();
        self.write(&format!("{} {level:<5} {target} {message}", timestamp()));
        self.last = Some((key, message.to_string()));
    }

    fn flush_repeats(&mut self) {
        if self.repeats > 0 {
            let line = format!(
                "{} INFO  log (previous message repeated {} more time{})",
                timestamp(),
                self.repeats,
                if self.repeats == 1 { "" } else { "s" }
            );
            self.repeats = 0;
            self.write(&line);
        }
    }

    fn write(&mut self, line: &str) {
        // Newlines inside a message stay readable but never forge a new log entry.
        let line = line.replace('\r', "").replace('\n', "\n    ");
        let line = crate::redact::redact(&line);
        let _ = self.file.write_line(&line);
        if cfg!(debug_assertions) {
            let _ = writeln!(io::stderr(), "{line}");
        }
    }
}

struct AppLogger {
    sink: Mutex<Sink>,
    max_level: log::LevelFilter,
}

impl AppLogger {
    fn sink(&self) -> MutexGuard<'_, Sink> {
        self.sink.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl log::Log for AppLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        level_allowed(metadata.target(), metadata.level(), self.max_level)
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            self.sink()
                .emit(record.level(), record.target(), &record.args().to_string());
        }
    }

    fn flush(&self) {
        self.sink().flush_repeats();
    }
}

/// Our own crate (and panics) log up to `max`; every dependency only warnings and errors.
fn level_allowed(target: &str, level: log::Level, max: log::LevelFilter) -> bool {
    let ours = target.starts_with("game_lobby") || target == "panic";
    level
        <= if ours {
            max
        } else {
            max.min(log::LevelFilter::Warn)
        }
}

static LOGGER: OnceLock<AppLogger> = OnceLock::new();

/// Starts file logging into `<data_dir>/logs` and installs the panic hook. Call once, early,
/// after the single-instance check. Failing to open the file is not fatal: the app keeps
/// running without a log.
pub fn init(data_dir: &Path) -> io::Result<()> {
    let max_level = if cfg!(debug_assertions) {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    let file = RotatingFile::open(&log_dir(data_dir), MAX_BYTES, MAX_FILES)?;
    let logger = LOGGER.get_or_init(|| AppLogger {
        sink: Mutex::new(Sink {
            file,
            last: None,
            repeats: 0,
        }),
        max_level,
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(max_level);
        install_panic_hook();
    }
    Ok(())
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!(target: "panic", "{}", describe_panic(info));
        if let Some(logger) = LOGGER.get() {
            log::Log::flush(logger);
        }
        previous(info);
    }));
}

fn describe_panic(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(non-text panic payload)".into());
    let location = info
        .location()
        .map(|l| format!("{}:{}", l.file(), l.line()))
        .unwrap_or_else(|| "unknown location".into());
    let thread = std::thread::current();
    format!(
        "panic in thread '{}' at {location}: {message}",
        thread.name().unwrap_or("unnamed")
    )
}

/// The last `max` lines of the log, oldest first, reading rotated files when the live one is
/// short. Missing files and unreadable bytes are tolerated.
pub fn tail_lines(data_dir: &Path, max: usize) -> Vec<String> {
    let dir = log_dir(data_dir);
    let mut lines: Vec<String> = Vec::new();
    for n in 0..MAX_FILES {
        if lines.len() >= max {
            break;
        }
        let Ok(bytes) = fs::read(file_path(&dir, n)) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut older: Vec<String> = text.lines().map(str::to_string).collect();
        older.append(&mut lines);
        lines = older;
    }
    let skip = lines.len().saturating_sub(max);
    lines.split_off(skip)
}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_utc(now.as_secs(), now.subsec_millis())
}

/// `2026-11-15T12:34:56.789Z` from seconds since the Unix epoch.
fn format_utc(secs: u64, millis: u32) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days (proleptic Gregorian), after Howard Hinnant.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gl-log-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn rotation_settings_are_about_one_megabyte_by_three_files() {
        assert_eq!(MAX_BYTES, 1_000_000);
        assert_eq!(MAX_FILES, 3);
        assert_eq!(
            file_path(Path::new("logs"), 0),
            Path::new("logs").join("game-lobby.log")
        );
        assert_eq!(
            file_path(Path::new("logs"), 2),
            Path::new("logs").join("game-lobby.log.2")
        );
        assert_eq!(log_dir(Path::new("data")), Path::new("data").join("logs"));
    }

    #[test]
    fn the_log_rotates_by_size_and_keeps_only_the_newest_files() {
        let dir = temp_dir();
        let mut log = RotatingFile::open(&dir, 100, 3).unwrap();
        for i in 0..40 {
            log.write_line(&format!("line {i:02} {}", "x".repeat(20)))
                .unwrap();
        }
        assert_eq!(
            names(&dir),
            ["game-lobby.log", "game-lobby.log.1", "game-lobby.log.2"]
        );
        for n in 0..3 {
            let len = fs::metadata(file_path(&dir, n)).unwrap().len();
            assert!(len <= 100, "file {n} is {len} bytes");
        }
        // Newest lines are in the live file; the oldest lines are gone.
        let live = fs::read_to_string(file_path(&dir, 0)).unwrap();
        assert!(live.contains("line 39"));
        let all: String = (0..3)
            .map(|n| fs::read_to_string(file_path(&dir, n)).unwrap())
            .collect();
        assert!(!all.contains("line 00"));
        assert!(all.contains("line 35"));
        // Reopening appends to the live file instead of truncating it.
        drop(log);
        let mut log = RotatingFile::open(&dir, 100, 3).unwrap();
        log.write_line("after restart").unwrap();
        assert!(fs::read_to_string(file_path(&dir, 0))
            .unwrap()
            .contains("after restart"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_single_file_setting_never_keeps_old_files() {
        let dir = temp_dir();
        let mut log = RotatingFile::open(&dir, 50, 1).unwrap();
        for i in 0..10 {
            log.write_line(&format!("line {i} {}", "y".repeat(30)))
                .unwrap();
        }
        assert_eq!(names(&dir), ["game-lobby.log"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_reads_across_rotated_files_in_order() {
        let dir = temp_dir();
        let mut log = RotatingFile::open(&log_dir(&dir), 100, 3).unwrap();
        for i in 0..12 {
            log.write_line(&format!("entry {i:02}")).unwrap();
        }
        let tail = tail_lines(&dir, 5);
        assert_eq!(
            tail,
            ["entry 07", "entry 08", "entry 09", "entry 10", "entry 11"]
        );
        assert!(tail_lines(&dir, 1000).len() <= 12);
        assert!(tail_lines(&dir.join("missing"), 10).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sinks_redact_collapse_repeats_and_keep_entries_on_one_line_each() {
        let dir = temp_dir();
        let mut sink = Sink {
            file: RotatingFile::open(&dir, MAX_BYTES, MAX_FILES).unwrap(),
            last: None,
            repeats: 0,
        };
        sink.emit(
            log::Level::Warn,
            "game_lobby_lib::x",
            "request failed: client_secret=abcdefghij0123456789abcdefghij",
        );
        for _ in 0..3 {
            sink.emit(log::Level::Warn, "game_lobby_lib::x", "poll failed");
        }
        sink.emit(
            log::Level::Info,
            "game_lobby_lib::x",
            "line one\nFAKE entry",
        );
        sink.flush_repeats();
        let text = fs::read_to_string(file_path(&dir, 0)).unwrap();
        assert!(!text.contains("abcdefghij0123456789abcdefghij"));
        assert!(text.contains("client_secret=[redacted]"));
        assert_eq!(text.matches("poll failed").count(), 1);
        assert!(text.contains("repeated 2 more times"));
        assert!(text.contains("\n    FAKE entry"));
        assert!(text
            .lines()
            .all(|l| l.starts_with("20") || l.starts_with("    ")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dependencies_log_only_warnings_and_our_crate_logs_info() {
        use log::{Level, LevelFilter};
        let max = LevelFilter::Info;
        assert!(level_allowed("game_lobby_lib::db", Level::Info, max));
        assert!(!level_allowed("game_lobby_lib::db", Level::Debug, max));
        assert!(!level_allowed("reqwest::connect", Level::Info, max));
        assert!(level_allowed("hyper::client", Level::Warn, max));
        assert!(level_allowed("panic", Level::Error, max));
    }

    #[test]
    fn timestamps_are_utc_iso() {
        assert_eq!(format_utc(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_utc(951_782_400, 5), "2000-02-29T00:00:00.005Z");
        assert_eq!(format_utc(1_794_141_296, 789), "2026-11-08T12:34:56.789Z");
    }
}
