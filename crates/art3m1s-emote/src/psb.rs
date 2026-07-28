use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::{EmoteError, Result};

const PSB_MAGIC: &[u8; 4] = b"PSB\0";
const KEY1: u32 = 123_456_789;
const KEY2: u32 = 362_436_069;
const KEY3: u32 = 521_288_629;
const MAX_COLLECTION_ITEMS: usize = 4_000_000;
const MAX_RECURSION_DEPTH: usize = 512;

#[derive(Clone, Debug, PartialEq)]
pub enum PsbValue {
    None,
    Null,
    Bool(bool),
    Integer(i64),
    Float(f32),
    Double(f64),
    Array(Vec<u32>),
    String(String),
    Resource(ResourceRef),
    List(Vec<PsbValue>),
    Object(BTreeMap<String, PsbValue>),
}

impl PsbValue {
    pub fn as_object(&self) -> Option<&BTreeMap<String, PsbValue>> {
        match self {
            Self::Object(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[PsbValue]> {
        match self {
            Self::List(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&PsbValue> {
        self.as_object()?.get(key)
    }

    pub fn at_path<'a>(&'a self, path: &[&str]) -> Option<&'a PsbValue> {
        path.iter().try_fold(self, |value, key| value.get(key))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceRef {
    pub index: u32,
    pub extra: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PsbHeader {
    pub version: u16,
    pub encryption_flags: u16,
    pub header_length: u32,
    pub offset_names: u32,
    pub offset_strings: u32,
    pub offset_strings_data: u32,
    pub offset_chunk_offsets: u32,
    pub offset_chunk_lengths: u32,
    pub offset_chunk_data: u32,
    pub offset_entries: u32,
    pub checksum: Option<u32>,
    pub offset_extra_chunk_offsets: Option<u32>,
    pub offset_extra_chunk_lengths: Option<u32>,
    pub offset_extra_chunk_data: Option<u32>,
    pub header_key: Option<u32>,
}

impl PsbHeader {
    fn encoded_length(version: u16) -> Result<usize> {
        match version {
            1 | 2 => Ok(40),
            3 => Ok(44),
            4 => Ok(56),
            other => Err(EmoteError::Unsupported(format!("PSB version {other}"))),
        }
    }
}

#[derive(Debug)]
pub struct PsbDocument {
    data: Vec<u8>,
    pub header: PsbHeader,
    pub names: Vec<String>,
    pub strings: Vec<String>,
    pub root: PsbValue,
    chunk_offsets: Vec<u32>,
    chunk_lengths: Vec<u32>,
    extra_chunk_offsets: Vec<u32>,
    extra_chunk_lengths: Vec<u32>,
}

impl PsbDocument {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_bytes(fs::read(path)?)
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        let header = parse_header(&data)?;
        validate_body_start(&data, &header)?;

        let (_, string_offsets, _) = parse_array(&data, header.offset_strings as usize)?;
        let (_, charset, names_cursor) = parse_array(&data, header.offset_names as usize)?;
        let (_, names_data, names_cursor) = parse_array(&data, names_cursor)?;
        let (_, name_indexes, _) = parse_array(&data, names_cursor)?;

        let names = decode_names(&charset, &names_data, &name_indexes)?;
        let strings = decode_strings(&data, header.offset_strings_data as usize, &string_offsets)?;

        let (_, chunk_offsets, _) = parse_array(&data, header.offset_chunk_offsets as usize)?;
        let (_, chunk_lengths, _) = parse_array(&data, header.offset_chunk_lengths as usize)?;
        if chunk_offsets.len() != chunk_lengths.len() {
            return Err(EmoteError::InvalidFormat(format!(
                "chunk table length mismatch: {} offsets, {} lengths",
                chunk_offsets.len(),
                chunk_lengths.len()
            )));
        }

        let (extra_chunk_offsets, extra_chunk_lengths) = match (
            header.offset_extra_chunk_offsets,
            header.offset_extra_chunk_lengths,
        ) {
            (Some(offsets), Some(lengths)) if offsets != 0 && lengths != 0 => {
                let (_, offsets, _) = parse_array(&data, offsets as usize)?;
                let (_, lengths, _) = parse_array(&data, lengths as usize)?;
                if offsets.len() != lengths.len() {
                    return Err(EmoteError::InvalidFormat(format!(
                        "extra chunk table length mismatch: {} offsets, {} lengths",
                        offsets.len(),
                        lengths.len()
                    )));
                }
                (offsets, lengths)
            }
            _ => (Vec::new(), Vec::new()),
        };

        let parser = ValueParser {
            data: &data,
            names: &names,
            strings: &strings,
        };
        let root = parser.parse_value(header.offset_entries as usize, 0)?;

        Ok(Self {
            data,
            header,
            names,
            strings,
            root,
            chunk_offsets,
            chunk_lengths,
            extra_chunk_offsets,
            extra_chunk_lengths,
        })
    }

    pub fn resource(&self, reference: ResourceRef) -> Result<&[u8]> {
        let (base, offsets, lengths) = if reference.extra {
            (
                self.header.offset_extra_chunk_data.ok_or_else(|| {
                    EmoteError::InvalidFormat("missing extra chunk data offset".into())
                })? as usize,
                &self.extra_chunk_offsets,
                &self.extra_chunk_lengths,
            )
        } else {
            (
                self.header.offset_chunk_data as usize,
                &self.chunk_offsets,
                &self.chunk_lengths,
            )
        };

        let index = reference.index as usize;
        let offset = *offsets.get(index).ok_or_else(|| {
            EmoteError::InvalidFormat(format!("resource index {index} is out of bounds"))
        })? as usize;
        let length = *lengths.get(index).ok_or_else(|| {
            EmoteError::InvalidFormat(format!("resource length index {index} is out of bounds"))
        })? as usize;
        checked_slice(&self.data, base + offset, length)
    }

    pub fn resource_count(&self) -> usize {
        self.chunk_offsets.len()
    }

    pub fn extra_resource_count(&self) -> usize {
        self.extra_chunk_offsets.len()
    }
}

fn parse_header(data: &[u8]) -> Result<PsbHeader> {
    if data.len() < 8 || data.get(0..4) != Some(PSB_MAGIC) {
        return Err(EmoteError::InvalidFormat("missing PSB signature".into()));
    }

    let version = read_u16(data, 4)?;
    let encryption_flags = read_u16(data, 6)?;
    let encoded_length = PsbHeader::encoded_length(version)?;
    if data.len() < encoded_length {
        return Err(EmoteError::InvalidFormat(format!(
            "PSB header is truncated: need {encoded_length}, got {}",
            data.len()
        )));
    }

    let mut decoded = data[..encoded_length].to_vec();
    let header_key = if encryption_flags & 1 != 0 {
        let key = infer_header_key(&decoded, version)?;
        crypt_header(&mut decoded[8..], key);
        Some(key)
    } else {
        None
    };

    let header = PsbHeader {
        version,
        encryption_flags,
        header_length: read_u32(&decoded, 8)?,
        offset_names: read_u32(&decoded, 12)?,
        offset_strings: read_u32(&decoded, 16)?,
        offset_strings_data: read_u32(&decoded, 20)?,
        offset_chunk_offsets: read_u32(&decoded, 24)?,
        offset_chunk_lengths: read_u32(&decoded, 28)?,
        offset_chunk_data: read_u32(&decoded, 32)?,
        offset_entries: read_u32(&decoded, 36)?,
        checksum: (version >= 3).then(|| read_u32(&decoded, 40)).transpose()?,
        offset_extra_chunk_offsets: (version >= 4).then(|| read_u32(&decoded, 44)).transpose()?,
        offset_extra_chunk_lengths: (version >= 4).then(|| read_u32(&decoded, 48)).transpose()?,
        offset_extra_chunk_data: (version >= 4).then(|| read_u32(&decoded, 52)).transpose()?,
        header_key,
    };

    validate_header(&header, data.len(), &decoded)?;
    Ok(header)
}

fn infer_header_key(header: &[u8], version: u16) -> Result<u32> {
    let expected_length = PsbHeader::encoded_length(version)? as u32;
    let encrypted = read_u32(header, 8)?;
    let first_stream_word = encrypted ^ expected_length;

    let a = KEY1 ^ KEY1.wrapping_shl(11);
    let rhs = first_stream_word ^ a ^ (a >> 8);
    Ok(rhs ^ (rhs >> 19))
}

fn crypt_header(data: &mut [u8], key4: u32) {
    let mut cipher = PsbCipher::new(key4);
    cipher.apply(data);
}

fn validate_header(header: &PsbHeader, file_len: usize, decoded: &[u8]) -> Result<()> {
    let expected_length = PsbHeader::encoded_length(header.version)? as u32;
    if header.header_length != 0 && header.header_length != expected_length {
        return Err(EmoteError::InvalidFormat(format!(
            "unexpected header length {} for PSB v{}",
            header.header_length, header.version
        )));
    }

    for (name, offset) in [
        ("names", header.offset_names),
        ("strings", header.offset_strings),
        ("strings data", header.offset_strings_data),
        ("chunk offsets", header.offset_chunk_offsets),
        ("chunk lengths", header.offset_chunk_lengths),
        ("chunk data", header.offset_chunk_data),
        ("entries", header.offset_entries),
    ] {
        if offset as usize >= file_len {
            return Err(EmoteError::InvalidFormat(format!(
                "{name} offset {offset} exceeds file length {file_len}"
            )));
        }
    }

    if let Some(expected) = header.checksum {
        let mut checksum_data = decoded
            .get(8..40)
            .ok_or_else(|| EmoteError::InvalidFormat("truncated checksum fields".into()))?
            .to_vec();
        if header.version >= 4 {
            checksum_data.extend_from_slice(decoded.get(44..56).ok_or_else(|| {
                EmoteError::InvalidFormat("truncated PSB v4 checksum fields".into())
            })?);
        }
        let actual = adler32(&checksum_data);
        if expected != actual {
            return Err(EmoteError::InvalidFormat(format!(
                "header checksum mismatch: expected {expected:#010x}, got {actual:#010x}"
            )));
        }
    }
    Ok(())
}

fn validate_body_start(data: &[u8], header: &PsbHeader) -> Result<()> {
    let body_type = *data
        .get(header.offset_names as usize)
        .ok_or_else(|| EmoteError::InvalidFormat("names section starts outside the file".into()))?;
    if !(0x0d..=0x14).contains(&body_type) {
        return Err(EmoteError::Unsupported(
            "encrypted PSB bodies are not implemented yet".into(),
        ));
    }
    Ok(())
}

fn decode_names(str1: &[u32], str2: &[u32], str3: &[u32]) -> Result<Vec<String>> {
    let mut names = Vec::with_capacity(str3.len());
    for &index in str3 {
        let mut cursor = *str2.get(index as usize).ok_or_else(|| {
            EmoteError::InvalidFormat(format!("name table index {index} is out of bounds"))
        })? as usize;
        let mut bytes = Vec::new();
        let mut remaining = str2.len().saturating_add(1);
        loop {
            if remaining == 0 {
                return Err(EmoteError::InvalidFormat(
                    "cycle detected in PSB name trie".into(),
                ));
            }
            remaining -= 1;

            let parent = *str2.get(cursor).ok_or_else(|| {
                EmoteError::InvalidFormat(format!("name trie node {cursor} is out of bounds"))
            })?;
            let delta = *str1.get(parent as usize).ok_or_else(|| {
                EmoteError::InvalidFormat(format!("name charset index {parent} is out of bounds"))
            })?;
            let byte = (cursor as u32)
                .checked_sub(delta)
                .ok_or_else(|| EmoteError::InvalidFormat("negative name trie byte".into()))?;
            if byte > u8::MAX as u32 {
                return Err(EmoteError::InvalidFormat(format!(
                    "name trie byte {byte} exceeds u8"
                )));
            }
            bytes.push(byte as u8);
            cursor = parent as usize;
            if cursor == 0 {
                break;
            }
        }
        bytes.reverse();
        names.push(String::from_utf8(bytes).map_err(|error| {
            EmoteError::InvalidFormat(format!("invalid UTF-8 in PSB name: {error}"))
        })?);
    }
    Ok(names)
}

fn decode_strings(data: &[u8], base: usize, offsets: &[u32]) -> Result<Vec<String>> {
    offsets
        .iter()
        .map(|offset| read_c_string(data, base + *offset as usize))
        .collect()
}

fn read_c_string(data: &[u8], start: usize) -> Result<String> {
    let tail = data.get(start..).ok_or_else(|| {
        EmoteError::InvalidFormat(format!("string offset {start} is out of bounds"))
    })?;
    let length = tail.iter().position(|byte| *byte == 0).ok_or_else(|| {
        EmoteError::InvalidFormat(format!("unterminated string at offset {start}"))
    })?;
    String::from_utf8(tail[..length].to_vec())
        .map_err(|error| EmoteError::InvalidFormat(format!("invalid UTF-8 string: {error}")))
}

struct ValueParser<'a> {
    data: &'a [u8],
    names: &'a [String],
    strings: &'a [String],
}

impl ValueParser<'_> {
    fn parse_value(&self, offset: usize, depth: usize) -> Result<PsbValue> {
        if depth > MAX_RECURSION_DEPTH {
            return Err(EmoteError::InvalidFormat(
                "PSB object recursion limit exceeded".into(),
            ));
        }

        let kind = *self.data.get(offset).ok_or_else(|| {
            EmoteError::InvalidFormat(format!("object offset {offset} is out of bounds"))
        })?;
        match kind {
            0x00 => Ok(PsbValue::None),
            0x01 => Ok(PsbValue::Null),
            0x02 => Ok(PsbValue::Bool(false)),
            0x03 => Ok(PsbValue::Bool(true)),
            0x04 => Ok(PsbValue::Integer(0)),
            0x05..=0x0c => {
                let width = (kind - 0x04) as usize;
                let raw = read_compact_u64(self.data, offset + 1, width)?;
                Ok(PsbValue::Integer(sign_extend(raw, width)))
            }
            0x0d..=0x14 => {
                let (_, values, _) = parse_array(self.data, offset)?;
                Ok(PsbValue::Array(values))
            }
            0x15..=0x18 => {
                let width = (kind - 0x14) as usize;
                let index = read_compact_u64(self.data, offset + 1, width)? as usize;
                let value = self.strings.get(index).ok_or_else(|| {
                    EmoteError::InvalidFormat(format!("string index {index} is out of bounds"))
                })?;
                Ok(PsbValue::String(value.clone()))
            }
            0x19..=0x1c => {
                let width = (kind - 0x18) as usize;
                Ok(PsbValue::Resource(ResourceRef {
                    index: read_compact_u64(self.data, offset + 1, width)? as u32,
                    extra: false,
                }))
            }
            0x1d => Ok(PsbValue::Float(0.0)),
            0x1e => Ok(PsbValue::Float(f32::from_le_bytes(
                checked_slice(self.data, offset + 1, 4)?
                    .try_into()
                    .expect("checked length"),
            ))),
            0x1f => Ok(PsbValue::Double(f64::from_le_bytes(
                checked_slice(self.data, offset + 1, 8)?
                    .try_into()
                    .expect("checked length"),
            ))),
            0x20 => self.parse_list(offset + 1, depth + 1),
            0x21 => self.parse_object(offset + 1, depth + 1),
            0x22..=0x25 => {
                let width = (kind - 0x21) as usize;
                Ok(PsbValue::Resource(ResourceRef {
                    index: read_compact_u64(self.data, offset + 1, width)? as u32,
                    extra: true,
                }))
            }
            other => Err(EmoteError::Unsupported(format!(
                "PSB object type {other:#04x}"
            ))),
        }
    }

    fn parse_list(&self, offset: usize, depth: usize) -> Result<PsbValue> {
        let (_, offsets, base) = parse_array(self.data, offset)?;
        let mut result = Vec::with_capacity(offsets.len());
        for relative in offsets {
            result.push(self.parse_value(base + relative as usize, depth)?);
        }
        Ok(PsbValue::List(result))
    }

    fn parse_object(&self, offset: usize, depth: usize) -> Result<PsbValue> {
        let (_, names, cursor) = parse_array(self.data, offset)?;
        let (_, offsets, base) = parse_array(self.data, cursor)?;
        if names.len() != offsets.len() {
            return Err(EmoteError::InvalidFormat(format!(
                "object table length mismatch: {} names, {} offsets",
                names.len(),
                offsets.len()
            )));
        }

        let mut result = BTreeMap::new();
        for (name_index, relative) in names.into_iter().zip(offsets) {
            let name = self.names.get(name_index as usize).ok_or_else(|| {
                EmoteError::InvalidFormat(format!("name index {name_index} is out of bounds"))
            })?;
            result.insert(
                name.clone(),
                self.parse_value(base + relative as usize, depth)?,
            );
        }
        Ok(PsbValue::Object(result))
    }
}

fn parse_array(data: &[u8], offset: usize) -> Result<(u8, Vec<u32>, usize)> {
    let kind = *data.get(offset).ok_or_else(|| {
        EmoteError::InvalidFormat(format!("array offset {offset} is out of bounds"))
    })?;
    if !(0x0d..=0x14).contains(&kind) {
        return Err(EmoteError::InvalidFormat(format!(
            "expected PSB array at {offset}, found {kind:#04x}"
        )));
    }
    let count_width = (kind - 0x0c) as usize;
    let count = read_compact_u64(data, offset + 1, count_width)? as usize;
    if count > MAX_COLLECTION_ITEMS {
        return Err(EmoteError::InvalidFormat(format!(
            "array item count {count} exceeds safety limit"
        )));
    }

    let width_offset = offset + 1 + count_width;
    let width_kind = *data
        .get(width_offset)
        .ok_or_else(|| EmoteError::InvalidFormat("truncated PSB array entry width".into()))?;
    if !(0x0c..=0x14).contains(&width_kind) {
        return Err(EmoteError::InvalidFormat(format!(
            "invalid PSB array entry width marker {width_kind:#04x}"
        )));
    }
    let entry_width = (width_kind - 0x0c) as usize;
    if count != 0 && entry_width == 0 {
        return Err(EmoteError::InvalidFormat(
            "non-empty PSB array uses zero-width entries".into(),
        ));
    }

    let entries_offset = width_offset + 1;
    let byte_length = count
        .checked_mul(entry_width)
        .ok_or_else(|| EmoteError::InvalidFormat("PSB array size overflow".into()))?;
    checked_slice(data, entries_offset, byte_length)?;

    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        result.push(
            read_compact_u64(data, entries_offset + index * entry_width, entry_width)? as u32,
        );
    }
    Ok((entry_width as u8, result, entries_offset + byte_length))
}

fn checked_slice(data: &[u8], offset: usize, length: usize) -> Result<&[u8]> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| EmoteError::InvalidFormat("byte range overflow".into()))?;
    data.get(offset..end).ok_or_else(|| {
        EmoteError::InvalidFormat(format!(
            "byte range {offset}..{end} exceeds file length {}",
            data.len()
        ))
    })
}

fn read_compact_u64(data: &[u8], offset: usize, width: usize) -> Result<u64> {
    if width > 8 {
        return Err(EmoteError::InvalidFormat(format!(
            "compact integer width {width} exceeds 8"
        )));
    }
    let mut bytes = [0u8; 8];
    bytes[..width].copy_from_slice(checked_slice(data, offset, width)?);
    Ok(u64::from_le_bytes(bytes))
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        checked_slice(data, offset, 2)?
            .try_into()
            .expect("checked length"),
    ))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        checked_slice(data, offset, 4)?
            .try_into()
            .expect("checked length"),
    ))
}

