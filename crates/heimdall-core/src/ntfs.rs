//! NTFS fast path: enumerate the master file table through the USN interface
//! and keep the index live by tailing the USN change journal.
//!
//! Reading a volume handle requires administrator rights, which is why this
//! runs inside `heimdall-indexer` (a SYSTEM service) in normal use.

use crate::index::{Source, Volume, DIR, HIDDEN, SYSTEM};
use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Ioctl::*;
use windows_sys::Win32::System::IO::DeviceIoControl;

const ROOT_REC: u32 = 5;
const FIRST_USER_REC: u64 = 16;
const BUF: usize = 1 << 20;

const REASON_CREATE: u32 = 0x0000_0100;
const REASON_DELETE: u32 = 0x0000_0200;
const REASON_RENAME_NEW: u32 = 0x0000_2000;
const REASON_BASIC_INFO: u32 = 0x0000_8000;

pub struct VolHandle(HANDLE);
unsafe impl Send for VolHandle {}
unsafe impl Sync for VolHandle {}

impl Drop for VolHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn open(letter: char) -> io::Result<VolHandle> {
    let path = wide(&format!("\\\\.\\{letter}:"));
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(VolHandle(h))
}

pub fn filesystem(root: &str) -> Option<(String, u32)> {
    let mut fs = [0u16; 32];
    let mut serial = 0u32;
    let ok = unsafe {
        GetVolumeInformationW(
            wide(root).as_ptr(),
            null_mut(),
            0,
            &mut serial,
            null_mut(),
            null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let n = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
    Some((String::from_utf16_lossy(&fs[..n]), serial))
}

/// Fixed (non-removable, non-network) drive letters.
pub fn fixed_drives() -> Vec<char> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| (b'A' + i) as char)
        .filter(|c| unsafe { GetDriveTypeW(wide(&format!("{c}:\\")).as_ptr()) } == 3 /* DRIVE_FIXED */)
        .collect()
}

fn ioctl<I>(h: &VolHandle, code: u32, input: Option<&I>, out: &mut [u8]) -> io::Result<usize> {
    let mut got = 0u32;
    let (ip, il) = match input {
        Some(i) => (i as *const I as *const c_void, size_of::<I>() as u32),
        None => (null(), 0),
    };
    let ok = unsafe {
        DeviceIoControl(h.0, code, ip, il, out.as_mut_ptr() as *mut c_void, out.len() as u32, &mut got, null_mut())
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(got as usize)
}

#[derive(Clone, Copy, Debug)]
pub struct Journal {
    pub id: u64,
    pub next_usn: i64,
    pub lowest_valid: i64,
}

pub fn journal(h: &VolHandle) -> io::Result<Journal> {
    let mut out = [0u8; size_of::<USN_JOURNAL_DATA_V0>()];
    let r = ioctl::<()>(h, FSCTL_QUERY_USN_JOURNAL, None, &mut out);
    if let Err(e) = &r {
        if e.raw_os_error() == Some(ERROR_JOURNAL_NOT_ACTIVE as i32) {
            let create = CREATE_USN_JOURNAL_DATA { MaximumSize: 32 << 20, AllocationDelta: 4 << 20 };
            ioctl(h, FSCTL_CREATE_USN_JOURNAL, Some(&create), &mut [])?;
            ioctl::<()>(h, FSCTL_QUERY_USN_JOURNAL, None, &mut out)?;
        } else {
            r?;
        }
    }
    let d: USN_JOURNAL_DATA_V0 = unsafe { std::ptr::read_unaligned(out.as_ptr() as *const _) };
    Ok(Journal { id: d.UsnJournalID, next_usn: d.NextUsn, lowest_valid: d.LowestValidUsn })
}

fn mft_records(h: &VolHandle) -> io::Result<u64> {
    let mut out = [0u8; 256];
    ioctl::<()>(h, FSCTL_GET_NTFS_VOLUME_DATA, None, &mut out)?;
    let d: NTFS_VOLUME_DATA_BUFFER = unsafe { std::ptr::read_unaligned(out.as_ptr() as *const _) };
    Ok(d.MftValidDataLength as u64 / d.BytesPerFileRecordSegment.max(1) as u64)
}

/// One parsed USN_RECORD_V2.
pub struct Record<'a> {
    pub rec: u32,
    pub parent: u32,
    pub reason: u32,
    pub attrs: u32,
    pub name: &'a [u16],
}

