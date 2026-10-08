//! LZO1X decompression, for the compressed mipmaps in PAA textures.
//!
//! Every read and back-reference is checked, and the output can't grow past the size the
//! caller expects, so a corrupt or crafted texture from a mod fails cleanly instead of
//! panicking or eating memory. (Follows the reference decoder, `lzo1x_d.ch`.)

use anyhow::{Result, bail};

/// Decodes `input`, which must produce at most `limit` bytes.
pub fn decompress(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut d = Decoder {
        input,
        ip: 0,
        out: Vec::with_capacity(limit.min(input.len().saturating_mul(256))),
        limit,
    };
    d.run()?;
    Ok(d.out)
}

struct Decoder<'a> {
    input: &'a [u8],
    ip: usize,
    out: Vec<u8>,
    limit: usize,
}

impl Decoder<'_> {
    fn byte(&mut self) -> Result<usize> {
        let Some(&b) = self.input.get(self.ip) else {
            bail!("LZO data is truncated");
        };
        self.ip += 1;
        Ok(usize::from(b))
    }

    fn le16(&mut self) -> Result<usize> {
        Ok(self.byte()? | self.byte()? << 8)
    }

    /// A length continued in extra bytes: each zero byte adds 255, then one more byte.
    fn long_length(&mut self, base: usize) -> Result<usize> {
        let mut length = base;
        while self.input.get(self.ip) == Some(&0) {
            self.ip += 1;
            length = length.saturating_add(255);
        }
        Ok(length.saturating_add(self.byte()?))
    }

    fn literals(&mut self, count: usize) -> Result<()> {
        let Some(bytes) = self.input.get(self.ip..self.ip.saturating_add(count)) else {
            bail!("LZO data is truncated");
        };
        if self.out.len() + count > self.limit {
            bail!("LZO data is larger than expected");
        }
        self.out.extend_from_slice(bytes);
        self.ip += count;
        Ok(())
    }

    /// Copies `length` bytes from `distance` back (they may overlap what's being written).
    fn copy_match(&mut self, distance: usize, length: usize) -> Result<()> {
        if distance == 0 || distance > self.out.len() {
            bail!("LZO data refers before its start");
        }
        if self.out.len().saturating_add(length) > self.limit {
            bail!("LZO data is larger than expected");
        }
        let start = self.out.len() - distance;
        for i in 0..length {
            let b = self.out[start + i];
            self.out.push(b);
        }
        Ok(())
    }

    fn run(&mut self) -> Result<()> {
        // After a match, how many literals followed it (0 to 3), or 4 after a literal run.
        let mut state;
        let first = self.byte()?;
        if first > 17 {
            let count = first - 17;
            self.literals(count)?;
            state = if count < 4 { count } else { 4 };
        } else {
            self.ip -= 1;
            state = 0;
        }
        loop {
            let t = self.byte()?;
            let (distance, length);
            if t < 16 {
                if state == 0 {
                    // A literal run.
                    let count = if t == 0 { self.long_length(15)? } else { t } + 3;
                    self.literals(count)?;
                    state = 4;
                    continue;
                }
                let low = (t >> 2) + (self.byte()? << 2);
                if state == 4 {
                    // A three-byte match just past the short-distance range.
                    (distance, length) = (1 + 0x800 + low, 3);
                } else {
                    (distance, length) = (1 + low, 2);
                }
            } else if t >= 64 {
                distance = 1 + ((t >> 2) & 7) + (self.byte()? << 3);
                length = (t >> 5) + 1;
            } else if t >= 32 {
                let count = t & 31;
                length = if count == 0 {
                    self.long_length(31)?
                } else {
                    count
                } + 2;
                let next = self.le16()?;
                distance = 1 + (next >> 2);
                state = next & 3;
                self.copy_match(distance, length)?;
                self.literals(state)?;
                continue;
            } else {
                let count = t & 7;
                length = if count == 0 {
                    self.long_length(7)?
                } else {
                    count
                } + 2;
                let next = self.le16()?;
                let far = ((t & 8) << 11) + (next >> 2);
                if far == 0 {
                    return Ok(()); // end of stream
                }
                distance = far + 0x4000;
                state = next & 3;
                self.copy_match(distance, length)?;
                self.literals(state)?;
                continue;
            }
            state = t & 3;
            self.copy_match(distance, length)?;
            self.literals(state)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decompress;

    #[test]
    fn rejects_references_before_the_start() {
        // Five literals, then a match reaching back past them.
        assert!(decompress(&[0x16, 0, 0, 0, 0, 0, 0x40, 0xff], 16).is_err());
    }

    #[test]
    fn decodes_literals_and_matches() {
        // "abcd", then "abcd" again from four back, then the end marker.
        let data = [0x15, b'a', b'b', b'c', b'd', 0x6c, 0x00, 0x11, 0x00, 0x00];
        assert_eq!(decompress(&data, 64).unwrap(), b"abcdabcd");
        assert!(decompress(&data, 6).is_err());
    }
}
