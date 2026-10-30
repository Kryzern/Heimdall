//! File search on macOS. There is no master file table to read, so the home
//! folder is walked in parallel into the same index the Windows indexer
//! builds, saved to a snapshot so the next launch can search straight away,
//! and walked again in the background every so often.
//!
//! The index stores paths the Windows way (`\Users\ethan\Documents\x.pdf`) so
//! the shared query language and ranking work unchanged; results are turned
//! back into `/` paths on the way out.

use heimdall_core::index::{Source, Volume, DIR, HIDDEN, NONE};
use heimdall_core::{snapshot, Query, Results, Searcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Walk again when a search comes in and the index is older than this.
const STALE: Duration = Duration::from_secs(10 * 60);

enum Msg {
    Query(u64, String),
    Rebuild,
}

pub struct FileSearch {
    tx: Sender<Msg>,
    pub rx: Receiver<(u64, Results)>,
    ready: Arc<AtomicBool>,
}

impl FileSearch {
    pub fn start() -> FileSearch {
        let (tx, jobs) = channel();
        let (done, rx) = channel();
        let ready = Arc::new(AtomicBool::new(false));
        let r = ready.clone();
        let me = tx.clone();
        std::thread::Builder::new().name("files".into()).spawn(move || worker(jobs, me, done, r)).ok();
        FileSearch { tx, rx, ready }
    }

    pub fn query(&self, id: u64, q: &str) {
        self.tx.send(Msg::Query(id, q.to_string())).ok();
    }

    pub fn rebuild(&self) {
        self.tx.send(Msg::Rebuild).ok();
    }

    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }
}

fn snapshot_path() -> PathBuf {
    heimdall_common::home().join("Library/Caches/Heimdall/index.bin")
}

/// `/Users/ethan` -> `\Users\ethan\`
pub fn to_index_path(p: &str) -> String {
    let mut s = p.replace('/', "\\");
    if !s.ends_with('\\') {
        s.push('\\');
    }
    s
}

/// `\Users\ethan\x.pdf` -> `/Users/ethan/x.pdf`
pub fn from_index_path(p: &str) -> String {
    p.replace('\\', "/")
}

fn worker(jobs: Receiver<Msg>, me: Sender<Msg>, done: Sender<(u64, Results)>, ready: Arc<AtomicBool>) {
    let vols: Arc<Mutex<Vec<Volume>>> = Arc::new(Mutex::new(snapshot::load(&snapshot_path()).unwrap_or_default()));
    if !vols.lock().unwrap().is_empty() {
        ready.store(true, Ordering::Relaxed);
    }
    let walking = Arc::new(AtomicBool::new(false));
    let mut built: Option<Instant> = None;
    let searcher = Searcher::new();
    let walk = |vols: &Arc<Mutex<Vec<Volume>>>| {
        if walking.swap(true, Ordering::Relaxed) {
            return;
        }
        let (vols, walking, ready, me) = (vols.clone(), walking.clone(), ready.clone(), me.clone());
        std::thread::Builder::new()
            .name("walk".into())
            .spawn(move || {
                let t = Instant::now();
                let mut fresh = vec![build(&heimdall_common::home())];
                let n = fresh[0].live;
                if let Some(dir) = snapshot_path().parent() {
                    std::fs::create_dir_all(dir).ok();
                }
                snapshot::save(&snapshot_path(), &mut fresh).ok();
                *vols.lock().unwrap() = fresh;
                ready.store(true, Ordering::Relaxed);
                walking.store(false, Ordering::Relaxed);
                heimdall_common::log(format!("indexed {n} entries in {:.1}s", t.elapsed().as_secs_f32()));
                // Wake the loop so a stale query isn't left unanswered.
                me.send(Msg::Query(u64::MAX, String::new())).ok();
            })
            .ok();
    };
    walk(&vols);
    built = built.or(Some(Instant::now()));
    while let Ok(mut msg) = jobs.recv() {
        // Only the newest query matters.
        while let Ok(next) = jobs.try_recv() {
            msg = next;
        }
        match msg {
            Msg::Rebuild => {
                walk(&vols);
                built = Some(Instant::now());
            }
            Msg::Query(id, q) => {
                if built.is_some_and(|b| b.elapsed() > STALE) {
                    walk(&vols);
                    built = Some(Instant::now());
                }
                if id == u64::MAX {
                    continue;
                }
                let r = searcher.search(&vols.lock().unwrap(), &Query::parse(&q), 60);
                if done.send((id, r)).is_err() {
                    break;
                }
            }
        }
    }
}