/// Iterate USN_RECORD_V2 structures in an ioctl output buffer.
fn records(buf: &[u8]) -> impl Iterator<Item = Record<'_>> {
    let mut off = 8usize;
    std::iter::from_fn(move || loop {
        if off + 60 > buf.len() {
            return None;
        }
        let rd = |o: usize, n: usize| &buf[off + o..off + o + n];
        let len = u32::from_le_bytes(rd(0, 4).try_into().unwrap()) as usize;
        if len == 0 || off + len > buf.len() {
            return None;
        }
        let major = u16::from_le_bytes(rd(4, 2).try_into().unwrap());
        let here = off;
        off += len;
        if major != 2 {
            continue;
        }
        let b = &buf[here..here + len];
        let u64at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let u32at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let u16at = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap()) as usize;
        let frn = u64at(8) & 0xFFFF_FFFF_FFFF;
        let parent = u64at(16) & 0xFFFF_FFFF_FFFF;
        let (nlen, noff) = (u16at(56), u16at(58));
        if noff + nlen > len {
            continue;
        }
        let name: &[u16] =
            unsafe { std::slice::from_raw_parts(b[noff..].as_ptr() as *const u16, nlen / 2) };
        return Some(Record {
            rec: frn as u32,
            parent: parent as u32,
            reason: u32at(40),
            attrs: u32at(52),
            name,
        });
    })
}

fn flags_of(attrs: u32) -> u16 {
    let mut f = 0;
    if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
        f |= DIR;
    }
    if attrs & FILE_ATTRIBUTE_HIDDEN != 0 {
        f |= HIDDEN;
    }
    if attrs & FILE_ATTRIBUTE_SYSTEM != 0 {
        f |= SYSTEM;
    }
    f
}

pub(crate) fn utf16_into(name: &[u16], out: &mut String) {
    out.clear();
    out.extend(char::decode_utf16(name.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')));
}

struct Part {
    recs: Vec<(u32, u32, u16, u32, u16)>, // rec, parent, flags, name offset, name len
    names: String,
}

/// Enumerate one slice of the MFT [lo, hi) on its own volume handle.
fn enumerate_range(letter: char, lo: u64, hi: u64, high_usn: i64) -> io::Result<Part> {
    let h = open(letter)?;
    let mut part = Part { recs: Vec::with_capacity(((hi - lo) as usize).min(1 << 22)), names: String::new() };
    let mut buf = vec![0u8; BUF];
    let mut med = MFT_ENUM_DATA_V0 { StartFileReferenceNumber: lo, LowUsn: 0, HighUsn: high_usn };
    let mut name = String::new();
    'outer: loop {
        let got = match ioctl(&h, FSCTL_ENUM_USN_DATA, Some(&med), &mut buf) {
            Ok(n) => n,
            Err(e) if e.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) => break,
            Err(e) => return Err(e),
        };
        if got <= 8 {
            break;
        }
        for r in records(&buf[..got]) {
            if r.rec as u64 >= hi {
                break 'outer;
            }
            if (r.rec as u64) < FIRST_USER_REC {
                continue;
            }
            utf16_into(r.name, &mut name);
            let off = part.names.len() as u32;
            part.names.push_str(&name);
            part.recs.push((r.rec, r.parent, flags_of(r.attrs), off, name.len() as u16));
        }
        med.StartFileReferenceNumber = u64::from_le_bytes(buf[..8].try_into().unwrap());
        if med.StartFileReferenceNumber >= hi {
            break;
        }
    }
    Ok(part)
}

