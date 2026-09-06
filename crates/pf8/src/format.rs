//! PF6/PF8 format constants and low-level parsing functions.

//    PF6/PF8 structure
//    |magic 'pf6' or 'pf8'
//    |index_size 4 //start from index_count (faddr 0x7)
//    |index_count 4
//    |file_entrys[]
//      |name_length 4
//      |name //string with '\0'
//      |00 00 00 00
//      |offset 4
//      |size 4
//    |filesize_count 4
//    |filesize_offsets[] 8 //offset from faddr 0xf, last is 00 00 00 00 00 00 00 00
//    |filesize_count_offset 4 //offset from faddr 0x7

use encoding_rs::SHIFT_JIS;

use crate::error::{Error, Result};

/// PF6 magic number
pub const PF6_MAGIC: &[u8] = b"pf6";

/// PF2 magic number（更老的 Artemis 归档；按 PF6 同等布局处理，不加密）。
/// （art3m1s 本地改动：上游只认 pf6/pf8。）
pub const PF2_MAGIC: &[u8] = b"pf2";

/// PF8 magic number
pub const PF8_MAGIC: &[u8] = b"pf8";

/// Archive format type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Pf6,
    Pf8,
}

/// PF8 format header offsets
pub mod offsets {
    pub const MAGIC: usize = 0x00;
    pub const INDEX_SIZE: usize = 0x03;
    pub const INDEX_COUNT: usize = 0x07;
    pub const ENTRIES_START: usize = 0x0B;
    pub const INDEX_DATA_START: usize = 0x07;
    pub const FILESIZE_OFFSETS_START: usize = 0x0F;
}

/// Raw file entry as stored in PF8 format
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub name: String,
    pub offset: u32,
    pub size: u32,
}

/// Validates that the data starts with PF6 or PF8 magic number
pub fn validate_magic(data: &[u8]) -> Result<ArchiveFormat> {
    if data.len() < 3 {
        return Err(Error::InvalidFormat("Data too short".to_string()));
    }

    let magic = &data[offsets::MAGIC..offsets::MAGIC + 3];

    if magic == PF6_MAGIC || magic == PF2_MAGIC {
        Ok(ArchiveFormat::Pf6)
    } else if magic == PF8_MAGIC {
        Ok(ArchiveFormat::Pf8)
    } else {
        Err(Error::InvalidFormat("Not a PF6 or PF8 file".to_string()))
    }
}

/// Reads a u32 from the given offset in little-endian format
pub fn read_u32_le(data: &[u8], offset: usize) -> Result<u32> {
    if offset + 4 > data.len() {
        return Err(Error::InvalidFormat(
            "Not enough data to read u32".to_string(),
        ));
    }

    Ok(u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

/// Decodes a filename byte slice to a String.
/// Tries UTF-8 first; falls back to Shift-JIS (CP932) for legacy Japanese archives.
/// （art3m1s：reader 走带编码参数的变体，保留此函数作上游 API 对照。）
#[allow(dead_code)]
fn decode_filename(bytes: &[u8]) -> String {
    decode_filename_impl(bytes, None)
}

/// 显式指定编码时直接按它解码（art3m1s：宿主按游戏语言环境传入，例如 GBK）；
/// 缺省保持上游的 UTF-8 → Shift_JIS 自动回退。
fn decode_filename_impl(bytes: &[u8], encoding: Option<&'static encoding_rs::Encoding>) -> String {
    if let Some(encoding) = encoding {
        let (decoded, _, had_errors) = encoding.decode(bytes);
        if had_errors {
            eprintln!("Warning: filename contains bytes invalid for the requested encoding");
        }
        return decoded.into_owned();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    let (decoded, _, had_errors) = SHIFT_JIS.decode(bytes);
    if had_errors {
        // Neither UTF-8 nor valid Shift-JIS, contains replacement characters
        eprintln!("Warning: filename contains unrecognizable bytes, some characters may be corrupted");
    }
    decoded.into_owned()
}

/// Parses the PF6/PF8 header and returns file entries along with format information.
/// （art3m1s：reader 走带编码参数的变体，保留此函数作上游 API 对照。）
#[allow(dead_code)]
pub fn parse_entries(data: &[u8]) -> Result<(Vec<RawEntry>, ArchiveFormat)> {
    parse_entries_with_encoding(data, None)
}

/// 带显式条目名编码的解析（art3m1s 本地改动；None 时与上游行为一致）。
pub fn parse_entries_with_encoding(
    data: &[u8],
    encoding: Option<&'static encoding_rs::Encoding>,
) -> Result<(Vec<RawEntry>, ArchiveFormat)> {
    let format = validate_magic(data)?;

    if data.len() < 11 {
        return Err(Error::InvalidFormat(
            "Data too short to parse header".to_string(),
        ));
    }

    let index_size = read_u32_le(data, offsets::INDEX_SIZE)?;
    let index_count = read_u32_le(data, offsets::INDEX_COUNT)?;

    let mut file_entries = Vec::new();
    let mut cursor = offsets::ENTRIES_START;
    let index_end_pos = (offsets::INDEX_DATA_START + index_size as usize).min(data.len());

    while cursor < index_end_pos && file_entries.len() < index_count as usize {
        if cursor + 4 > data.len() {
            break;
        }

        let name_length = read_u32_le(data, cursor)?;
        cursor += 4;

        if cursor + name_length as usize + 12 > data.len() {
            break;
        }

        let name_bytes = &data[cursor..cursor + name_length as usize];
        let name = decode_filename_impl(name_bytes, encoding);
        cursor += name_length as usize + 4; // Skip name and 4 zero bytes

        let offset = read_u32_le(data, cursor)?;
        let size = read_u32_le(data, cursor + 4)?;
        cursor += 8;

        file_entries.push(RawEntry { name, offset, size });
    }

    if file_entries.len() != index_count as usize {
        return Err(Error::Corrupted(format!(
            "Index count mismatch. Expected {}, found {}",
            index_count,
            file_entries.len()
        )));
    }

    Ok((file_entries, format))
}

/// Gets the index size from PF6/PF8 header
pub fn get_index_size(data: &[u8]) -> Result<u32> {
    validate_magic(data)?;
    read_u32_le(data, offsets::INDEX_SIZE)
}
