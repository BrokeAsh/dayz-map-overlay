//! Reads the terrain size from a `.wrp` world file header.
//!
//! Both formats start with the texture-layer grid (cells per side, twice), the heightmap grid
//! (twice), and the layer cell size in metres, so the terrain is `cells * cell size` across.
//! Version 29 OPRW files insert one extra byte before those fields, so a few offsets are tried
//! and the first plausible one wins.

/// Bytes needed from the start of the file.
pub const HEADER_LEN: usize = 40;

pub fn terrain_size(header: &[u8]) -> Option<f64> {
    let offsets: &[usize] = match header.get(..4)? {
        b"8WVR" => &[4],
        b"OPRW" => &[16, 17, 12, 20],
        _ => return None,
    };
    offsets.iter().find_map(|&at| {
        let int = |i: usize| -> Option<i32> {
            Some(i32::from_le_bytes(
                header.get(at + i * 4..at + i * 4 + 4)?.try_into().ok()?,
            ))
        };
        let (cells_x, cells_y, height_x, height_y) = (int(0)?, int(1)?, int(2)?, int(3)?);
        let cell = f32::from_le_bytes(header.get(at + 16..at + 20)?.try_into().ok()?);
        let size = f64::from(cells_x) * f64::from(cell);
        let plausible = cells_x == cells_y
            && height_x == height_y
            && (16..=16384).contains(&cells_x)
            && height_x >= cells_x
            && (1.0..=500.0).contains(&cell)
            && (500.0..=100_000.0).contains(&size);
        plausible.then_some(size)
    })
}

/// Objects placed on a terrain, from an OPRW world file.
///
/// After the compressed height and texture data, the file lists every model path, then the
/// "classed" objects (buildings with a config class, which is how the game attaches scripts such
/// as drinkable wells), then road networks, then one 60-byte record per placed object. Only the
/// pieces needed here are read, and the object table is found by its record shape rather than by
/// walking the quadtrees in front of it.
pub struct Objects {
    pub models: Vec<String>,
    pub classed: Vec<ClassedObject>,
    /// Placed objects whose model passed the filter: (model index, x, z).
    pub placed: Vec<(u32, f32, f32)>,
}

pub struct ClassedObject {
    /// Lower-case config class, such as `land_misc_well_pump_blue`.
    pub class: String,
    pub x: f32,
    pub z: f32,
}

/// Object record: id, model index, 3x4 transform (rotation rows, then position), flags.
const RECORD: usize = 60;

