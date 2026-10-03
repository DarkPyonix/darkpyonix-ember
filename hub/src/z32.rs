//! z-base-32, the key encoding of the hub's `/pkarr/{key}` paths (52 characters for a 32-byte
//! key, most significant bit first, no padding).

const ALPHABET: &[u8; 32] = b"ybndrfg8ejkmcpqxot1uwisza345h769";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &b in bytes {
        buffer = (buffer << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(ALPHABET[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Decodes a canonical 52-character key; `None` if malformed.
pub fn decode_key(s: &str) -> Option<[u8; 32]> {
    if s.len() != 52 {
        return None;
    }
    let mut out = [0u8; 32];
    let mut i = 0;
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        buffer = (buffer << 5) | v;
        bits += 5;
        if bits >= 8 {
            out[i] = ((buffer >> (bits - 8)) & 0xff) as u8;
            i += 1;
            bits -= 8;
            buffer &= (1 << bits) - 1;
        }
    }
    // 260 bits: the last 4 are padding and must be zero.
    (i == 32 && buffer == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for seed in 0..20u8 {
            let key: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(31).wrapping_add(seed));
            let s = encode(&key);
            assert_eq!(s.len(), 52);
            assert_eq!(decode_key(&s), Some(key));
        }
        assert_eq!(encode(&[0u8; 32]), "y".repeat(52));
        assert_eq!(decode_key("short"), None);
        assert_eq!(decode_key(&"0".repeat(52)), None);
    }
}
