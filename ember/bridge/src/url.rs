//! `encodeURIComponent`, byte for byte, so URLs built here match the ones
//! `ember/proxy/static/detach.js` builds (checked by ember/vectors/bridge/detach_vectors.json).

/// Characters `encodeURIComponent` leaves alone: `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
}

pub(crate) fn encode_uri_component(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if unreserved(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::encode_uri_component;

    #[test]
    fn matches_encode_uri_component() {
        assert_eq!(encode_uri_component("/a b"), "%2Fa%20b");
        assert_eq!(encode_uri_component("A-z_0.!~*'()"), "A-z_0.!~*'()");
        assert_eq!(encode_uri_component("한"), "%ED%95%9C");
        assert_eq!(encode_uri_component("[\"x\",\"y\"]"), "%5B%22x%22%2C%22y%22%5D");
        assert_eq!(encode_uri_component("%3A"), "%253A");
    }
}
