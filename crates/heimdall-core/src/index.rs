//! The in-memory file index.
//!
//! Every filename on a volume lives in one contiguous byte arena, with a
//! case-folded twin at identical offsets. Each name is wrapped in NUL bytes
//! so a substring scan can never match across two names, and "is this a
//! prefix / a whole-name match" is a single byte comparison.
//!
//! Entries are 16 bytes. Paths are never stored; they are rebuilt from the
//! parent chain only for the handful of rows that actually reach the screen.

pub const DIR: u16 = 1;
pub const HIDDEN: u16 = 2;
pub const SYSTEM: u16 = 4;
pub const DELETED: u16 = 0x8000;

pub const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct Entry {
    /// Record number of this entry (MFT record on NTFS, synthetic otherwise).
    pub rec: u32,
    /// Record number of the parent directory.
    pub parent: u32,
    pub name_off: u32,
    pub name_len: u16,
    pub flags: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Source {
    /// Built from the NTFS master file table, kept live by the USN journal.
    Ntfs = 0,
    /// Built by walking directories (non-NTFS drives, or no admin rights).
    Walk = 1,
}

pub struct Volume {
    /// Display root, e.g. `C:\`.
    pub root: String,
    pub root_rec: u32,
    pub source: Source,
    pub serial: u32,
    pub journal_id: u64,
    pub next_usn: i64,

    pub entries: Vec<Entry>,
    /// Original names (UTF-8), NUL separated.
    pub names: Vec<u8>,
    /// Case-folded names at the same offsets as `names`.
    pub folded: Vec<u8>,
    /// Arena offset of every name ever appended, ascending.
    pub slot_off: Vec<u32>,
    /// Entry index that owned the name at `slot_off[i]` when it was appended.
    pub slot_ent: Vec<u32>,
    /// Record number -> entry index.
    pub rec_map: Vec<u32>,

    pub live: u32,
    /// Bumped on every mutation; invalidates incremental search caches.
    pub gen: u64,
    dead_bytes: usize,
}

impl Volume {
    pub fn new(root: impl Into<String>, root_rec: u32, source: Source) -> Self {
        Volume {
            root: root.into(),
            root_rec,
            source,
            serial: 0,
            journal_id: 0,
            next_usn: 0,
            entries: Vec::new(),
            names: vec![0],
            folded: vec![0],
            slot_off: Vec::new(),
            slot_ent: Vec::new(),
            rec_map: Vec::new(),
            live: 0,
            gen: 0,
            dead_bytes: 0,
        }
    }

    pub fn with_capacity(mut self, entries: usize, name_bytes: usize) -> Self {
        self.entries.reserve(entries);
        self.slot_off.reserve(entries);
        self.slot_ent.reserve(entries);
        self.names.reserve(name_bytes);
        self.folded.reserve(name_bytes);
        self
    }

    fn append_name(&mut self, name: &str, ent: u32) -> (u32, u16) {
        let name = truncate_utf8(name, u16::MAX as usize);
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name.as_bytes());
        self.names.push(0);
        fold_into(name, &mut self.folded);
        self.folded.push(0);
        debug_assert_eq!(self.names.len(), self.folded.len());
        self.slot_off.push(off);
        self.slot_ent.push(ent);
        (off, name.len() as u16)
    }

    fn map_rec(&mut self, rec: u32, idx: u32) {
        let r = rec as usize;
        if r >= self.rec_map.len() {
            self.rec_map.resize((r + 1).max(self.rec_map.len() * 5 / 4), NONE);
        }
        self.rec_map[r] = idx;
    }

    /// Append a brand-new record. Used by bulk builders; no duplicate check.
    pub fn push(&mut self, rec: u32, parent: u32, name: &str, flags: u16) {
        let idx = self.entries.len() as u32;
        let (name_off, name_len) = self.append_name(name, idx);
        self.entries.push(Entry { rec, parent, name_off, name_len, flags });
        self.map_rec(rec, idx);
        self.live += 1;
    }

    pub fn entry_of(&self, rec: u32) -> Option<u32> {
        match self.rec_map.get(rec as usize) {
            Some(&i) if i != NONE => Some(i),
            _ => None,
        }
    }

    /// Insert or update a record (create / rename / move).
    pub fn upsert(&mut self, rec: u32, parent: u32, name: &str, flags: u16) {
        self.gen += 1;
        if let Some(i) = self.entry_of(rec) {
            let e = self.entries[i as usize];
            let same_name = self.name(i) == name;
            if same_name && e.parent == parent && e.flags & DELETED == 0 {
                self.entries[i as usize].flags = flags;
                return;
            }
            if e.flags & DELETED == 0 {
                self.live -= 1;
            }
            let (name_off, name_len) = if same_name {
                (e.name_off, e.name_len)
            } else {
                self.dead_bytes += e.name_len as usize + 1;
                self.append_name(name, i)
            };
            self.entries[i as usize] = Entry { rec, parent, name_off, name_len, flags };
            self.live += 1;
        } else {
            self.push(rec, parent, name, flags);
        }
    }

    pub fn remove(&mut self, rec: u32) {
        if let Some(i) = self.entry_of(rec) {
            let e = &mut self.entries[i as usize];
            if e.flags & DELETED == 0 {
                e.flags |= DELETED;
                self.live -= 1;
                self.dead_bytes += e.name_len as usize + 1;
                self.gen += 1;
            }
            self.rec_map[rec as usize] = NONE;
        }
    }

    #[inline]
    pub fn name(&self, i: u32) -> &str {
        let e = &self.entries[i as usize];
        let s = &self.names[e.name_off as usize..e.name_off as usize + e.name_len as usize];
        // Names only ever enter the arena as &str.
        unsafe { std::str::from_utf8_unchecked(s) }
    }

    #[inline]
    pub fn folded_name(&self, i: u32) -> &[u8] {
        let e = &self.entries[i as usize];
        &self.folded[e.name_off as usize..e.name_off as usize + e.name_len as usize]
    }

    /// Walk up the parent chain, yielding entry indices of ancestors (nearest first).
    pub fn ancestors(&self, i: u32) -> Ancestors<'_> {
        Ancestors { vol: self, rec: self.entries[i as usize].parent, hops: 0 }
    }

    /// Full path of an entry, e.g. `C:\Users\ethan\notes.md`.
    pub fn path(&self, i: u32) -> String {
        let mut parts: Vec<u32> = self.ancestors(i).collect();
        parts.reverse();
        let mut s = String::with_capacity(self.root.len() + 64);
        s.push_str(&self.root);
        for p in parts {
            s.push_str(self.name(p));
            s.push('\\');
        }
        s.push_str(self.name(i));
        s
    }

    pub fn wasted_ratio(&self) -> f32 {
        self.dead_bytes as f32 / self.names.len().max(1) as f32
    }

    /// Rebuild arenas without dead names or deleted entries.
    pub fn compact(&mut self) {
        let fresh = Volume::new(self.root.clone(), self.root_rec, self.source)
            .with_capacity(self.live as usize, self.names.len().saturating_sub(self.dead_bytes));
        let old = std::mem::replace(self, fresh);
        self.serial = old.serial;
        self.journal_id = old.journal_id;
        self.next_usn = old.next_usn;
        self.gen = old.gen + 1;
        self.rec_map = vec![NONE; old.rec_map.len()];
        for (i, e) in old.entries.iter().enumerate() {
            if e.flags & DELETED == 0 {
                self.push(e.rec, e.parent, old.name(i as u32), e.flags);
            }
        }
    }

    pub fn memory_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<Entry>()
            + self.names.capacity()
            + self.folded.capacity()
            + (self.slot_off.capacity() + self.slot_ent.capacity() + self.rec_map.capacity()) * 4
    }
}

