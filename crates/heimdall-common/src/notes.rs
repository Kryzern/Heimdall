//! Quick notes: `note call the dentist` appends a timestamped Markdown list
//! item to one file, by default `Documents/Heimdall notes.md`.

use crate::config::Config;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The configured notes file (a leading `~` is the home folder), or the default.
pub fn path(cfg: &Config) -> PathBuf {
    let set = cfg.notes.trim();
    if set.is_empty() {
        return crate::home().join("Documents").join("Heimdall notes.md");
    }
    match set.strip_prefix("~/").or_else(|| set.strip_prefix("~\\")) {
        Some(rest) => crate::home().join(rest),
        None => PathBuf::from(set),
    }
}

/// Append `text` as `- <stamp> <text>`. `stamp` is the local time, which each
/// platform formats itself, e.g. "2026-10-30 14:05".
pub fn append(cfg: &Config, stamp: &str, text: &str) -> std::io::Result<()> {
    let path = path(cfg);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    // Start on a fresh line if the file doesn't end with one.
    let lead = if f.metadata()?.len() > 0 && !ends_with_newline(&path) { "\n" } else { "" };
    writeln!(f, "{lead}- {stamp} {text}")
}

fn ends_with_newline(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let mut last = [0u8; 1];
    std::fs::File::open(path)
        .and_then(|mut f| {
            f.seek(SeekFrom::End(-1))?;
            f.read_exact(&mut last)
        })
        .map(|_| last[0] == b'\n')
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_lines() {
        let file = std::env::temp_dir().join(format!("heimdall-notes-{}.md", std::process::id()));
        std::fs::write(&file, "# Inbox").unwrap();
        let cfg = Config { notes: file.to_string_lossy().into_owned(), ..Config::default() };
        append(&cfg, "2026-10-30 14:05", "first").unwrap();
        append(&cfg, "2026-10-30 14:06", "second").unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::remove_file(&file).ok();
        assert_eq!(text, "# Inbox\n- 2026-10-30 14:05 first\n- 2026-10-30 14:06 second\n");
    }
}
