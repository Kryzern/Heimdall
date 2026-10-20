//! Wire protocol between `heimdall.exe` and `heimdall-indexer` over a
//! message-mode named pipe. One request message, one response message.
//!
//! Request:  [op: u8][limit: u32][query: utf8...]
//! Search:   [n: u32][total: u64][micros: u32] then n x [flags: u8][score: i32][len: u16][path: utf8]
//! Status:   [volumes: u32][entries: u64][ntfs: u8]

use crate::search::{Hit, Results};
use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Pipes::*;

pub const PIPE_NAME: &str = r"\\.\pipe\heimdall-index-v1";

pub const OP_SEARCH: u8 = 1;
pub const OP_STATUS: u8 = 2;

pub fn encode_request(op: u8, limit: u32, query: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(5 + query.len());
    b.push(op);
    b.extend_from_slice(&limit.to_le_bytes());
    b.extend_from_slice(query.as_bytes());
    b
}

pub fn decode_request(b: &[u8]) -> Option<(u8, u32, &str)> {
    if b.len() < 5 {
        return None;
    }
    let limit = u32::from_le_bytes(b[1..5].try_into().ok()?);
    Some((b[0], limit, std::str::from_utf8(&b[5..]).ok()?))
}

pub fn encode_results(r: &Results) -> Vec<u8> {
    let mut b = Vec::with_capacity(16 + r.hits.iter().map(|h| h.path.len() + 7).sum::<usize>());
    b.extend_from_slice(&(r.hits.len() as u32).to_le_bytes());
    b.extend_from_slice(&r.total.to_le_bytes());
    b.extend_from_slice(&r.micros.to_le_bytes());
    for h in &r.hits {
        b.push(h.dir as u8);
        b.extend_from_slice(&h.score.to_le_bytes());
        let p = &h.path.as_bytes()[..h.path.len().min(u16::MAX as usize)];
        b.extend_from_slice(&(p.len() as u16).to_le_bytes());
        b.extend_from_slice(p);
    }
    b
}

pub fn decode_results(b: &[u8]) -> Option<Results> {
    let mut at = 0usize;
    let mut next = |n: usize| -> Option<&[u8]> {
        let s = b.get(at..at + n)?;
        at += n;
        Some(s)
    };
    let n = u32::from_le_bytes(next(4)?.try_into().ok()?);
    let total = u64::from_le_bytes(next(8)?.try_into().ok()?);
    let micros = u32::from_le_bytes(next(4)?.try_into().ok()?);
    let mut hits = Vec::with_capacity(n.min(4096) as usize);
    for _ in 0..n {
        let dir = next(1)?[0] != 0;
        let score = i32::from_le_bytes(next(4)?.try_into().ok()?);
        let len = u16::from_le_bytes(next(2)?.try_into().ok()?) as usize;
        let path = String::from_utf8(next(len)?.to_vec()).ok()?;
        hits.push(Hit { path, dir, score });
    }
    Some(Results { hits, total, micros })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Status {
    pub volumes: u32,
    pub entries: u64,
    pub ntfs: bool,
}

pub fn encode_status(s: &Status) -> Vec<u8> {
    let mut b = Vec::with_capacity(13);
    b.extend_from_slice(&s.volumes.to_le_bytes());
    b.extend_from_slice(&s.entries.to_le_bytes());
    b.push(s.ntfs as u8);
    b
}

pub fn decode_status(b: &[u8]) -> Option<Status> {
    Some(Status {
        volumes: u32::from_le_bytes(b.get(0..4)?.try_into().ok()?),
        entries: u64::from_le_bytes(b.get(4..12)?.try_into().ok()?),
        ntfs: *b.get(12)? != 0,
    })
}

/// A persistent connection to the indexer service.
pub struct Client {
    h: HANDLE,
    buf: Vec<u8>,
}

unsafe impl Send for Client {}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.h) };
    }
}

impl Client {
    pub fn connect() -> io::Result<Client> {
        let name = crate::ntfs::wide(PIPE_NAME);
        let h = unsafe {
            CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null(), OPEN_EXISTING, 0, null_mut())
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mode = PIPE_READMODE_MESSAGE;
        if unsafe { SetNamedPipeHandleState(h, &mode, null(), null()) } == 0 {
            let e = io::Error::last_os_error();
            unsafe { CloseHandle(h) };
            return Err(e);
        }
        Ok(Client { h, buf: vec![0u8; 256 << 10] })
    }

    pub fn call(&mut self, req: &[u8]) -> io::Result<Vec<u8>> {
        let mut got = 0u32;
        let ok = unsafe {
            TransactNamedPipe(
                self.h,
                req.as_ptr() as *const c_void,
                req.len() as u32,
                self.buf.as_mut_ptr() as *mut c_void,
                self.buf.len() as u32,
                &mut got,
                null_mut(),
            )
        };
        let mut out = self.buf[..got as usize].to_vec();
        if ok == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                return Err(err);
            }
            loop {
                let mut n = 0u32;
                let ok = unsafe {
                    ReadFile(self.h, self.buf.as_mut_ptr(), self.buf.len() as u32, &mut n, null_mut())
                };
                out.extend_from_slice(&self.buf[..n as usize]);
                if ok != 0 {
                    break;
                }
                let e = io::Error::last_os_error();
                if e.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                    return Err(e);
                }
            }
        }
        Ok(out)
    }

    pub fn search(&mut self, query: &str, limit: u32) -> io::Result<Results> {
        let resp = self.call(&encode_request(OP_SEARCH, limit, query))?;
        decode_results(&resp).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad response"))
    }

    pub fn status(&mut self) -> io::Result<Status> {
        let resp = self.call(&encode_request(OP_STATUS, 0, ""))?;
        decode_status(&resp).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad response"))
    }
}