pub struct Ancestors<'a> {
    vol: &'a Volume,
    rec: u32,
    hops: u32,
}

impl Iterator for Ancestors<'_> {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        if self.rec == self.vol.root_rec || self.hops > 128 {
            return None;
        }
        let i = self.vol.entry_of(self.rec)?;
        let e = &self.vol.entries[i as usize];
        if e.parent == self.rec {
            return None;
        }
        self.rec = e.parent;
        self.hops += 1;
        Some(i)
    }
}

fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Case-fold so that the output has exactly the same byte length as the input.
/// That keeps `names` and `folded` offset-compatible. Characters whose
/// lowercase form has a different UTF-8 width are left untouched.
pub fn fold_into(s: &str, out: &mut Vec<u8>) {
    if s.is_ascii() {
        out.extend(s.bytes().map(|b| b.to_ascii_lowercase()));
        return;
    }
    let mut buf = [0u8; 4];
    for c in s.chars() {
        let mut low = c.to_lowercase();
        let l = if low.len() == 1 { low.next().unwrap() } else { c };
        let l = if l.len_utf8() == c.len_utf8() { l } else { c };
        out.extend_from_slice(l.encode_utf8(&mut buf).as_bytes());
    }
}

pub fn fold(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len());
    fold_into(s, &mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Volume {
        let mut v = Volume::new("C:\\", 5, Source::Walk);
        v.push(10, 5, "Users", DIR);
        v.push(11, 10, "Ethan", DIR);
        v.push(12, 11, "Notes.md", 0);
        v
    }

    #[test]
    fn paths() {
        let v = sample();
        assert_eq!(v.path(2), "C:\\Users\\Ethan\\Notes.md");
        assert_eq!(v.folded_name(2), b"notes.md");
    }

    #[test]
    fn rename_move_delete() {
        let mut v = sample();
        v.upsert(12, 10, "Todo.txt", 0);
        assert_eq!(v.path(2), "C:\\Users\\Todo.txt");
        v.remove(11);
        assert_eq!(v.live, 2);
        v.compact();
        assert_eq!(v.live, 2);
        let i = v.entry_of(12).unwrap();
        assert_eq!(v.path(i), "C:\\Users\\Todo.txt");
        assert!(v.entry_of(11).is_none());
    }

    #[test]
    fn fold_keeps_length() {
        for s in ["ÀÉÎõü", "İstanbul", "ẞig", "Straße", "日本語"] {
            assert_eq!(fold(s).len(), s.len(), "{s}");
        }
    }
}
