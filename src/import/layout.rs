//! Works out where terrain layer tiles sit in the world.
//!
//! Each `layers\P_XXX-YYY_*.rvmat` material maps world positions to its tile's UVs with a
//! `uvTransform`: `u = x * aside.x + pos.x` and `v = z * dir.y + pos.y`. Reading two neighbouring
//! tiles' transforms gives the tile size in metres, the overlap between tiles, and where the grid
//! starts. The satellite grid can reach past the terrain edge, so the terrain size itself comes
//! from the `.wrp` file when available.

use anyhow::{Context, Result, bail};

use super::pbo::Pbo;

#[derive(Debug, Clone, Copy)]
pub struct TileLayout {
    /// Fraction of a tile that overlaps the neighbour on each side.
    pub overlap_frac: f64,
    /// Metres between neighbouring tiles (the size of a tile once the overlap is cropped).
    pub step_m: f64,
    /// World x of the grid's west edge.
    pub left: f64,
    /// World z of the grid's north edge.
    pub top: f64,
}

impl TileLayout {
    pub fn from_rvmats(pbo: &Pbo) -> Result<Self> {
        let origin = world_uv_transform(pbo, "p_000-000")?;
        let next = world_uv_transform(pbo, "p_001-000")?;
        // (Written so NaN fails too.)
        if !(origin.aside > 0.0 && origin.aside.is_finite()) || !(origin.dir_v != 0.0) {
            bail!("unexpected uvTransform in {}", pbo.path.display());
        }
        let tile_m = 1.0 / origin.aside;
        let step_frac = origin.pos_u - next.pos_u;
        if !(0.5..=1.0).contains(&step_frac) {
            bail!("tiles overlap unexpectedly (step {step_frac})");
        }
        let overlap_frac = (1.0 - step_frac) / 2.0;
        let layout = Self {
            overlap_frac,
            step_m: step_frac * tile_m,
            left: (overlap_frac - origin.pos_u) * tile_m,
            top: (overlap_frac - origin.pos_v) / origin.dir_v,
        };
        if ![layout.step_m, layout.left, layout.top]
            .iter()
            .all(|v| v.is_finite())
        {
            bail!("unexpected uvTransform in {}", pbo.path.display());
        }
        Ok(layout)
    }

    /// The north-east extent of the tile grid.
    pub fn coverage(&self, grid: u32) -> f64 {
        f64::from(grid) * self.step_m
    }
}

struct UvTransform {
    aside: f64,
    dir_v: f64,
    pos_u: f64,
    pos_v: f64,
}

fn world_uv_transform(pbo: &Pbo, tile_prefix: &str) -> Result<UvTransform> {
    let entry = pbo
        .entries
        .iter()
        .find(|e| {
            let lower = e.name.to_ascii_lowercase();
            lower.ends_with(".rvmat")
                && lower
                    .rsplit('\\')
                    .next()
                    .is_some_and(|f| f.starts_with(tile_prefix))
        })
        .with_context(|| format!("no {tile_prefix} material in {}", pbo.path.display()))?;
    let data = pbo.read(entry)?;
    let (aside, dir, pos) = if data.starts_with(b"\0raP") {
        binary_transform(&data)
    } else {
        text_transform(&String::from_utf8_lossy(&data))
    }
    .context("no worldPos uvTransform")?;
    if aside.is_empty() || dir.len() < 2 || pos.len() < 2 {
        bail!("incomplete uvTransform");
    }
    Ok(UvTransform {
        aside: aside[0],
        dir_v: dir[1],
        pos_u: pos[0],
        pos_v: pos[1],
    })
}

type Vectors = (Vec<f64>, Vec<f64>, Vec<f64>);

/// Rapified material: string entries are `01 00 name\0 value\0`, arrays are
/// `02 name\0 count (type value)*`. The first worldPos texGen is the satellite mapping.
fn binary_transform(data: &[u8]) -> Option<Vectors> {
    let start = find(data, b"uvSource\0worldPos\0", 0)?;
    Some((
        float_array(data, b"\x02aside\0", start)?,
        float_array(data, b"\x02dir\0", start)?,
        float_array(data, b"\x02pos\0", start)?,
    ))
}

/// Plain-text material: `uvSource="worldPos"; class uvTransform { aside[]={...}; ... }`.
fn text_transform(text: &str) -> Option<Vectors> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("\"worldpos\"")?;
    let array = |key: &str| -> Option<Vec<f64>> {
        let at = start + lower[start..].find(&format!("{key}[]"))?;
        let open = at + lower[at..].find('{')? + 1;
        let close = open + lower[open..].find('}')?;
        lower[open..close]
            .split(',')
            .map(|n| n.trim().parse().ok())
            .collect()
    };
    Some((array("aside")?, array("dir")?, array("pos")?))
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn float_array(data: &[u8], key: &[u8], from: usize) -> Option<Vec<f64>> {
    let mut i = find(data, key, from)? + key.len();
    let count = *data.get(i)? as usize;
    i += 1;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = *data.get(i)?;
        let raw: [u8; 4] = data.get(i + 1..i + 5)?.try_into().ok()?;
        values.push(match kind {
            1 => f64::from(f32::from_le_bytes(raw)),
            2 => f64::from(i32::from_le_bytes(raw)),
            _ => return None,
        });
        i += 5;
    }
    Some(values)
}