fn adler32(data: &[u8]) -> u32 {
    const MOD_ADLER: u32 = 65_521;
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + *byte as u32) % MOD_ADLER;
        b = (b + a) % MOD_ADLER;
    }
    (b << 16) | a
}

struct PsbCipher {
    key1: u32,
    key2: u32,
    key3: u32,
    key4: u32,
    current: u32,
}

impl PsbCipher {
    fn new(key4: u32) -> Self {
        Self {
            key1: KEY1,
            key2: KEY2,
            key3: KEY3,
            key4,
            current: 0,
        }
    }

    fn apply(&mut self, data: &mut [u8]) {
        for byte in data {
            if self.current == 0 {
                let a = self.key1 ^ self.key1.wrapping_shl(11);
                let b = self.key4;
                let next = a ^ b ^ ((a ^ (b >> 11)) >> 8);
                self.key1 = self.key2;
                self.key2 = self.key3;
                self.key3 = b;
                self.key4 = next;
                self.current = next;
            }
            *byte ^= self.current as u8;
            self.current >>= 8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_stream_key_from_canonical_header_length() {
        let key = 439_510_497;
        let mut header = vec![0u8; 56];
        header[..4].copy_from_slice(PSB_MAGIC);
        header[4..6].copy_from_slice(&4u16.to_le_bytes());
        header[6..8].copy_from_slice(&1u16.to_le_bytes());
        header[8..12].copy_from_slice(&56u32.to_le_bytes());
        crypt_header(&mut header[8..], key);

        assert_eq!(infer_header_key(&header, 4).unwrap(), key);
    }

    #[test]
    fn parses_compact_array() {
        let bytes = [0x0d, 0x03, 0x0d, 0x02, 0x07, 0xff];
        let (_, values, end) = parse_array(&bytes, 0).unwrap();
        assert_eq!(values, [2, 7, 255]);
        assert_eq!(end, bytes.len());
    }

    #[test]
    fn decodes_names_from_three_psb_tables() {
        // A compact trie for the two keys "a" and "ab".
        let mut str1 = vec![0; 98];
        str1[97] = 1;
        let mut str2 = vec![0; 100];
        str2[1] = 97;
        str2[2] = 99;
        str2[97] = 0;
        str2[99] = 97;
        let str3 = [1, 2];

        assert_eq!(decode_names(&str1, &str2, &str3).unwrap(), ["a", "ab"]);
    }
}

/// PSB 的整数按最小字节宽度存储且为有符号数：任何宽度都必须按其宽度
/// 符号扩展（0xFF 单字节 = -1，而不是 255）。
fn sign_extend(raw: u64, width: usize) -> i64 {
    if width >= 8 {
        raw as i64
    } else {
        let shift = 64 - width * 8;
        ((raw << shift) as i64) >> shift
    }
}

#[cfg(test)]
mod sign_extend_tests {
    use super::sign_extend;

    #[test]
    fn extends_by_storage_width() {
        assert_eq!(sign_extend(0xFF, 1), -1);
        assert_eq!(sign_extend(0x7F, 1), 127);
        assert_eq!(sign_extend(226, 1), -30);
        assert_eq!(sign_extend(0xFFFF, 2), -1);
        assert_eq!(sign_extend(64_736, 2), -800);
        assert_eq!(sign_extend(32_767, 2), 32_767);
        assert_eq!(sign_extend(0xFF_FFFF, 3), -1);
        assert_eq!(sign_extend(0x7F_FFFF, 3), 0x7F_FFFF);
        assert_eq!(sign_extend(0xFFFF_FFFF, 4), -1);
        assert_eq!(sign_extend(u64::MAX, 8), -1);
    }
}
