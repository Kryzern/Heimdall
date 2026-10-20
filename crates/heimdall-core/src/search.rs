//! Search: SIMD substring prefilter -> cheap name scoring -> path scoring on
//! the survivors only.
//!
//! 1. The longest query token is located with `memchr::memmem` over the
//!    folded name arena, split into chunks scanned on every core.
//! 2. Candidates get a name score (prefix, word boundary, whole-name match,
//!    length) plus the inline filters. No paths are built here.
//! 3. The best few hundred get their path built and scored (user folders up,
//!    caches and system folders down), then metadata filters if asked for.
//!
//! When the user keeps typing (`rep` -> `repo`), the previous candidate set
//! is filtered instead of rescanning the arena.

use crate::index::{Volume, DELETED, DIR, HIDDEN, SYSTEM};
use crate::query::{Kind, Query};
use memchr::memmem::{self, Finder};
use rayon::prelude::*;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: String,
    pub dir: bool,
    pub score: i32,
}

#[derive(Clone, Debug, Default)]
pub struct Results {
    pub hits: Vec<Hit>,
    /// Number of entries that matched before truncation.
    pub total: u64,
    pub micros: u32,
}

type Candidates = Arc<Vec<Vec<u32>>>;

struct Cache {
    primary: Vec<u8>,
    gens: Vec<u64>,
    cands: Candidates,
}

#[derive(Default)]
pub struct Searcher {
    cache: Mutex<Option<Cache>>,
}

#[derive(Clone, Copy)]
struct Scored {
    score: i32,
    vol: u16,
    pending: u8,
    ent: u32,
}

impl Searcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn search(&self, vols: &[Volume], q: &Query, limit: usize) -> Results {
        let t0 = Instant::now();
        if q.is_empty() || vols.is_empty() {
            return Results::default();
        }

        let cands = match q.tokens.len() {
            0 => Arc::new(vols.par_iter().map(all_live).collect()),
            1 => self.candidates(vols, &q.tokens[0]),
            // `heimdall report`: either word may live in the folder path rather
            // than the name, so take the union and let stage 2 check the rest.
            _ => {
                let finders: Vec<Finder<'_>> = q.tokens.iter().map(|t| Finder::new(t)).collect();
                Arc::new(
                    vols.par_iter()
                        .map(|v| {
                            let mut all: Vec<u32> = finders.iter().flat_map(|f| scan(v, f)).collect();
                            all.sort_unstable();
                            all.dedup();
                            all
                        })
                        .collect(),
                )
            }
        };

        let within: Vec<Vec<HashSet<u32>>> = if q.within.is_empty() {
            Vec::new()
        } else {
            vols.par_iter().map(|v| q.within.iter().map(|w| matching_dirs(v, w)).collect()).collect()
        };

        let finders: Vec<Finder<'_>> = q.tokens.iter().map(|t| Finder::new(t)).collect();
        // Results that still need a path check or metadata get a deeper pool.
        let keep = if q.needs_metadata() || q.tokens.len() > 1 { 4096 } else { 384 }.max(limit * 4);
        // Score in parallel chunks; each chunk keeps only its own top `keep`,
        // so a one-letter query never materialises millions of scored rows.
        let (mut total, mut scored) = cands
            .par_iter()
            .enumerate()
            .map(|(vi, list)| {
                let v = &vols[vi];
                let within = within.get(vi);
                list.par_chunks(16_384)
                    .map(|chunk| {
                        let mut out: Vec<Scored> = chunk
                            .iter()
                            .filter_map(|&i| {
                                stage1(v, i, q, &finders, within)
                                    .map(|(score, pending)| Scored { score, vol: vi as u16, pending, ent: i })
                            })
                            .collect();
                        let n = out.len() as u64;
                        top_k(&mut out, keep);
                        (n, out)
                    })
                    .reduce(|| (0, Vec::new()), merge)
            })
            .reduce(|| (0, Vec::new()), merge);

        if scored.is_empty() && q.tokens.len() == 1 && q.tokens[0].len() >= 3 && q.within.is_empty() {
            scored = fuzzy(vols, &q.tokens[0], q);
            total = scored.len() as u64;
        }
        top_k(&mut scored, keep);

        let mut hits: Vec<Hit> = scored
            .par_iter()
            .filter_map(|s| stage2(&vols[s.vol as usize], s, q))
            .collect();
        hits.sort_unstable_by(|a, b| b.score.cmp(&a.score).then(a.path.len().cmp(&b.path.len())));
        hits.truncate(limit);

