//! Index snapshots: the arrays are written as raw little-endian blocks so a
//! restart is "read a few files' worth of bytes and catch up on the journal".

use crate::index::{Entry, Source, Volume};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

const MAGIC: &[u8; 4] = b"HMDL";
const VERSION: u32 = 1;

fn as_bytes<T: Copy>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn put<W: Write, T: Copy>(w: &mut W, v: &[T]) -> io::Result<()> {
    w.write_all(&(v.len() as u64).to_le_bytes())?;
    w.write_all(as_bytes(v))
}

fn get<R: Read, T: Copy + Default>(r: &mut R) -> io::Result<Vec<T>> {
    let n = u64::from_le_bytes(take(r)?) as usize;
    if n > (1 << 34) / std::mem::size_of::<T>().max(1) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "snapshot array too large"));
    }
    let mut v = vec![T::default(); n];
    let bytes = unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, std::mem::size_of_val(&v[..])) };
    r.read_exact(bytes)?;
    Ok(v)
}

fn take<R: Read, const N: usize>(r: &mut R) -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}

pub fn save(path: &Path, vols: &mut [Volume]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
        w.write_all(MAGIC)?;
        w.write_all(&VERSION.to_le_bytes())?;
        w.write_all(&(vols.len() as u32).to_le_bytes())?;
        for v in vols.iter_mut() {
            if v.wasted_ratio() > 0.05 {
                v.compact();
            }
            put(&mut w, v.root.as_bytes())?;
            w.write_all(&v.root_rec.to_le_bytes())?;
            w.write_all(&[v.source as u8])?;
            w.write_all(&v.serial.to_le_bytes())?;
            w.write_all(&v.journal_id.to_le_bytes())?;
            w.write_all(&v.next_usn.to_le_bytes())?;
            w.write_all(&v.live.to_le_bytes())?;
            put(&mut w, &v.entries)?;
            put(&mut w, &v.names)?;
            put(&mut w, &v.folded)?;
            put(&mut w, &v.slot_off)?;
            put(&mut w, &v.slot_ent)?;
            put(&mut w, &v.rec_map)?;
        }
        w.flush()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn load(path: &Path) -> io::Result<Vec<Volume>> {
    let mut r = BufReader::with_capacity(1 << 20, File::open(path)?);
    if &take::<_, 4>(&mut r)? != MAGIC || u32::from_le_bytes(take(&mut r)?) != VERSION {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a heimdall snapshot"));
    }
    let n = u32::from_le_bytes(take(&mut r)?);
    let mut vols = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let root = String::from_utf8(get::<_, u8>(&mut r)?)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad root"))?;
        let root_rec = u32::from_le_bytes(take(&mut r)?);
        let source = if take::<_, 1>(&mut r)?[0] == 0 { Source::Ntfs } else { Source::Walk };
        let mut v = Volume::new(root, root_rec, source);
        v.serial = u32::from_le_bytes(take(&mut r)?);
        v.journal_id = u64::from_le_bytes(take(&mut r)?);
        v.next_usn = i64::from_le_bytes(take(&mut r)?);
        v.live = u32::from_le_bytes(take(&mut r)?);
        v.entries = get::<_, Entry>(&mut r)?;
        v.names = get(&mut r)?;
        v.folded = get(&mut r)?;
        v.slot_off = get(&mut r)?;
        v.slot_ent = get(&mut r)?;
        v.rec_map = get(&mut r)?;
        let n = v.names.len();
        let sane = n == v.folded.len()
            && v.slot_off.len() == v.slot_ent.len()
            && std::str::from_utf8(&v.names).is_ok()
            && v.entries.iter().all(|e| e.name_off as usize + e.name_len as usize <= n)
            && v.slot_off.iter().all(|&o| (o as usize) < n)
            && v.slot_ent.iter().all(|&i| (i as usize) < v.entries.len())
            && v.rec_map.iter().all(|&i| i == crate::index::NONE || (i as usize) < v.entries.len());
        if !sane {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "corrupt snapshot"));
        }
        vols.push(v);
    }
    Ok(vols)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::DIR;

    #[test]
    fn roundtrip() {
        let mut v = Volume::new("C:\\", 5, Source::Ntfs);
        v.push(10, 5, "Users", DIR);
        v.push(11, 10, "a.txt", 0);
        v.journal_id = 42;
        let p = std::env::temp_dir().join(format!("heimdall-test-{}.bin", std::process::id()));
        save(&p, &mut [v]).unwrap();
        let back = load(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(back[0].path(1), "C:\\Users\\a.txt");
        assert_eq!(back[0].journal_id, 42);
    }
}
