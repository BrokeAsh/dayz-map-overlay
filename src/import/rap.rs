//! Parser for rapified (binarized) configs such as a terrain's `config.bin`.

use anyhow::{Result, bail};

#[derive(Debug, Clone)]
pub enum Value {
    Str(String),
    Float(f32),
    Int(i32),
    Array(Vec<Value>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f32),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Class {
    pub entries: Vec<(String, Entry)>,
}

#[derive(Debug, Clone)]
pub enum Entry {
    Class(Class),
    Value(Value),
}

impl Class {
    /// Case-insensitive lookup, as in the game.
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, e)| e)
    }

    pub fn class(&self, name: &str) -> Option<&Class> {
        match self.get(name)? {
            Entry::Class(c) => Some(c),
            Entry::Value(_) => None,
        }
    }

    pub fn value(&self, name: &str) -> Option<&Value> {
        match self.get(name)? {
            Entry::Value(v) => Some(v),
            Entry::Class(_) => None,
        }
    }

    pub fn classes(&self) -> impl Iterator<Item = (&str, &Class)> {
        self.entries.iter().filter_map(|(n, e)| match e {
            Entry::Class(c) => Some((n.as_str(), c)),
            Entry::Value(_) => None,
        })
    }
}

pub fn parse(data: &[u8]) -> Result<Class> {
    if !data.starts_with(b"\0raP") {
        bail!("not a rapified config");
    }
    let mut reader = Reader { data, depth: 0 };
    reader.class_body(16)
}

struct Reader<'a> {
    data: &'a [u8],
    depth: usize,
}

impl Reader<'_> {
    fn byte(&self, i: &mut usize) -> Result<u8> {
        let b = *self
            .data
            .get(*i)
            .ok_or_else(|| anyhow::anyhow!("config is truncated"))?;
        *i += 1;
        Ok(b)
    }

    fn u32(&self, i: &mut usize) -> Result<u32> {
        let bytes = self
            .data
            .get(*i..*i + 4)
            .ok_or_else(|| anyhow::anyhow!("config is truncated"))?;
        *i += 4;
        Ok(u32::from_le_bytes(bytes.try_into()?))
    }

    fn compressed(&self, i: &mut usize) -> Result<usize> {
        let mut value = 0usize;
        for shift in (0..35).step_by(7) {
            let b = self.byte(i)?;
            value |= ((b & 0x7f) as usize) << shift;
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        bail!("bad compressed integer in config")
    }

    fn string(&self, i: &mut usize) -> Result<String> {
        let rest = self.data.get(*i..).unwrap_or_default();
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| anyhow::anyhow!("unterminated string"))?;
        *i += len + 1;
        Ok(String::from_utf8_lossy(&rest[..len]).into_owned())
    }

    fn scalar(&self, kind: u8, i: &mut usize) -> Result<Value> {
        Ok(match kind {
            0 | 4 => Value::Str(self.string(i)?),
            1 => Value::Float(f32::from_bits(self.u32(i)?)),
            2 => Value::Int(self.u32(i)? as i32),
            _ => bail!("unknown config value type {kind}"),
        })
    }

    fn array(&self, i: &mut usize) -> Result<Vec<Value>> {
        let count = self.compressed(i)?;
        let mut items = Vec::with_capacity(count.min(4096));
        for _ in 0..count {
            let kind = self.byte(i)?;
            items.push(if kind == 3 {
                Value::Array(self.array(i)?)
            } else {
                self.scalar(kind, i)?
            });
        }
        Ok(items)
    }

    fn class_body(&mut self, offset: usize) -> Result<Class> {
        self.depth += 1;
        if self.depth > 64 {
            bail!("config nesting is too deep");
        }
        let mut i = offset;
        let _parent = self.string(&mut i)?;
        let count = self.compressed(&mut i)?;
        let mut class = Class::default();
        for _ in 0..count {
            match self.byte(&mut i)? {
                0 => {
                    let name = self.string(&mut i)?;
                    let body = self.u32(&mut i)? as usize;
                    let child = self.class_body(body)?;
                    class.entries.push((name, Entry::Class(child)));
                }
                1 => {
                    let kind = self.byte(&mut i)?;
                    let name = self.string(&mut i)?;
                    let value = self.scalar(kind, &mut i)?;
                    class.entries.push((name, Entry::Value(value)));
                }
                2 => {
                    let name = self.string(&mut i)?;
                    let items = self.array(&mut i)?;
                    class
                        .entries
                        .push((name, Entry::Value(Value::Array(items))));
                }
                3 | 4 => {
                    // External class reference or delete: nothing to keep.
                    self.string(&mut i)?;
                }
                5 => {
                    // `name[] += {...}`
                    self.u32(&mut i)?;
                    let name = self.string(&mut i)?;
                    let items = self.array(&mut i)?;
                    class
                        .entries
                        .push((name, Entry::Value(Value::Array(items))));
                }
                other => bail!("unknown config entry type {other}"),
            }
        }
        self.depth -= 1;
        Ok(class)
    }
}
