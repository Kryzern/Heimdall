//! File search off the UI thread. Talks to `heimdall-indexer` when it is
//! installed; otherwise keeps a local index built by walking the drives.
//! Only the newest query is ever answered, so fast typing never queues work.

use heimdall_core::proto::Client;
use heimdall_core::{ntfs, snapshot, walk, Query, Results, Searcher, Volume};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const WM_FILES_DONE: u32 = 0x8000 + 21;
const REBUILD_EVERY: Duration = Duration::from_secs(30 * 60);

#[derive(Default)]
pub struct Status {
    pub service: AtomicBool,
    pub building: AtomicBool,
    pub entries: AtomicU64,
}

struct Local {
    vols: RwLock<Vec<Volume>>,
    searcher: Searcher,
    started: AtomicBool,
}

pub struct FileSearch {
    tx: Sender<(u64, String)>,
    pub rx: Receiver<(u64, Results)>,
    pub status: Arc<Status>,
    local: Arc<Local>,
}

fn snapshot_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Heimdall")
        .join("index.bin")
}

impl FileSearch {
    pub fn start(hwnd: HWND) -> FileSearch {
        let (tx, qrx) = channel::<(u64, String)>();
        let (rtx, rx) = channel();
        let status = Arc::new(Status::default());
        let local = Arc::new(Local { vols: RwLock::new(Vec::new()), searcher: Searcher::new(), started: AtomicBool::new(false) });
        let hwnd = hwnd.0 as isize;

        let (st, lc) = (status.clone(), local.clone());
        std::thread::Builder::new()
            .name("files".into())
            .spawn(move || {
                let mut client = Client::connect().ok();
                if client.is_some() {
                    st.service.store(true, Ordering::Relaxed);
                    if let Some(s) = client.as_mut().and_then(|c| c.status().ok()) {
                        st.entries.store(s.entries, Ordering::Relaxed);
                    }
                } else {
                    start_local(lc.clone(), st.clone());
                }
                while let Ok((mut id, mut q)) = qrx.recv() {
                    while let Ok((i, nq)) = qrx.try_recv() {
                        id = i;
                        q = nq;
                    }
                    let mut res = None;
                    if client.is_none() {
                        // Cheap when the pipe does not exist; picks up a freshly installed service.
                        client = Client::connect().ok();
                    }
                    if let Some(c) = client.as_mut() {
                        match c.search(&q, 40) {
                            Ok(r) => {
                                st.service.store(true, Ordering::Relaxed);
                                res = Some(r);
                            }
                            Err(_) => {
                                // Service went away: reconnect once, else go local.
                                client = Client::connect().ok();
                                res = client.as_mut().and_then(|c| c.search(&q, 40).ok());
                                if client.is_none() {
                                    st.service.store(false, Ordering::Relaxed);
                                    start_local(lc.clone(), st.clone());
                                }
                            }
                        }
                    }
                    let res = res.unwrap_or_else(|| {
                        let vols = lc.vols.read().unwrap();
                        lc.searcher.search(&vols, &Query::parse(&q), 40)
                    });
                    if rtx.send((id, res)).is_err() {
                        break;
                    }
                    unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_FILES_DONE, WPARAM(0), LPARAM(0)).ok() };
                }
            })
            .ok();

        FileSearch { tx, rx, status, local }
    }

    pub fn query(&self, id: u64, q: &str) {
        self.tx.send((id, q.to_string())).ok();
    }

    /// Force a local rebuild (no-op when the service is in charge).
    pub fn rebuild(&self) {
        if !self.status.service.load(Ordering::Relaxed) {
            let (lc, st) = (self.local.clone(), self.status.clone());
            std::thread::spawn(move || build_all(&lc, &st));
        }
    }

    pub fn ready(&self) -> bool {
        self.status.service.load(Ordering::Relaxed) || self.status.entries.load(Ordering::Relaxed) > 0
    }
}

fn start_local(lc: Arc<Local>, st: Arc<Status>) {
    if lc.started.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::Builder::new()
        .name("local-index".into())
        .spawn(move || {
            // A recent snapshot is good enough to serve; skip the first walk.
            let mut fresh = std::fs::metadata(snapshot_path())
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < REBUILD_EVERY);
            match snapshot::load(&snapshot_path()) {
                Ok(v) => {
                    st.entries.store(v.iter().map(|v| v.live as u64).sum(), Ordering::Relaxed);
                    *lc.vols.write().unwrap() = v;
                }
                Err(_) => fresh = false,
            }
            loop {
                if !std::mem::take(&mut fresh) {
                    build_all(&lc, &st);
                }
                std::thread::sleep(REBUILD_EVERY);
                if Client::connect().is_ok() {
                    // The service took over; give the memory back.
                    st.service.store(true, Ordering::Relaxed);
                    lc.vols.write().unwrap().clear();
                    lc.started.store(false, Ordering::Relaxed);
                    return;
                }
            }
        })
        .ok();
}

fn build_all(lc: &Local, st: &Status) {
    if st.building.swap(true, Ordering::Relaxed) {
        return;
    }
    for letter in ntfs::fixed_drives() {
        let root = format!("{letter}:\\");
        let v = match ntfs::build(letter) {
            Ok(v) => v,
            Err(_) => walk::build(&root),
        };
        let mut g = lc.vols.write().unwrap();
        match g.iter().position(|x| x.root == root) {
            Some(i) => g[i] = v,
            None => g.push(v),
        }
        st.entries.store(g.iter().map(|v| v.live as u64).sum(), Ordering::Relaxed);
    }
    {
        let mut g = lc.vols.write().unwrap();
        snapshot::save(&snapshot_path(), &mut g).ok();
    }
    st.building.store(false, Ordering::Relaxed);
}
