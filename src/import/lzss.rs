//! Bohemia's LZSS variant, used for compressed PBO entries.
//!
//! Each flag byte covers eight items: a set bit is a literal byte, a clear bit a two-byte
//! back-reference (12-bit distance, 4-bit length + 3). References before the start of the
//! output read as spaces. A four-byte checksum follows the data.

pub fn decompress(input: &[u8], expected: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(expected);
    let mut i = 0;
    while out.len() < expected && i < input.len() {
        let flags = input[i];
        i += 1;
        for bit in 0..8 {
            if out.len() >= expected || i >= input.len() {
                break;
            }
            if flags & (1 << bit) != 0 {
                out.push(input[i]);
                i += 1;
            } else {
                let Some(&[a, b]) = input
                    .get(i..i + 2)
                    .map(|s| <&[u8; 2]>::try_from(s).unwrap())
                else {
                    break;
                };
                i += 2;
                let distance = a as usize | ((b as usize & 0xf0) << 4);
                let len = (b as usize & 0x0f) + 3;
                for _ in 0..len.min(expected - out.len()) {
                    let byte = out.len().checked_sub(distance).map_or(b' ', |at| out[at]);
                    out.push(byte);
                }
            }
        }
    }
    out
}
