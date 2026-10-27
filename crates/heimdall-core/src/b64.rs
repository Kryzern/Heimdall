//! Base64 for the `b64` and `b64d` prefixes. Decoding accepts the standard and
//! URL-safe alphabets, with or without padding, and ignores whitespace.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn decode(input: &str) -> Option<Vec<u8>> {
    let mut digits: Vec<u8> = input.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    for _ in 0..2 {
        if digits.last() == Some(&b'=') {
            digits.pop();
        }
    }
    if digits.is_empty() || digits.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in digits {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Decode to text, or `None` when the input isn't base64 of UTF-8.
pub fn decode_text(input: &str) -> Option<String> {
    String::from_utf8(decode(input)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar", "héllo wörld"] {
            assert_eq!(decode_text(&encode(s.as_bytes())).as_deref(), if s.is_empty() { None } else { Some(s) });
        }
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode(b"fo"), "Zm8=");
    }

    #[test]
    fn lenient_decode() {
        assert_eq!(decode_text("Zm8").as_deref(), Some("fo"));
        assert_eq!(decode_text("Zm9v YmFy\n").as_deref(), Some("foobar"));
        assert_eq!(decode("-_8"), decode("+/8="));
        assert_eq!(decode("Zm9v!"), None);
        assert_eq!(decode("Z"), None);
        assert_eq!(decode_text("/w=="), None);
    }
}
