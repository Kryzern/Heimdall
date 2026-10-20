//! Fallback indexer: a parallel directory walk. Used for non-NTFS drives and
//! when the indexer service is not installed (no admin rights for the MFT).

use crate::index::{Source, Volume, DIR, HIDDEN, SYSTEM};
use std::os::windows::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

const ROOT_REC: u32 = 0;

struct Found {
    rec: u32,
    parent: u32,
    flags: u16,
    name: String,
}

pub fn build(root: &str) -> Volume {
    let next = AtomicU32::new(ROOT_REC + 1);
    let batches: Mutex<Vec<Vec<Found>>> = Mutex::new(Vec::new());

    fn visit<'s>(
        s: &rayon::Scope<'s>,
        dir: PathBuf,
        rec: u32,
        next: &'s AtomicU32,
        batches: &'s Mutex<Vec<Vec<Found>>>,
    ) {
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        let mut batch = Vec::new();
        for ent in rd.flatten() {
            let Ok(ft) = ent.file_type() else { continue };
            let attrs = ent.metadata().map(|m| m.file_attributes()).unwrap_or(0);
            let id = next.fetch_add(1, Ordering::Relaxed);
            let mut flags = 0;
            if ft.is_dir() {
                flags |= DIR;
            }
            if attrs & 0x2 != 0 {
                flags |= HIDDEN;
            }
            if attrs & 0x4 != 0 {
                flags |= SYSTEM;
            }
            let name = ent.file_name().to_string_lossy().into_owned();
            // Junctions and symlinks are listed but not followed (no cycles).
            if ft.is_dir() && !ft.is_symlink() {
                let path = ent.path();
                s.spawn(move |s| visit(s, path, id, next, batches));
            }
            batch.push(Found { rec: id, parent: rec, flags, name });
        }
        if !batch.is_empty() {
            batches.lock().unwrap().push(batch);
        }
    }

    // A private pool: directory listing blocks on I/O, and must never starve
    // searches running on the global pool.
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 16);
    match rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|i| format!("walk-{i}")).build() {
        Ok(pool) => pool.scope(|s| visit(s, PathBuf::from(root), ROOT_REC, &next, &batches)),
        Err(_) => rayon::scope(|s| visit(s, PathBuf::from(root), ROOT_REC, &next, &batches)),
    }

    let batches = batches.into_inner().unwrap();
    let n: usize = batches.iter().map(|b| b.len()).sum();
    let bytes: usize = batches.iter().flatten().map(|f| f.name.len() + 1).sum();
    let mut v = Volume::new(root, ROOT_REC, Source::Walk).with_capacity(n, bytes + 1);
    v.rec_map = vec![crate::index::NONE; next.load(Ordering::Relaxed) as usize];
    for f in batches.into_iter().flatten() {
        v.push(f.rec, f.parent, &f.name, f.flags);
    }
    v
}
