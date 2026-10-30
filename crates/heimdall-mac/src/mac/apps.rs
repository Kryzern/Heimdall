//! Installed applications: every `.app` bundle in the usual Applications
//! folders, one level of subfolders deep (`/Applications/Utilities`, vendor
//! folders like `/Applications/Adobe Photoshop 2026`).

use heimdall_common::items::Target;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct App {
    pub name: String,
    /// Path of the `.app` bundle.
    pub path: String,
    pub target: Target,
}

pub fn enumerate() -> Vec<App> {
    let home = heimdall_common::home();
    let roots = [
        "/Applications".into(),
        "/System/Applications".into(),
        "/System/Library/CoreServices/Applications".into(),
        home.join("Applications").to_string_lossy().into_owned(),
    ];
    let mut out = Vec::new();
    for root in &roots {
        scan(Path::new(root), 0, &mut out);
    }
    // Finder lives on its own.
    let finder = "/System/Library/CoreServices/Finder.app";
    if Path::new(finder).exists() {
        out.push(app("Finder".into(), finder.into()));
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

fn scan(dir: &Path, depth: u32, out: &mut Vec<App>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        if let Some(stem) = name.strip_suffix(".app") {
            out.push(app(stem.to_string(), path.to_string_lossy().into_owned()));
        } else if depth == 0 && e.file_type().is_ok_and(|t| t.is_dir()) {
            scan(&path, depth + 1, out);
        }
    }
}

fn app(name: String, path: String) -> App {
    App { target: Target::new(&name), name, path }
}
