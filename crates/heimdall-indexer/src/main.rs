//! heimdall-indexer: keeps an index of every file on every fixed drive and
//! answers searches over a named pipe.
//!
//!   heimdall-indexer install     copy to Program Files, register and start the service
//!   heimdall-indexer uninstall   stop and remove the service
//!   heimdall-indexer run         run in the foreground (needs an elevated console)
//!
//! Started without arguments it expects to be launched by the service manager.

mod engine;
mod pipe;
mod service;

use std::path::PathBuf;

pub const SERVICE_NAME: &str = "HeimdallIndexer";

pub fn data_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("Heimdall")
}

pub fn log(msg: impl AsRef<str>) {
    use std::io::Write;
    let line = format!("[{}] {}\n", heimdall_core::query::now_unix(), msg.as_ref());
    if service::is_console() {
        eprint!("{line}");
    }
    let dir = data_dir();
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("indexer.log");
    if std::fs::metadata(&path).map(|m| m.len() > 1 << 20).unwrap_or(false) {
        std::fs::rename(&path, path.with_extension("log.old")).ok();
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        f.write_all(line.as_bytes()).ok();
    }
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    let code = match arg.as_str() {
        "install" => report(service::install()),
        "uninstall" => report(service::uninstall()),
        "run" => {
            service::set_console();
            let engine = engine::Engine::start();
            pipe::serve(engine);
            0
        }
        "" => {
            if service::dispatch() {
                0
            } else {
                usage();
                1
            }
        }
        _ => {
            usage();
            2
        }
    };
    std::process::exit(code);
}

fn report(r: std::io::Result<String>) -> i32 {
    match r {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("heimdall-indexer: {e}");
            if e.raw_os_error() == Some(5) {
                eprintln!("This needs an elevated (administrator) prompt.");
            }
            1
        }
    }
}

fn usage() {
    eprintln!("usage: heimdall-indexer [install | uninstall | run]");
}
