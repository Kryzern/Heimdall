//! heimdall-bench: measures index build time, memory and query latency, and
//! compares round-trip latency against a running Everything instance.
//!
//!   heimdall-bench                  index all fixed drives, run the default query set
//!   heimdall-bench --drives C,D     only these drives
//!   heimdall-bench --walk           force the directory-walk indexer
//!   heimdall-bench --iters 50       iterations per query (default 30)
//!   heimdall-bench --snapshot P     load a saved index (e.g. %LOCALAPPDATA%\Heimdall\index.bin)
//!                                   instead of building one

mod everything;

use heimdall_core::index::Source;
use heimdall_core::{ntfs, snapshot, walk, Query, Searcher, Volume};
use std::time::{Duration, Instant};

const QUERIES: &[&str] = &[
    "a", "re", "report", "main.rs", "readme", "heimdall", "config json", "ext:pdf invoice", "node_modules",
    "zzqxv",
];
const TYPING: &str = "readme.md";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let opt = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let iters: usize = opt("--iters").and_then(|s| s.parse().ok()).unwrap_or(30);
    let drives: Vec<char> = match opt("--drives") {
        Some(s) => s.split(',').filter_map(|d| d.chars().next()).map(|c| c.to_ascii_uppercase()).collect(),
        None => ntfs::fixed_drives(),
    };

    println!("Heimdall benchmark  ({} logical CPUs)\n", std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));
    println!("== Index build ==");
    let mut vols = Vec::new();
    let total = Instant::now();
    if let Some(p) = opt("--snapshot") {
        vols = snapshot::load(std::path::Path::new(&p)).unwrap_or_else(|e| panic!("{p}: {e}"));
        println!("  loaded {} volumes from {p}", vols.len());
    }
    let drives = if vols.is_empty() { drives } else { Vec::new() };
    for d in &drives {
        let t0 = Instant::now();
        let root = format!("{d}:\\");
        let v = if flag("--walk") {
            walk::build(&root)
        } else {
            match ntfs::build(*d) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("  {d}: MFT unavailable ({e}); walking instead (run elevated for the NTFS path)");
                    walk::build(&root)
                }
            }
        };
        println!(
            "  {d}:  {:>9} entries  {:>8.1} ms  {:>6.1} MB  [{}]",
            v.live,
            t0.elapsed().as_secs_f64() * 1e3,
            v.memory_bytes() as f64 / 1048576.0,
            if v.source == Source::Ntfs { "MFT" } else { "walk" }
        );
        vols.push(v);
    }
    let entries: u64 = vols.iter().map(|v| v.live as u64).sum();
    let mem: usize = vols.iter().map(Volume::memory_bytes).sum();
    println!(
        "  all: {:>9} entries  {:>8.1} ms  {:>6.1} MB  ({:.1} bytes/entry)\n",
        entries,
        total.elapsed().as_secs_f64() * 1e3,
        mem as f64 / 1048576.0,
        mem as f64 / entries.max(1) as f64
    );

    println!("== Query latency, in-process, cold cache (median / p95 over {iters} runs) ==");
    for q in QUERIES {
        let query = Query::parse(q);
        let mut times = Vec::with_capacity(iters);
        let mut matches = 0;
        for _ in 0..iters {
            let s = Searcher::new();
            let t0 = Instant::now();
            let r = s.search(&vols, &query, 50);
            times.push(t0.elapsed());
            matches = r.total;
        }
        let (med, p95) = stats(&mut times);
        println!("  {:<18} {:>9} matches  {:>8.3} ms  {:>8.3} ms", format!("\"{q}\""), matches, ms(med), ms(p95));
    }

    println!("\n== Typing \"{TYPING}\" one key at a time (incremental narrowing) ==");
    let mut per_key = Vec::new();
    for _ in 0..iters {
        let s = Searcher::new();
        for n in 1..=TYPING.len() {
            let query = Query::parse(&TYPING[..n]);
            let t0 = Instant::now();
            s.search(&vols, &query, 50);
            per_key.push(t0.elapsed());
        }
    }
    let (med, p95) = stats(&mut per_key);
    println!("  per keystroke: median {:.3} ms, p95 {:.3} ms", ms(med), ms(p95));

    if let Ok(mut c) = heimdall_core::proto::Client::connect() {
        println!("\n== Heimdall indexer service, named-pipe round trip ==");
        if let Ok(st) = c.status() {
            println!("  service index: {} entries ({})", st.entries, if st.ntfs { "MFT" } else { "walk" });
        }
        for q in QUERIES {
            let mut times = Vec::with_capacity(iters);
            let mut matches = 0;
            for _ in 0..iters {
                let t0 = Instant::now();
                if let Ok(r) = c.search(q, 50) {
                    matches = r.total;
                }
                times.push(t0.elapsed());
            }
            let (med, p95) = stats(&mut times);
            println!("  {:<18} {:>9} matches  {:>8.3} ms  {:>8.3} ms", format!("\"{q}\""), matches, ms(med), ms(p95));
        }
    }

    match everything::Client::connect() {
        Some(mut ev) => {
            println!("\n== Everything ({}), WM_COPYDATA round trip ==", ev.class);
            for q in QUERIES {
                let eq = everything::translate(q);
                let mut times = Vec::with_capacity(iters);
                let mut matches = 0;
                for _ in 0..iters {
                    let t0 = Instant::now();
                    match ev.query(&eq, 50) {
                        Some(n) => matches = n,
                        None => break,
                    }
                    times.push(t0.elapsed());
                }
                if times.is_empty() {
                    println!("  {:<18} (no reply)", format!("\"{q}\""));
                    continue;
                }
                let (med, p95) = stats(&mut times);
                println!("  {:<18} {:>9} matches  {:>8.3} ms  {:>8.3} ms", format!("\"{q}\""), matches, ms(med), ms(p95));
            }
            if let Some((ws, private)) = everything::memory() {
                println!("  Everything.exe memory: {:.1} MB working set, {:.1} MB private", ws as f64 / 1048576.0, private as f64 / 1048576.0);
            }
        }
        None => println!("\n(Everything is not running; skipping the comparison.)"),
    }
}

fn stats(t: &mut [Duration]) -> (Duration, Duration) {
    t.sort_unstable();
    let p95 = ((t.len() as f64 * 0.95) as usize).min(t.len() - 1);
    (t[t.len() / 2], t[p95])
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}