pub fn objects(data: &[u8], keep_model: impl Fn(&str) -> bool) -> Option<Objects> {
    if data.get(..4)? != b"OPRW" {
        return None;
    }
    let (models, after_models) = model_list(data)?;
    let (classed, after_classed) = classed_objects(data, after_models)?;
    let keep: Vec<bool> = models.iter().map(|m| keep_model(m)).collect();
    let mut placed = Vec::new();
    let table = object_table(data, after_classed, models.len());
    log::debug!(
        "{} models, {} classed objects, object table {table:?}",
        models.len(),
        classed.len()
    );
    if let Some((start, end)) = table {
        for record in data[start..end].as_chunks::<RECORD>().0 {
            let model = u32_at(record, 4)?;
            if keep[model as usize] && record_kind(record, models.len()) != Record::Empty {
                placed.push((model, f32_at(record, 44)?, f32_at(record, 52)?));
            }
        }
    }
    Some(Objects {
        models,
        classed,
        placed,
    })
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn f32_at(data: &[u8], at: usize) -> Option<f32> {
    u32_at(data, at).map(f32::from_bits)
}

fn cstr(data: &[u8], at: usize) -> Option<(&str, usize)> {
    let len = data.get(at..)?.iter().position(|&b| b == 0)?;
    let text = std::str::from_utf8(&data[at..at + len]).ok()?;
    Some((text, at + len + 1))
}

fn printable(b: u8) -> bool {
    (0x20..0x7f).contains(&b)
}

/// The model path table: a count followed by that many `.p3d` paths. Returns it and where it ends.
fn model_list(data: &[u8]) -> Option<(Vec<String>, usize)> {
    let mut from = 0;
    while let Some(found) = find(&data[from..], b".p3d\0") {
        let end = from + found;
        from = end + 5;
        let mut start = end;
        while start > 0 && printable(data[start - 1]) {
            start -= 1;
        }
        // The count's low bytes can look like text, so try a few starts.
        for skip in 0..4 {
            let first = start + skip;
            if first < 4 || first >= end {
                break;
            }
            let Some(count) = u32_at(data, first - 4).filter(|&n| (1..500_000).contains(&n)) else {
                continue;
            };
            if let Some(list) = read_models(data, first, count as usize) {
                return Some(list);
            }
        }
    }
    None
}

fn read_models(data: &[u8], mut at: usize, count: usize) -> Option<(Vec<String>, usize)> {
    let mut models = Vec::with_capacity(count);
    for _ in 0..count {
        let (path, next) = cstr(data, at)?;
        if !path.to_ascii_lowercase().ends_with(".p3d") || !path.bytes().all(printable) {
            return None;
        }
        models.push(path.to_ascii_lowercase());
        at = next;
    }
    Some((models, at))
}

/// Class name, model path, position (x, height, z), and two ids per entry.
fn classed_objects(data: &[u8], at: usize) -> Option<(Vec<ClassedObject>, usize)> {
    let count = u32_at(data, at)? as usize;
    if count > 5_000_000 {
        return None;
    }
    let mut at = at + 4;
    let mut objects = Vec::with_capacity(count);
    for _ in 0..count {
        let (class, next) = cstr(data, at)?;
        let (_model, next) = cstr(data, next)?;
        if class.is_empty() || !class.bytes().all(printable) {
            return None;
        }
        objects.push(ClassedObject {
            class: class.to_ascii_lowercase(),
            x: f32_at(data, next)?,
            z: f32_at(data, next + 8)?,
        });
        at = next + 20;
    }
    Some((objects, at))
}

#[derive(PartialEq)]
enum Record {
    /// Certainly an object: a real rotation, a known model, a position on the map.
    Object,
    /// Probably an object (some have an all-zero rotation); fine inside a run, not to start one.
    Loose,
    /// Padding (seen at the start of the table).
    Empty,
    Invalid,
}

fn record_kind(record: &[u8], models: usize) -> Record {
    let word = |i: usize| u32::from_le_bytes(record[i * 4..i * 4 + 4].try_into().unwrap());
    let float = |i: usize| f32::from_bits(word(i));
    if record.iter().all(|&b| b == 0) {
        return Record::Empty;
    }
    let loose = (word(1) as usize) < models
        && word(14) <= 0xffff
        && float(11).abs() < 1e6
        && float(13).abs() < 1e6
        && float(12).abs() < 1e5;
    let row = (float(2).powi(2) + float(3).powi(2) + float(4).powi(2)).sqrt();
    match (loose, (0.01..100.0).contains(&row)) {
        (true, true) => Record::Object,
        (true, false) => Record::Loose,
        (false, _) => Record::Invalid,
    }
}

/// The byte range of the object table: the longest run of records that look like objects.
/// (Deer Isle has a run of about a thousand look-alikes in front of its 3.4 million objects.)
fn object_table(data: &[u8], from: usize, models: usize) -> Option<(usize, usize)> {
    let kind = |at: usize| match data.get(at..at + RECORD) {
        Some(record) => record_kind(record, models),
        None => Record::Invalid,
    };
    let mut best: Option<(usize, usize)> = None;
    let mut at = from;
    while at + RECORD <= data.len() {
        if kind(at) != Record::Object {
            at += 1;
            continue;
        }
        let mut end = at;
        while kind(end) != Record::Invalid {
            end += RECORD;
        }
        let records = (end - at) / RECORD;
        if best.is_none_or(|(s, e)| records > (e - s) / RECORD) {
            best = Some((at, end));
        }
        // Other tables can look like a few objects in a row; the real one is by far the longest.
        at = if records > 1 { end } else { at + 1 };
    }
    best
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
