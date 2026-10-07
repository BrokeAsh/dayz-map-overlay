//! Reader for Bohemia Interactive PBO archives (the `.pbo` files under `Addons`).
//!
//! Only the header is parsed up front; entry data is read on demand, so scanning
//! many archives for map tiles stays cheap.

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Packing method marking the optional "Vers" header extension.
const VERS: u32 = 0x5665_7273;
/// Packing method of an LZSS-compressed entry ("Cprs").
const CPRS: u32 = 0x4370_7273;

#[derive(Debug, Clone)]
pub struct PboEntry {
    /// Path inside the archive, with `\` separators as stored.
    pub name: String,
    pub method: u32,
    /// Unpacked size of a compressed entry.
    pub original_size: u32,
    pub offset: u64,
    pub size: u32,
}

#[derive(Debug, Clone)]
pub struct Pbo {
    pub path: PathBuf,
    /// The `prefix` header property, e.g. `DZ\worlds\enoch\data`.
    pub prefix: String,
    pub entries: Vec<PboEntry>,
}

fn read_cstr(r: &mut impl BufRead) -> Result<String> {
    let mut buf = Vec::new();
    r.read_until(0, &mut buf)?;
    if buf.pop() != Some(0) {
        bail!("unexpected end of PBO header");
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn read_u32(r: &mut impl Read) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

impl Pbo {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut r = BufReader::new(file);
        let mut prefix = String::new();
        let mut headers = Vec::new();
        let mut first = true;
        loop {
            let name = read_cstr(&mut r)?;
            let method = read_u32(&mut r)?;
            let original_size = read_u32(&mut r)?;
            let _reserved = read_u32(&mut r)?;
            let _timestamp = read_u32(&mut r)?;
            let size = read_u32(&mut r)?;
            if first && name.is_empty() && method == VERS {
                loop {
                    let key = read_cstr(&mut r)?;
                    if key.is_empty() {
                        break;
                    }
                    let value = read_cstr(&mut r)?;
                    if key.eq_ignore_ascii_case("prefix") {
                        prefix = value;
                    }
                }
                first = false;
                continue;
            }
            first = false;
            if name.is_empty() {
                break;
            }
            headers.push((name, method, original_size, size));
        }
        let mut offset = r.stream_position()?;
        let entries = headers
            .into_iter()
            .map(|(name, method, original_size, size)| {
                let entry = PboEntry {
                    name,
                    method,
                    original_size,
                    offset,
                    size,
                };
                offset += u64::from(size);
                entry
            })
            .collect();
        Ok(Self {
            path: path.to_owned(),
            prefix,
            entries,
        })
    }

    pub fn read(&self, entry: &PboEntry) -> Result<Vec<u8>> {
        let data = self.read_raw(entry, entry.size as usize)?;
        match entry.method {
            0 => Ok(data),
            CPRS => Ok(super::lzss::decompress(&data, entry.original_size as usize)),
            other => bail!("{}: unknown packing method 0x{other:08x}", entry.name),
        }
    }

    /// Reads at most the first `len` bytes of an entry.
    pub fn read_prefix(&self, entry: &PboEntry, len: usize) -> Result<Vec<u8>> {
        if entry.method != 0 {
            let mut data = self.read(entry)?;
            data.truncate(len);
            return Ok(data);
        }
        self.read_raw(entry, len)
    }

    fn read_raw(&self, entry: &PboEntry, len: usize) -> Result<Vec<u8>> {
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut data = vec![0; len.min(entry.size as usize)];
        file.read_exact(&mut data)?;
        Ok(data)
    }

    /// The full in-game path of an entry, such as `dz\worlds\enoch\world\enoch.wrp`.
    pub fn full_path(&self, entry: &PboEntry) -> String {
        let prefix = self.prefix.trim_matches('\\');
        if prefix.is_empty() {
            entry.name.to_ascii_lowercase()
        } else {
            format!("{prefix}\\{}", entry.name).to_ascii_lowercase()
        }
    }
}
