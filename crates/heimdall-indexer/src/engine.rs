use crate::{data_dir, log};
use heimdall_core::index::Source;
use heimdall_core::{ntfs, snapshot, walk, Searcher, Volume};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

pub struct Engine {
    pub vols: RwLock<Vec<Volume>>,
    pub searcher: Searcher,
    dirty: AtomicBool,
}

const SAVE_EVERY: Duration = Duration::from_secs(10 * 60);
const RESCAN_EVERY: Duration = Duration::from_secs(30 * 60);

impl Engine {
    pub fn start() -> Arc<Engine> {
        let t0 = Instant::now();
        let mut loaded = snapshot::load(&data_dir().join("index.bin")).unwrap_or_default();
        log(format!("snapshot: {} volume(s) in {:?}", loaded.len(), t0.elapsed()));

        let mut slots = Vec::new();
        let mut jobs = Vec::new();
        for letter in ntfs::fixed_drives() {
            let root = format!("{letter}:\\");
            let is_ntfs = ntfs::filesystem(&root).is_some_and(|(fs, _)| fs == "NTFS");
            let prev = loaded.iter().position(|v| v.root == root).map(|i| loaded.swap_remove(i));
            let (vol, resume) = match prev {
                Some(v) if is_ntfs && v.source == Source::Ntfs && ntfs::resumable(&v) => (v, true),
                // Walk-built snapshot: serve it now, refresh in the background.
                Some(v) if !is_ntfs => (v, false),
                _ => (Volume::new(root.clone(), 0, Source::Walk), false),
            };
            jobs.push((slots.len(), letter, is_ntfs, resume));
            slots.push(vol);
        }

        let engine = Arc::new(Engine { vols: RwLock::new(slots), searcher: Searcher::new(), dirty: AtomicBool::new(false) });
        for (slot, letter, is_ntfs, resume) in jobs {
            let e = engine.clone();
            std::thread::Builder::new()
                .name(format!("vol-{letter}"))
                .spawn(move || if is_ntfs { e.run_ntfs(slot, letter, resume) } else { e.run_walk(slot, letter) })
                .ok();
        }
        let e = engine.clone();
        std::thread::Builder::new()
            .name("saver".into())
            .spawn(move || loop {
                std::thread::sleep(SAVE_EVERY);
                e.save_if_dirty();
            })
            .ok();
        engine
    }

    fn replace(&self, slot: usize, v: Volume) {
        self.vols.write().unwrap()[slot] = v;
        self.dirty.store(true, Ordering::Relaxed);
    }

    fn run_ntfs(&self, slot: usize, letter: char, mut resume: bool) {
        loop {
            if !resume {
                let t0 = Instant::now();
                match ntfs::build(letter) {
                    Ok(v) => {
                        log(format!(
                            "{letter}: MFT indexed {} entries in {:?} ({} MB)",
                            v.live,
                            t0.elapsed(),
                            v.memory_bytes() >> 20
                        ));
                        self.replace(slot, v);
                    }
                    Err(e) => {
                        log(format!("{letter}: MFT read failed ({e}); falling back to a directory walk"));
                        return self.run_walk(slot, letter);
                    }
                }
            }
            resume = false;
            background_mode();
            let (jid, next) = {
                let g = self.vols.read().unwrap();
                (g[slot].journal_id, g[slot].next_usn)
            };
            let err = ntfs::watch(letter, jid, next, |changes, next| {
                if changes.is_empty() {
                    return;
                }
                let mut g = self.vols.write().unwrap();
                ntfs::apply(&mut g[slot], changes, next);
                self.dirty.store(true, Ordering::Relaxed);
            });
            log(format!("{letter}: journal watch ended ({err}); rebuilding"));
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    fn run_walk(&self, slot: usize, letter: char) {
        loop {
            let t0 = Instant::now();
            let v = walk::build(&format!("{letter}:\\"));
            log(format!("{letter}: walked {} entries in {:?}", v.live, t0.elapsed()));
            self.replace(slot, v);
            background_mode();
            std::thread::sleep(RESCAN_EVERY);
        }
    }

    pub fn save_if_dirty(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let t0 = Instant::now();
        let mut g = self.vols.write().unwrap();
        match snapshot::save(&data_dir().join("index.bin"), &mut g) {
            Ok(()) => log(format!("snapshot saved in {:?}", t0.elapsed())),
            Err(e) => log(format!("snapshot save failed: {e}")),
        }
    }

    pub fn status(&self) -> heimdall_core::proto::Status {
        let g = self.vols.read().unwrap();
        heimdall_core::proto::Status {
            volumes: g.len() as u32,
            entries: g.iter().map(|v| v.live as u64).sum(),
            ntfs: g.iter().any(|v| v.source == Source::Ntfs),
        }
    }
}

/// Lower CPU and I/O priority for the long-lived watcher threads.
fn background_mode() {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
}