        Results { hits, total, micros: t0.elapsed().as_micros().min(u32::MAX as u128) as u32 }
    }

    fn candidates(&self, vols: &[Volume], primary: &[u8]) -> Candidates {
        let gens: Vec<u64> = vols.iter().map(|v| v.gen ^ (v.entries.len() as u64) << 40).collect();
        let prev = {
            let guard = self.cache.lock().unwrap();
            guard.as_ref().and_then(|c| {
                (c.gens == gens && memmem::find(primary, &c.primary).is_some())
                    .then(|| (c.primary.clone(), c.cands.clone()))
            })
        };

        let cands: Candidates = match prev {
            Some((p, c)) if p == primary => return c,
            Some((_, c)) => {
                // Narrowing: every match of "repor" is a match of "repo".
                let f = Finder::new(primary);
                Arc::new(
                    c.par_iter()
                        .enumerate()
                        .map(|(vi, list)| {
                            let v = &vols[vi];
                            list.par_iter()
                                .copied()
                                .filter(|&i| f.find(v.folded_name(i)).is_some())
                                .collect()
                        })
                        .collect(),
                )
            }
            None => {
                let f = Finder::new(primary);
                Arc::new(vols.par_iter().map(|v| scan(v, &f)).collect())
            }
        };

        *self.cache.lock().unwrap() = Some(Cache { primary: primary.to_vec(), gens, cands: cands.clone() });
        cands
    }
}

/// Parallel SIMD scan of one volume's folded arena.
fn scan(v: &Volume, f: &Finder<'_>) -> Vec<u32> {
    let n = v.slot_off.len();
    if n == 0 {
        return Vec::new();
    }
    let chunks = (n / 32_768).clamp(1, 512);
    let parts: Vec<Vec<u32>> = (0..chunks)
        .into_par_iter()
        .map(|c| {
            let a = c * n / chunks;
            let b = (c + 1) * n / chunks;
            if a == b {
                return Vec::new();
            }
            let mut out = Vec::new();
            if f.needle().len() <= 2 {
                // Very short needles hit almost every name, often several times.
                // Walking name by name is cheaper than resolving each hit.
                for si in a..b {
                    let ei = v.slot_ent[si];
                    let e = &v.entries[ei as usize];
                    if e.name_off != v.slot_off[si] || e.flags & DELETED != 0 {
                        continue;
                    }
                    let name = &v.folded[e.name_off as usize..e.name_off as usize + e.name_len as usize];
                    if f.find(name).is_some() {
                        out.push(ei);
                    }
                }
                return out;
            }
            let start = v.slot_off[a] as usize;
            let end = if b < n { v.slot_off[b] as usize } else { v.folded.len() };
            let hay = &v.folded[start..end];
            let mut si = a;
            for m in f.find_iter(hay) {
                let pos = (start + m) as u32;
                while si + 1 < b && v.slot_off[si + 1] <= pos {
                    si += 1;
                }
                let ei = v.slot_ent[si];
                let e = &v.entries[ei as usize];
                // Stale slot (renamed entry) or deleted entry.
                if e.name_off != v.slot_off[si] || e.flags & DELETED != 0 {
                    continue;
                }
                if out.last() != Some(&ei) {
                    out.push(ei);
                }
            }
            out
        })
        .collect();
    parts.concat()
}

fn all_live(v: &Volume) -> Vec<u32> {
    (0..v.entries.len() as u32).filter(|&i| v.entries[i as usize].flags & DELETED == 0).collect()
}

fn matching_dirs(v: &Volume, term: &[u8]) -> HashSet<u32> {
    let f = Finder::new(term);
    scan(v, &f)
        .into_iter()
        .filter(|&i| v.entries[i as usize].flags & DIR != 0)
        .map(|i| v.entries[i as usize].rec)
        .collect()
}

#[inline]
fn is_boundary(b: u8) -> bool {
    matches!(b, b' ' | b'_' | b'-' | b'.' | b'(' | b'[' | b'+' | b',' | b'@' | b'#' | b'&' | b'\'')
}

#[inline]
fn token_score(orig: &[u8], name: &[u8], tok: &[u8], p: usize) -> i32 {
    let mut s = if p == 0 {
        50
    } else if is_boundary(name[p - 1]) || (orig[p].is_ascii_uppercase() && orig[p - 1].is_ascii_lowercase()) {
        30
    } else {
        6
    };
    let end = p + tok.len();
    if end == name.len() || is_boundary(name[end]) {
        s += 8;
    }
    s
}