/// Build a complete index of an NTFS volume, splitting the MFT across threads.
pub fn build(letter: char) -> io::Result<Volume> {
    let root = format!("{letter}:\\");
    let (_, serial) = filesystem(&root).unwrap_or_default();
    let h = open(letter)?;
    let j = journal(&h)?;
    let total = mft_records(&h)?.max(FIRST_USER_REC + 1);
    drop(h);

    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 16) as u64;
    let step = total.div_ceil(threads);
    let parts: Vec<io::Result<Part>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|t| {
                let lo = t * step;
                let hi = if t + 1 == threads { u64::MAX } else { (t + 1) * step };
                s.spawn(move || enumerate_range(letter, lo, hi, j.next_usn))
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut parts: Vec<Part> = parts.into_iter().collect::<io::Result<_>>()?;
    let n: usize = parts.iter().map(|p| p.recs.len()).sum();
    let bytes: usize = parts.iter().map(|p| p.names.len() + p.recs.len()).sum();
    let mut v = Volume::new(root, ROOT_REC, Source::Ntfs).with_capacity(n, bytes + 1);
    v.serial = serial;
    v.journal_id = j.id;
    v.next_usn = j.next_usn;
    v.rec_map = vec![crate::index::NONE; total as usize + 1024];
    for p in parts.iter_mut() {
        for &(rec, parent, flags, off, len) in &p.recs {
            v.push(rec, parent, &p.names[off as usize..off as usize + len as usize], flags);
        }
        p.recs = Vec::new();
    }
    Ok(v)
}

pub enum Change {
    Upsert { rec: u32, parent: u32, name: String, flags: u16 },
    Remove { rec: u32 },
}

/// Block on the change journal and hand batches of changes to `apply`.
/// Returns only on error (e.g. the journal was reset and a rebuild is needed).
pub fn watch(letter: char, journal_id: u64, mut next: i64, mut apply: impl FnMut(Vec<Change>, i64)) -> io::Error {
    let h = match open(letter) {
        Ok(h) => h,
        Err(e) => return e,
    };
    let mut buf = vec![0u8; 256 << 10];
    let mut name = String::new();
    loop {
        let rd = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: next,
            ReasonMask: REASON_CREATE | REASON_DELETE | REASON_RENAME_NEW | REASON_BASIC_INFO,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 1,
            UsnJournalID: journal_id,
        };
        let got = match ioctl(&h, FSCTL_READ_USN_JOURNAL, Some(&rd), &mut buf) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if got < 8 {
            continue;
        }
        let new_next = i64::from_le_bytes(buf[..8].try_into().unwrap());
        let mut changes = Vec::new();
        for r in records(&buf[..got]) {
            if (r.rec as u64) < FIRST_USER_REC {
                continue;
            }
            if r.reason & REASON_DELETE != 0 {
                changes.push(Change::Remove { rec: r.rec });
            } else {
                utf16_into(r.name, &mut name);
                changes.push(Change::Upsert { rec: r.rec, parent: r.parent, name: name.clone(), flags: flags_of(r.attrs) });
            }
        }
        next = new_next;
        apply(changes, next);
    }
}

pub fn apply(v: &mut Volume, changes: Vec<Change>, next_usn: i64) {
    for c in changes {
        match c {
            Change::Upsert { rec, parent, name, flags } => v.upsert(rec, parent, &name, flags),
            Change::Remove { rec } => v.remove(rec),
        }
    }
    v.next_usn = next_usn;
}

/// Can a saved snapshot be caught up from the journal, or must it be rebuilt?
pub fn resumable(v: &Volume) -> bool {
    let letter = match v.root.chars().next() {
        Some(c) => c,
        None => return false,
    };
    let Ok(h) = open(letter) else { return false };
    let Ok(j) = journal(&h) else { return false };
    let serial_ok = filesystem(&v.root).map(|(_, s)| s == v.serial).unwrap_or(false);
    serial_ok && j.id == v.journal_id && v.next_usn >= j.lowest_valid && v.next_usn <= j.next_usn
}