/// Folders under the home folder that are left out: app data and caches,
/// which Spotlight skips too, and the Trash.
const SKIP_IN_HOME: &[&str] = &["Library", ".Trash"];

struct Found {
    rec: u32,
    parent: u32,
    flags: u16,
    name: String,
}

/// Walk `root` into a volume. Symlinks are listed but never followed.
pub fn build(root: &Path) -> Volume {
    const ROOT_REC: u32 = 0;
    let next = AtomicU32::new(ROOT_REC + 1);
    let batches: Mutex<Vec<Vec<Found>>> = Mutex::new(Vec::new());

    fn visit<'s>(s: &rayon::Scope<'s>, dir: PathBuf, rec: u32, top: bool, next: &'s AtomicU32, batches: &'s Mutex<Vec<Vec<Found>>>) {
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        let mut batch = Vec::new();
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if top && SKIP_IN_HOME.contains(&name.as_str()) {
                continue;
            }
            let Ok(ft) = ent.file_type() else { continue };
            let id = next.fetch_add(1, Ordering::Relaxed);
            let mut flags = 0;
            if ft.is_dir() {
                flags |= DIR;
                let path = ent.path();
                s.spawn(move |s| visit(s, path, id, false, next, batches));
            }
            if name.starts_with('.') {
                flags |= HIDDEN;
            }
            batch.push(Found { rec: id, parent: rec, flags, name });
        }
        if !batch.is_empty() {
            batches.lock().unwrap().push(batch);
        }
    }

    // A private pool: listing folders blocks on I/O and must never starve
    // searches running on the global pool.
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 12);
    let start = root.to_path_buf();
    match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(pool) => pool.scope(|s| visit(s, start, ROOT_REC, true, &next, &batches)),
        Err(_) => rayon::scope(|s| visit(s, start, ROOT_REC, true, &next, &batches)),
    }

    let batches = batches.into_inner().unwrap();
    let n: usize = batches.iter().map(|b| b.len()).sum();
    let bytes: usize = batches.iter().flatten().map(|f| f.name.len() + 1).sum();
    let mut v = Volume::new(to_index_path(&root.to_string_lossy()), ROOT_REC, Source::Walk).with_capacity(n, bytes + 1);
    v.rec_map = vec![NONE; next.load(Ordering::Relaxed) as usize];
    for f in batches.into_iter().flatten() {
        v.push(f.rec, f.parent, &f.name, f.flags);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_and_finds() {
        let root = std::env::temp_dir().join(format!("heimdall-walk-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Documents/Projects")).unwrap();
        std::fs::create_dir_all(root.join("Library/Caches")).unwrap();
        std::fs::write(root.join("Documents/Projects/report.pdf"), b"x").unwrap();
        std::fs::write(root.join("Library/Caches/report.pdf"), b"x").unwrap();
        let v = build(&root);
        let r = Searcher::new().search(&[v], &Query::parse("report"), 10);
        std::fs::remove_dir_all(&root).ok();
        let paths: Vec<String> = r.hits.iter().map(|h| from_index_path(&h.path)).collect();
        assert_eq!(paths.len(), 1, "{paths:?}");
        assert!(paths[0].ends_with("/Documents/Projects/report.pdf"), "{paths:?}");
        assert!(paths[0].starts_with(&root.to_string_lossy().replace('\\', "/")), "{paths:?}");
    }

    #[test]
    fn path_conversion() {
        assert_eq!(to_index_path("/Users/ethan"), "\\Users\\ethan\\");
        assert_eq!(from_index_path("\\Users\\ethan\\a.txt"), "/Users/ethan/a.txt");
    }
}
