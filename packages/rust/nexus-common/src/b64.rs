// Standard base64 (RFC 4648, with padding on encode). Used to carry arbitrary
// text (quotes, newlines, unicode) safely through the flat-JSON IPC transport.
// Decode is strict: any character outside the alphabet (other than padding and
// whitespace) yields an empty result — the transport fails closed.

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 encode (with padding).
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18 & 63) as usize] as char);
        out.push(B64[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Standard base64 decode; returns empty on malformed input.
pub fn decode(s: &str) -> Vec<u8> {
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=' && !c.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        let mut bits = 0;
        for &c in chunk {
            let v = match val(c) {
                Some(v) => v,
                None => return Vec::new(),
            };
            n = (n << 6) | v;
            bits += 6;
        }
        n <<= 24 - bits;
        let nbytes = (bits) / 8;
        for i in 0..nbytes {
            out.push((n >> (16 - i * 8) & 0xff) as u8);
        }
    }
    out
}

/// Decode a base64 field into a UTF-8 string (lossy for invalid UTF-8).
pub fn to_utf8(s: &str) -> String {
    String::from_utf8_lossy(&decode(s)).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for case in [&b""[..], b"a", b"ab", b"abc", b"hello world", b"\xff\x00\x01"] {
            assert_eq!(decode(&encode(case)), case.to_vec());
        }
        assert_eq!(to_utf8(&encode("h\u{e9}llo \u{1f510}".as_bytes())), "h\u{e9}llo \u{1f510}");
    }

    #[test]
    fn known_vectors() {
        assert_eq!(encode(b"Man"), "TWFu");
        assert_eq!(encode(b"Ma"), "TWE=");
        assert_eq!(encode(b"M"), "TQ==");
        assert_eq!(decode("aGVsbG8="), b"hello".to_vec());
        assert_eq!(decode("aGVsbG8"), b"hello".to_vec()); // padding optional
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode("not base64!!").is_empty());
    }
}
