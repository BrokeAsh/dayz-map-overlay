//! Decoder for Bohemia's PAA texture format, as used by terrain layer tiles.

use anyhow::{Context, Result, bail};
use image::RgbaImage;

const DXT1: u16 = 0xFF01;
const DXT3: u16 = 0xFF03;
const DXT5: u16 = 0xFF05;

/// Decodes the largest mipmap of a DXT-compressed PAA into RGBA.
pub fn decode(data: &[u8]) -> Result<RgbaImage> {
    let mut r = Cursor { data, pos: 0 };
    let format = match r.u16()? {
        DXT1 => texpresso::Format::Bc1,
        DXT3 => texpresso::Format::Bc2,
        DXT5 => texpresso::Format::Bc3,
        other => bail!("unsupported PAA format 0x{other:04x}"),
    };
    // Tagged metadata blocks ("GGATCGVA", "GGATSFFO", ...).
    while r.peek(4)? == b"GGAT" {
        r.skip(8)?;
        let len = r.u32()? as usize;
        r.skip(len)?;
    }
    let palette = r.u16()? as usize;
    r.skip(palette * 3)?;

    let raw_width = r.u16()?;
    let height = r.u16()? as usize;
    let len = r.u24()?;
    let payload = r.take(len)?;
    let width = (raw_width & 0x7FFF) as usize;
    if width == 0 || height == 0 {
        bail!("PAA has no mipmaps");
    }
    // Terrain tiles are at most a few thousand pixels; this keeps a bad header from asking for
    // gigabytes.
    if width > 8192 || height > 8192 {
        bail!("PAA is too large ({width}x{height})");
    }
    let expected = format.compressed_size(width, height);
    let blocks = if raw_width & 0x8000 != 0 {
        lzokay_native::decompress_all(payload, Some(expected))
            .map_err(|e| anyhow::anyhow!("LZO: {e:?}"))
            .context("decompressing PAA mipmap")?
    } else {
        payload.to_vec()
    };
    if blocks.len() < expected {
        bail!(
            "PAA mipmap is truncated ({} of {expected} bytes)",
            blocks.len()
        );
    }
    let mut rgba = vec![0; width * height * 4];
    format.decompress(&blocks[..expected], width, height, &mut rgba);
    Ok(RgbaImage::from_raw(width as u32, height as u32, rgba).expect("buffer size matches"))
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let Some(end) = end else {
            bail!("PAA is truncated")
        };
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }
    fn peek(&self, n: usize) -> Result<&'a [u8]> {
        self.data
            .get(self.pos..self.pos + n)
            .context("PAA is truncated")
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    fn u24(&mut self) -> Result<usize> {
        let b = self.take(3)?;
        Ok(b[0] as usize | (b[1] as usize) << 8 | (b[2] as usize) << 16)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
}
