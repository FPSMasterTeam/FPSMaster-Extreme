use std::collections::HashMap;

use crate::io::{PacketReader, ProtocolError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum NbtTag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List(Vec<NbtTag>),
    Compound(NbtCompound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

pub type NbtCompound = HashMap<String, NbtTag>;

impl NbtTag {
    pub fn as_compound(&self) -> Option<&NbtCompound> {
        match self {
            NbtTag::Compound(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[NbtTag]> {
        match self {
            NbtTag::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            NbtTag::String(s) => Some(s),
            _ => None,
        }
    }

    /// The payload of a TAG_Byte. Block-entity NBT stores its small enums this
    /// way (a skull's `SkullType` / `Rot`), which `as_i32` deliberately does not
    /// widen into.
    pub fn as_byte(&self) -> Option<i8> {
        match self {
            NbtTag::Byte(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_short(&self) -> Option<i16> {
        match self {
            NbtTag::Short(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_i32(&self) -> Option<i32> {
        match self {
            NbtTag::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            NbtTag::Double(v) => Some(*v),
            NbtTag::Float(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> bool {
        match self {
            NbtTag::Byte(v) => *v != 0,
            _ => false,
        }
    }
}

pub fn read_nbt_payload(body: &mut PacketReader<'_>, tag_type: u8) -> Result<NbtTag> {
    match tag_type {
        1 => Ok(NbtTag::Byte(body.read_i8()?)),
        2 => Ok(NbtTag::Short(body.read_i16()?)),
        3 => Ok(NbtTag::Int(body.read_i32()?)),
        4 => Ok(NbtTag::Long(body.read_i64()?)),
        5 => Ok(NbtTag::Float(body.read_f32()?)),
        6 => Ok(NbtTag::Double(body.read_f64()?)),
        7 => {
            let len = read_nbt_len(body)?;
            Ok(NbtTag::ByteArray(body.read_bytes(len)?.to_vec()))
        }
        8 => {
            let len = body.read_u16()? as usize;
            let bytes = body.read_bytes(len)?;
            let s = std::str::from_utf8(bytes)
                .map_err(|_| ProtocolError::InvalidData("invalid NBT string"))?;
            Ok(NbtTag::String(s.to_owned()))
        }
        9 => {
            let element_type = body.read_u8()?;
            let len = read_nbt_len(body)?;
            let mut list = Vec::with_capacity(len.min(1024));
            for _ in 0..len {
                list.push(read_nbt_payload(body, element_type)?);
            }
            Ok(NbtTag::List(list))
        }
        10 => {
            let mut map = HashMap::new();
            loop {
                let child_type = body.read_u8()?;
                if child_type == 0 {
                    return Ok(NbtTag::Compound(map));
                }
                let name_len = body.read_u16()? as usize;
                let name_bytes = body.read_bytes(name_len)?;
                let name = std::str::from_utf8(name_bytes)
                    .map_err(|_| ProtocolError::InvalidData("invalid NBT key"))?
                    .to_owned();
                let value = read_nbt_payload(body, child_type)?;
                map.insert(name, value);
            }
        }
        11 => {
            let len = read_nbt_len(body)?;
            let mut arr = Vec::with_capacity(len.min(1024));
            for _ in 0..len {
                arr.push(body.read_i32()?);
            }
            Ok(NbtTag::IntArray(arr))
        }
        12 => {
            let len = read_nbt_len(body)?;
            let mut arr = Vec::with_capacity(len.min(1024));
            for _ in 0..len {
                arr.push(body.read_i64()?);
            }
            Ok(NbtTag::LongArray(arr))
        }
        _ => Err(ProtocolError::InvalidData("unknown NBT tag type")),
    }
}

fn read_nbt_len(body: &mut PacketReader<'_>) -> Result<usize> {
    let len = body.read_i32()?;
    if len < 0 {
        return Err(ProtocolError::InvalidData("negative NBT length"));
    }
    Ok(len as usize)
}

/// Read the optional root NBT compound from a slot. Returns `None` if tag type is 0 (no NBT).
pub fn read_slot_nbt(body: &mut PacketReader<'_>) -> Result<Option<NbtCompound>> {
    let tag_type = body.read_u8()?;
    if tag_type == 0 {
        return Ok(None);
    }
    // Named root tag: skip the name.
    let name_len = body.read_u16()? as usize;
    body.read_bytes(name_len)?;
    match read_nbt_payload(body, tag_type)? {
        NbtTag::Compound(c) => Ok(Some(c)),
        _ => Err(ProtocolError::InvalidData("slot NBT root must be compound")),
    }
}
