//! 进程级文件日志。UI 线程只做一次 `write`，不做轮转（按天文件在 M1 补）。

use crate::paths;
use chrono::Local;
use log::{Level, LevelFilter, Log, Metadata, Record};
use parking_lot::Mutex;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::sync::OnceLock;

struct FileLogger {
    level: LevelFilter,
    file: Mutex<File>,
}

static LOGGER: OnceLock<FileLogger> = OnceLock::new();

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} [{:<5}] {}: {}\n",
            Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            record.level(),
            record.target(),
            record.args()
        );
        if let Some(mut file) = self.file.try_lock() {
            let _ = file.write_all(line.as_bytes());
        }
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
    }

    fn flush(&self) {
        if let Some(mut file) = self.file.try_lock() {
            let _ = file.flush();
        }
    }
}

/// 初始化日志：写 `%APPDATA%/RustDb/logs/rustdb.log`，并安装 panic hook。
pub fn init(level: LevelFilter) {
    let path = match paths::ensure_data_dir() {
        Ok(dir) => dir.join("logs").join("rustdb.log"),
        Err(_) => std::env::temp_dir().join("rustdb.log"),
    };
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap_or_else(|_| File::create(std::env::temp_dir().join("rustdb.log")).unwrap());

    let _ = LOGGER.set(FileLogger {
        level,
        file: Mutex::new(file),
    });
    if let Some(logger) = LOGGER.get() {
        let _ = log::set_logger(logger);
        log::set_max_level(level);
    }

    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}");
        let _ = std::io::stderr().flush();
    }));
    log::info!("RustDb 启动，日志级别 {level}");
}

/// 当前日志文件路径（诊断页展示用）。
pub fn log_path() -> std::path::PathBuf {
    paths::logs_dir().join("rustdb.log")
}

pub fn level_filter_from_str(s: &str) -> LevelFilter {
    match s.to_ascii_lowercase().as_str() {
        "trace" => LevelFilter::Trace,
        "debug" => LevelFilter::Debug,
        "warn" => LevelFilter::Warn,
        "error" => LevelFilter::Error,
        _ => LevelFilter::Info,
    }
}

pub fn level_tag(level: Level) -> &'static str {
    match level {
        Level::Error => "error",
        Level::Warn => "warn",
        Level::Info => "info",
        Level::Debug => "debug",
        Level::Trace => "trace",
    }
}
