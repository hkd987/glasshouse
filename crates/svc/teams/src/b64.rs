//! Base64url without padding (RFC 4648 §5), as JWTs and JWKs carry it.

/// The bytes `text` encodes, or `None` if it isn't canonical base64url
/// without padding.
pub(crate) fn decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for b in text.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // Left-over bits must be zero, so each text has one meaning.
    (acc == 0).then_some(out)
}

/// `bytes` as base64url without padding.
#[cfg(any(test, feature = "testing"))]
pub(crate) fn encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_refuses_what_isnt_base64url() {
        for text in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            let encoded = encode(text.as_bytes());
            assert!(!encoded.contains('='), "{encoded}");
            assert_eq!(decode(&encoded).as_deref(), Some(text.as_bytes()));
        }
        assert_eq!(encode(b"\xfb\xff"), "-_8");
        assert_eq!(decode("Zm9vYg"), Some(b"foob".to_vec()));
        assert_eq!(decode("Zm9vYg=="), None, "no padding");
        assert_eq!(decode("Zm9v+g"), None, "not the URL alphabet");
        assert_eq!(decode("Zm9vYh"), None, "left-over bits");
        assert_eq!(decode("Z"), None);
    }
}
