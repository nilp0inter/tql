//! Minimal bencode scanner used to recover the BTIH info hash from a
//! `.torrent` file (Leg 12c).
//!
//! BEP 3 defines info_hash as `SHA1` of the bencoded `info` dictionary as it
//! appears in the file — byte-for-byte, no re-encoding. So we don't actually
//! need to *parse* bencode into typed values; we only need to find the byte
//! range covered by the value associated with the top-level `info` key.

use sha1::{Digest, Sha1};

#[derive(Debug, PartialEq, Eq)]
pub enum BencodeError {
    NotADict,
    Truncated,
    Malformed,
    InfoMissing,
}

impl std::fmt::Display for BencodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BencodeError::NotADict => f.write_str("top-level value is not a bencoded dictionary"),
            BencodeError::Truncated => f.write_str("bencode truncated"),
            BencodeError::Malformed => f.write_str("bencode malformed"),
            BencodeError::InfoMissing => f.write_str("no `info` key in top-level dictionary"),
        }
    }
}

impl std::error::Error for BencodeError {}

/// Compute the BTIH info hash (40-char lowercase hex) of a `.torrent` file.
pub fn compute_info_hash(bytes: &[u8]) -> Result<String, BencodeError> {
    let (start, end) = find_info_range(bytes)?;
    let mut hasher = Sha1::new();
    hasher.update(&bytes[start..end]);
    let digest = hasher.finalize();
    Ok(hex_lower(&digest))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Find the byte range `[start, end)` covering the value bound to the
/// top-level `info` key.
fn find_info_range(bytes: &[u8]) -> Result<(usize, usize), BencodeError> {
    if bytes.first() != Some(&b'd') {
        return Err(BencodeError::NotADict);
    }
    let mut pos = 1;
    while pos < bytes.len() && bytes[pos] != b'e' {
        // Read key (must be a bytestring).
        let (key_start, key_len, after_key) = parse_bytestring_meta(bytes, pos)?;
        let key = &bytes[key_start..key_start + key_len];
        pos = after_key;
        // Read value.
        let value_start = pos;
        let value_end = scan_element(bytes, pos)?;
        if key == b"info" {
            return Ok((value_start, value_end));
        }
        pos = value_end;
    }
    Err(BencodeError::InfoMissing)
}

/// Bytestring `<len>:<bytes>` → `(content_start, content_len, end)`.
fn parse_bytestring_meta(bytes: &[u8], pos: usize) -> Result<(usize, usize, usize), BencodeError> {
    let colon = find_byte(bytes, pos, b':').ok_or(BencodeError::Malformed)?;
    let len_str = std::str::from_utf8(&bytes[pos..colon]).map_err(|_| BencodeError::Malformed)?;
    let len: usize = len_str.parse().map_err(|_| BencodeError::Malformed)?;
    let content_start = colon + 1;
    let end = content_start
        .checked_add(len)
        .ok_or(BencodeError::Malformed)?;
    if end > bytes.len() {
        return Err(BencodeError::Truncated);
    }
    Ok((content_start, len, end))
}

fn find_byte(bytes: &[u8], from: usize, target: u8) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|b| *b == target)
        .map(|i| i + from)
}

/// Return the byte index *after* the bencoded element that starts at `pos`.
fn scan_element(bytes: &[u8], pos: usize) -> Result<usize, BencodeError> {
    let b = *bytes.get(pos).ok_or(BencodeError::Truncated)?;
    match b {
        b'i' => {
            let e = find_byte(bytes, pos + 1, b'e').ok_or(BencodeError::Malformed)?;
            Ok(e + 1)
        }
        b'l' | b'd' => {
            let mut cur = pos + 1;
            while cur < bytes.len() && bytes[cur] != b'e' {
                if b == b'd' {
                    // dict: key (bytestring), then value
                    let (_, _, after_key) = parse_bytestring_meta(bytes, cur)?;
                    cur = after_key;
                }
                cur = scan_element(bytes, cur)?;
            }
            if cur >= bytes.len() {
                return Err(BencodeError::Truncated);
            }
            Ok(cur + 1)
        }
        b'0'..=b'9' => {
            let (_, _, end) = parse_bytestring_meta(bytes, pos)?;
            Ok(end)
        }
        _ => Err(BencodeError::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny but valid bencoded torrent with a known info dict so we
    /// can hand-verify the SHA1 boundary logic.
    fn synth_torrent(info_payload: &[u8]) -> Vec<u8> {
        // Top-level dict with keys "announce" and "info" (sorted).
        // announce = "udp://x:1"
        let announce_v = b"udp://x:1";
        let mut out = Vec::new();
        out.push(b'd');
        // "announce"
        out.extend_from_slice(b"8:announce");
        out.extend_from_slice(format!("{}:", announce_v.len()).as_bytes());
        out.extend_from_slice(announce_v);
        // "info"
        out.extend_from_slice(b"4:info");
        out.extend_from_slice(info_payload);
        out.push(b'e');
        out
    }

    #[test]
    fn finds_info_dict_in_simple_torrent() {
        // info = d6:lengthi12e4:name3:fooe
        let info = b"d6:lengthi12e4:name3:fooe";
        let blob = synth_torrent(info);
        let (s, e) = find_info_range(&blob).unwrap();
        assert_eq!(&blob[s..e], info);
    }

    #[test]
    fn info_hash_matches_sha1_of_info_bytes() {
        let info = b"d6:lengthi12e4:name3:fooe";
        let blob = synth_torrent(info);
        let got = compute_info_hash(&blob).unwrap();

        // Independently compute the expected hash over the same byte range.
        let mut h = Sha1::new();
        h.update(info);
        let expected = hex_lower(&h.finalize());
        assert_eq!(got, expected);
        assert_eq!(got.len(), 40);
    }

    #[test]
    fn rejects_non_dict_top_level() {
        assert!(matches!(
            compute_info_hash(b"i42e"),
            Err(BencodeError::NotADict)
        ));
    }

    #[test]
    fn rejects_missing_info_key() {
        // dict with only "announce"
        let blob = b"d8:announce5:hello e".to_vec();
        let mut clean = Vec::new();
        clean.push(b'd');
        clean.extend_from_slice(b"8:announce5:hello");
        clean.push(b'e');
        assert!(matches!(
            compute_info_hash(&clean),
            Err(BencodeError::InfoMissing)
        ));
        let _ = blob;
    }

    #[test]
    fn handles_nested_dicts_and_lists() {
        // info = d5:filesld6:lengthi1e4:pathl1:ae e e
        // properly:
        //   d 5:files l d 6:length i1e 4:path l 1:a e e e e
        let info: &[u8] = b"d5:filesld6:lengthi1e4:pathl1:aeeee";
        let blob = synth_torrent(info);
        let (s, e) = find_info_range(&blob).unwrap();
        assert_eq!(&blob[s..e], info);
    }

    #[test]
    fn truncated_input_is_an_error() {
        let truncated = b"d4:info";
        assert!(compute_info_hash(truncated).is_err());
    }
}
