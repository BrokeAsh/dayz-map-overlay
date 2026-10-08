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
    /// The file's `stamp` when the header was read; entry offsets are only good while it holds.
    pub opened: String,
}

/// Size and modification time, which change when Steam updates the file.
pub fn stamp(path: &Path) -> String {
    std::fs::metadata(path)
        .map(|m| {
            let modified = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok());
            format!("{}:{}", m.len(), modified.map_or(0, |d| d.as_secs()))
        })
        .unwrap_or_default()
}

/// Whether a read failed in the file system (locked, unplugged, unreadable) rather than in the
/// data, so trying again later may work.
pub fn is_io(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>().is_some()
}

fn read_cstr(r: &mut impl BufRead) -> Result<String> {
    // Names and header values are short; a file without NULs mustn't be read whole.
    let mut buf = Vec::new();
    r.take(1024).read_until(0, &mut buf)?;
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
        let opened = stamp(path);
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
            opened,
        })
    }

    /// Whether the file changed since it was opened (a mod update), so its entries are stale.
    pub fn changed(&self) -> bool {
        stamp(&self.path) != self.opened
    }

    /// Reads and unpacks an entry. Errors from the file system come back as `io::Error`
    /// (see [`is_io`]); anything else means the entry itself is bad.
    pub fn read(&self, entry: &PboEntry) -> Result<Vec<u8>> {
        self.unpack(entry, usize::MAX)
    }

    /// Reads at most the first `len` bytes of an entry (only those are unpacked).
    pub fn read_prefix(&self, entry: &PboEntry, len: usize) -> Result<Vec<u8>> {
        if entry.method == 0 {
            return self.read_raw(entry, len);
        }
        self.unpack(entry, len)
    }

    fn unpack(&self, entry: &PboEntry, len: usize) -> Result<Vec<u8>> {
        match entry.method {
            0 => {
                let mut data = self.read_raw(entry, entry.size as usize)?;
                data.truncate(len);
                Ok(data)
            }
            CPRS => {
                // Packed entries in real mods are at most a few tens of MB; a crafted header
                // could otherwise unpack gigabytes from a small archive.
                const MAX_UNPACKED: u32 = 64 << 20;
                if entry.original_size > MAX_UNPACKED {
                    bail!("{}: compressed entry too large", entry.name);
                }
                let packed = self.read_raw(entry, entry.size as usize)?;
                let want = len.min(entry.original_size as usize);
                let data = super::lzss::decompress(&packed, want);
                if data.len() != want {
                    bail!("{}: corrupt compressed entry", entry.name);
                }
                Ok(data)
            }
            other => bail!("{}: unknown packing method 0x{other:08x}", entry.name),
        }
    }

    fn read_raw(&self, entry: &PboEntry, len: usize) -> Result<Vec<u8>> {
        let mut file = File::open(&self.path)?;
        // Sizes come from the header; check them against the file before allocating.
        if entry.offset + u64::from(entry.size) > file.metadata()?.len() {
            bail!(
                "{}: {} is shorter than its header says (it may have changed since it was \
                 scanned)",
                entry.name,
                self.path.display()
            );
        }
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
