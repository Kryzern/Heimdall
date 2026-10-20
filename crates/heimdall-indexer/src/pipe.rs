use crate::engine::Engine;
use crate::log;
use heimdall_core::proto::{self, OP_SEARCH, OP_STATUS};
use heimdall_core::Query;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::*;

/// SYSTEM and administrators: full control. Interactive/authenticated users:
/// read + write (send a query, read the answer). Remote clients are rejected.
const SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;AU)";

struct Pipe(HANDLE);
unsafe impl Send for Pipe {}

pub fn serve(engine: Arc<Engine>) {
    let sddl: Vec<u16> = SDDL.encode_utf16().chain(Some(0)).collect();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, null_mut()) };
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let name: Vec<u16> = proto::PIPE_NAME.encode_utf16().chain(Some(0)).collect();
    log(format!("listening on {}", proto::PIPE_NAME));
    loop {
        let h = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                1 << 20,
                64 << 10,
                0,
                if sd.is_null() { null() } else { &sa },
            )
        };
        if h == INVALID_HANDLE_VALUE {
            log(format!("CreateNamedPipe failed: {}", std::io::Error::last_os_error()));
            std::thread::sleep(std::time::Duration::from_secs(1));
            continue;
        }
        let ok = unsafe { ConnectNamedPipe(h, null_mut()) } != 0
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if !ok {
            unsafe { CloseHandle(h) };
            continue;
        }
        let e = engine.clone();
        let pipe = Pipe(h);
        std::thread::Builder::new().name("client".into()).spawn(move || client(e, pipe)).ok();
    }
}

fn client(engine: Arc<Engine>, pipe: Pipe) {
    let h = pipe.0;
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let mut got = 0u32;
        let ok = unsafe { ReadFile(h, buf.as_mut_ptr(), buf.len() as u32, &mut got, null_mut()) };
        if ok == 0 {
            break;
        }
        let resp = match proto::decode_request(&buf[..got as usize]) {
            Some((OP_SEARCH, limit, q)) => {
                let query = Query::parse(q);
                let vols = engine.vols.read().unwrap();
                let r = engine.searcher.search(&vols, &query, limit.clamp(1, 1000) as usize);
                proto::encode_results(&r)
            }
            Some((OP_STATUS, _, _)) => proto::encode_status(&engine.status()),
            _ => Vec::new(),
        };
        let mut wrote = 0u32;
        let ok = unsafe { WriteFile(h, resp.as_ptr(), resp.len() as u32, &mut wrote, null_mut()) };
        if ok == 0 {
            break;
        }
    }
    unsafe {
        DisconnectNamedPipe(h);
        CloseHandle(h);
    }
}