fn has_ext(name: &[u8], ext: &[u8]) -> bool {
    name.len() > ext.len() && name.ends_with(ext) && name[name.len() - ext.len() - 1] == b'.'
}

fn stem(name: &[u8]) -> &[u8] {
    match memchr::memrchr(b'.', name) {
        Some(0) | None => name,
        Some(i) => &name[..i],
    }
}

fn stage1(
    v: &Volume,
    i: u32,
    q: &Query,
    finders: &[Finder<'_>],
    within: Option<&Vec<HashSet<u32>>>,
) -> Option<(i32, u8)> {
    let e = &v.entries[i as usize];
    if e.flags & DELETED != 0 {
        return None;
    }
    let dir = e.flags & DIR != 0;
    match q.kind {
        Kind::Files if dir => return None,
        Kind::Dirs if !dir => return None,
        _ => {}
    }
    let name = v.folded_name(i);
    if !q.exts.is_empty() && (dir || !q.exts.iter().any(|x| has_ext(name, x))) {
        return None;
    }
    let orig = &v.names[e.name_off as usize..e.name_off as usize + e.name_len as usize];

    let mut score = 0i32;
    let mut pending = 0u8;
    let mut matched = 0usize;
    for (k, (t, f)) in q.tokens.iter().zip(finders).enumerate() {
        match f.find(name) {
            Some(p) => {
                score += token_score(orig, name, t, p);
                matched += t.len();
            }
            None if k < 8 => pending |= 1 << k,
            None => return None,
        }
    }
    if q.tokens.len() == 1 {
        let t = q.tokens[0].as_slice();
        if name == t {
            score += 120;
        } else if stem(name) == t {
            score += 100;
        }
    }
    score -= (name.len().saturating_sub(matched) as i32).min(48) / 2;
    if dir {
        score += 4;
    }
    if e.flags & (HIDDEN | SYSTEM) != 0 {
        score -= 25;
    }
    if matches!(name.first(), Some(b'.' | b'~' | b'$')) {
        score -= 12;
    }

    if let Some(sets) = within {
        let anc: Vec<u32> = v.ancestors(i).map(|a| v.entries[a as usize].rec).collect();
        if !sets.iter().all(|set| anc.iter().any(|r| set.contains(r))) {
            return None;
        }
    }
    Some((score, pending))
}

const PATH_WEIGHTS: &[(&[u8], i32)] = &[
    (b"\\desktop\\", 22),
    (b"\\documents\\", 18),
    (b"\\downloads\\", 18),
    (b"\\pictures\\", 10),
    (b"\\videos\\", 8),
    (b"\\music\\", 6),
    (b"\\onedrive", 8),
    (b"\\source\\", 10),
    (b"\\repos\\", 10),
    (b"\\projects\\", 10),
    (b"\\appdata\\", -40),
    (b":\\windows\\", -45),
    (b"\\winsxs\\", -60),
    (b"\\program files", -10),
    (b"\\programdata\\", -30),
    (b"\\node_modules\\", -60),
    (b"\\.git\\", -60),
    (b"\\target\\", -25),
    (b"\\.cargo\\", -35),
    (b"\\.rustup\\", -40),
    (b"\\__pycache__\\", -50),
    (b"\\site-packages\\", -40),
    (b"\\$recycle.bin\\", -80),
    (b"\\cache", -25),
    (b"\\temp\\", -30),
    (b"\\.vs\\", -40),
    (b"\\obj\\", -20),
    (b"\\system volume information\\", -80),
];

fn path_score(fp: &[u8]) -> i32 {
    let mut s: i32 = PATH_WEIGHTS
        .iter()
        .filter(|(pat, _)| memmem::find(fp, pat).is_some())
        .map(|(_, w)| *w)
        .sum();
    s = s.max(-110);
    let depth = memchr::memchr_iter(b'\\', fp).count() as i32;
    s - depth * 3
}

fn stage2(v: &Volume, s: &Scored, q: &Query) -> Option<Hit> {
    let path = v.path(s.ent);
    let fp = crate::index::fold(&path);
    if s.pending != 0 {
        for (k, t) in q.tokens.iter().enumerate().take(8) {
            if s.pending & (1 << k) != 0 && memmem::find(&fp, t).is_none() {
                return None;
            }
        }
    }
    let dir = v.entries[s.ent as usize].flags & DIR != 0;
    if q.needs_metadata() {
        let md = std::fs::symlink_metadata(&path).ok()?;
        if !dir {
            if q.min_size.is_some_and(|m| md.len() < m) || q.max_size.is_some_and(|m| md.len() > m) {
                return None;
            }
        }
        if let Some(after) = q.modified_after {
            let mt = md
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            if mt < after {
                return None;
            }
        }
    }
    Some(Hit { score: s.score + path_score(&fp), path, dir })
}

fn merge(a: (u64, Vec<Scored>), b: (u64, Vec<Scored>)) -> (u64, Vec<Scored>) {
    let (mut va, vb) = (a.1, b.1);
    va.extend(vb);
    (a.0 + b.0, va)
}

fn top_k(v: &mut Vec<Scored>, k: usize) {
    if v.len() > k {
        v.select_nth_unstable_by(k, |a, b| b.score.cmp(&a.score));
        v.truncate(k);
    }
}

/// Subsequence fallback for typos like `hmdl` -> `heimdall`.
fn fuzzy(vols: &[Volume], tok: &[u8], q: &Query) -> Vec<Scored> {
    vols.par_iter()
        .enumerate()
        .flat_map_iter(|(vi, v)| {
            let n = v.entries.len() as u32;
            (0..n)
                .into_par_iter()
                .filter_map(|i| {
                    let e = &v.entries[i as usize];
                    if e.flags & DELETED != 0 {
                        return None;
                    }
                    let name = v.folded_name(i);
                    let (start, span) = subsequence(name, tok)?;
                    let mut sc = -10 - (span as i32 - tok.len() as i32) * 3 - start as i32;
                    if start == 0 {
                        sc += 20;
                    }
                    sc -= (name.len() as i32 - span as i32).min(30) / 3;
                    let dir = e.flags & DIR != 0;
                    match q.kind {
                        Kind::Files if dir => return None,
                        Kind::Dirs if !dir => return None,
                        _ => {}
                    }
                    Some(Scored { score: sc, vol: vi as u16, pending: 0, ent: i })
                })
                .collect::<Vec<_>>()
        })
        .filter(|s| s.score > -40)
        .collect()
}

/// Returns (start, span) of the tightest-from-first-char subsequence match.
fn subsequence(hay: &[u8], needle: &[u8]) -> Option<(usize, usize)> {
    let start = memchr::memchr(needle[0], hay)?;
    let mut j = 1;
    let mut i = start + 1;
    while j < needle.len() {
        let off = memchr::memchr(needle[j], &hay[i..])?;
        i += off + 1;
        j += 1;
    }
    Some((start, i - start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Source, DIR};

    fn vol() -> Volume {
        let mut v = Volume::new("C:\\", 5, Source::Walk);
        v.push(10, 5, "Users", DIR);
        v.push(11, 10, "ethan", DIR);
        v.push(12, 11, "Downloads", DIR);
        v.push(13, 12, "Quarterly Report.pdf", 0);
        v.push(14, 11, "AppData", DIR);
        v.push(15, 14, "report.pdf", 0);
        v.push(16, 11, "Desktop", DIR);
        v.push(17, 16, "heimdall", DIR);
        v.push(18, 17, "report.txt", 0);
        v
    }

    #[test]
    fn ranks_user_folders_over_appdata() {
        let s = Searcher::new();
        let r = s.search(&[vol()], &Query::parse("report"), 10);
        assert_eq!(r.total, 3);
        assert_ne!(r.hits[0].path, "C:\\Users\\ethan\\AppData\\report.pdf");
    }

    #[test]
    fn filters_and_narrowing() {
        let s = Searcher::new();
        let vols = [vol()];
        assert_eq!(s.search(&vols, &Query::parse("rep ext:pdf"), 10).total, 2);
        assert_eq!(s.search(&vols, &Query::parse("repo in:downloads"), 10).total, 1);
        assert_eq!(s.search(&vols, &Query::parse("report.t"), 10).total, 1);
        assert_eq!(s.search(&vols, &Query::parse("heimdall report"), 10).hits[0].path, "C:\\Users\\ethan\\Desktop\\heimdall\\report.txt");
    }

    #[test]
    fn fuzzy_fallback() {
        let s = Searcher::new();
        let r = s.search(&[vol()], &Query::parse("hmdl"), 10);
        assert_eq!(r.hits[0].path, "C:\\Users\\ethan\\Desktop\\heimdall");
    }
}
