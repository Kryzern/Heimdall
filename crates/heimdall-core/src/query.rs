//! Query parsing: free-text tokens plus inline filters.
//!
//! ```text
//! report ext:pdf,docx in:downloads size:>2mb mod:week type:file
//! holiday type:image
//! ```

use crate::index::fold;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Any,
    Files,
    Dirs,
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    /// Case-folded name tokens; every one must match.
    pub tokens: Vec<Vec<u8>>,
    /// Lowercase extensions without the dot.
    pub exts: Vec<Vec<u8>>,
    /// Folder names that must appear somewhere above the result.
    pub within: Vec<Vec<u8>>,
    pub kind: Kind,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Unix seconds; only results modified after this.
    pub modified_after: Option<u64>,
}

impl Query {
    pub fn parse(input: &str) -> Query {
        let mut q = Query::default();
        for word in input.split_whitespace() {
            if let Some((key, val)) = word.split_once(':') {
                if val.is_empty() {
                    continue;
                }
                let handled = match key.to_ascii_lowercase().as_str() {
                    "ext" => {
                        q.exts.extend(
                            val.split(',').filter(|s| !s.is_empty()).map(|s| fold(s.trim_start_matches('.'))),
                        );
                        true
                    }
                    "in" => {
                        q.within.push(fold(val.trim_matches('\\')));
                        true
                    }
                    "type" | "kind" | "is" => {
                        let val = val.to_ascii_lowercase();
                        q.kind = match val.as_str() {
                            "dir" | "dirs" | "folder" | "folders" => Kind::Dirs,
                            "file" | "files" => Kind::Files,
                            _ => Kind::Any,
                        };
                        if let Some((_, exts)) = CATEGORIES.iter().find(|(names, _)| names.split(' ').any(|n| n == val)) {
                            q.exts.extend(exts.split(' ').map(|e| e.as_bytes().to_vec()));
                        }
                        true
                    }
                    "size" => {
                        if let Some(rest) = val.strip_prefix('>') {
                            q.min_size = parse_size(rest);
                        } else if let Some(rest) = val.strip_prefix('<') {
                            q.max_size = parse_size(rest);
                        } else {
                            q.min_size = parse_size(val);
                        }
                        true
                    }
                    "mod" | "modified" | "date" => {
                        q.modified_after = parse_age(val).map(|age| now_unix().saturating_sub(age));
                        true
                    }
                    // A drive letter ("c:") or anything else: treat as text.
                    _ => false,
                };
                if handled {
                    continue;
                }
            }
            // `projects\heimdall\main` -> within [projects, heimdall], token "main"
            if word.contains('\\') || word.contains('/') {
                let mut parts: Vec<&str> = word.split(['\\', '/']).filter(|s| !s.is_empty()).collect();
                if word.ends_with(['\\', '/']) {
                    q.within.extend(parts.drain(..).map(fold));
                } else if let Some(last) = parts.pop() {
                    q.within.extend(parts.into_iter().map(fold));
                    q.tokens.push(fold(last));
                }
                continue;
            }
            q.tokens.push(fold(word));
        }
        q
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.exts.is_empty() && self.within.is_empty()
    }

    pub fn needs_metadata(&self) -> bool {
        self.min_size.is_some() || self.max_size.is_some() || self.modified_after.is_some()
    }

    /// The most selective token: drives the SIMD prefilter.
    pub fn primary(&self) -> Option<&[u8]> {
        self.tokens.iter().max_by_key(|t| t.len()).map(|t| t.as_slice())
    }
}

/// `type:image` and friends: names (space separated) -> extensions.
pub const CATEGORIES: &[(&str, &str)] = &[
    ("image images picture pictures photo photos pic", "jpg jpeg png gif webp bmp tif tiff heic heif avif svg ico raw cr2 nef arw dng psd"),
    ("video videos movie movies", "mp4 mkv mov avi wmv webm m4v flv mpg mpeg ts 3gp"),
    ("audio music sound song songs", "mp3 flac wav m4a aac ogg opus wma aiff alac mid midi"),
    ("doc docs document documents", "pdf doc docx odt rtf txt md xls xlsx ods csv ppt pptx odp pages numbers key epub"),
    ("code source", "rs c h cpp hpp cc cs java kt go py js ts jsx tsx html css scss json toml yaml yml xml sh ps1 bat lua rb php swift sql vue svelte zig"),
    ("archive archives zip compressed", "zip rar 7z tar gz tgz bz2 xz zst cab iso"),
    ("app apps program programs exe", "exe msi lnk bat cmd appref-ms"),
];

fn parse_size(s: &str) -> Option<u64> {
    let s = s.to_ascii_lowercase();
    let idx = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(idx);
    let n: f64 = num.parse().ok()?;
    let mul = match unit.trim_end_matches('b') {
        "" => 1.0,
        "k" => 1024.0,
        "m" => 1024.0 * 1024.0,
        "g" => 1024.0 * 1024.0 * 1024.0,
        "t" => 1024f64.powi(4),
        _ => return None,
    };
    Some((n * mul) as u64)
}

fn parse_age(s: &str) -> Option<u64> {
    const DAY: u64 = 86_400;
    Some(match s.to_ascii_lowercase().as_str() {
        "today" => {
            // Seconds since local midnight is not worth a timezone dependency;
            // "today" means the last 24 hours.
            DAY
        }
        "yesterday" => 2 * DAY,
        "week" => 7 * DAY,
        "month" => 31 * DAY,
        "year" => 365 * DAY,
        other => {
            let idx = other.find(|c: char| !c.is_ascii_digit()).unwrap_or(other.len());
            let n: u64 = other[..idx].parse().ok()?;
            n * match &other[idx..] {
                "h" => 3600,
                "d" | "" => DAY,
                "w" => 7 * DAY,
                "m" => 31 * DAY,
                "y" => 365 * DAY,
                _ => return None,
            }
        }
    })
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters() {
        let q = Query::parse("Report ext:pdf,.DOCX in:Downloads size:>2mb type:file");
        assert_eq!(q.tokens, vec![b"report".to_vec()]);
        assert_eq!(q.exts, vec![b"pdf".to_vec(), b"docx".to_vec()]);
        assert_eq!(q.within, vec![b"downloads".to_vec()]);
        assert_eq!(q.min_size, Some(2 * 1024 * 1024));
        assert_eq!(q.kind, Kind::Files);
    }

    #[test]
    fn categories() {
        let q = Query::parse("holiday type:image");
        assert_eq!(q.tokens, vec![b"holiday".to_vec()]);
        assert!(q.exts.contains(&b"jpg".to_vec()) && q.exts.contains(&b"png".to_vec()));
        assert_eq!(q.kind, Kind::Any);
        assert!(!Query::parse("type:video").is_empty());
    }

    #[test]
    fn path_tokens() {
        let q = Query::parse("heimdall\\src\\main");
        assert_eq!(q.within, vec![b"heimdall".to_vec(), b"src".to_vec()]);
        assert_eq!(q.tokens, vec![b"main".to_vec()]);
    }
}
