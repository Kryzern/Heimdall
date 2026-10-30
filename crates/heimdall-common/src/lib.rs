//! Launcher logic that doesn't depend on the platform: the config file,
//! result rows and fuzzy matching, the search field, usage history and pins,
//! and quick notes. Shared by the Windows and macOS front ends.

pub mod config;
pub mod edit;
pub mod history;
pub mod items;
pub mod notes;

/// Append a line to `heimdall.log` next to the config.
pub fn log(msg: impl AsRef<str>) {
    use std::io::Write;
    let dir = config::dir();
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("heimdall.log");
    if std::fs::metadata(&path).map(|m| m.len() > 512 << 10).unwrap_or(false) {
        std::fs::remove_file(&path).ok();
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        writeln!(f, "[{}] {}", heimdall_core::query::now_unix(), msg.as_ref()).ok();
    }
}

/// The user's home folder.
pub fn home() -> std::path::PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir)
}
