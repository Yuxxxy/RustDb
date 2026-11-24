//! F-80 单元格详情：显示模式、JSON 路径定位、二进制/时间元数据。
//!
//! 纯函数区，不碰 UI；格式化只影响显示（修改值仍走变更集原文）。

use chrono::Local;
use rustdb_db_api::{hex_dump, Value};

/// 详情正文的显示方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailMode {
    Value,
    JsonPretty,
    JsonCompact,
    Hex,
    Text,
}

impl DetailMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Value => "值",
            Self::JsonPretty => "格式化",
            Self::JsonCompact => "压缩",
            Self::Hex => "十六进制",
            Self::Text => "文本",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::JsonPretty => "pretty",
            Self::JsonCompact => "compact",
            Self::Hex => "hex",
            Self::Text => "text",
        }
    }
}

const JSON_MODES: [DetailMode; 2] = [DetailMode::JsonPretty, DetailMode::JsonCompact];
const BINARY_MODES: [DetailMode; 2] = [DetailMode::Hex, DetailMode::Text];

/// 该值可切换的显示模式；只有一个模式时不渲染切换按钮。
pub fn modes(value: &Value) -> &'static [DetailMode] {
    match value {
        Value::Json(_) => &JSON_MODES,
        Value::Bytes(_) | Value::Omitted { .. } => &BINARY_MODES,
        _ => &[],
    }
}

pub fn default_mode(value: &Value) -> DetailMode {
    modes(value).first().copied().unwrap_or(DetailMode::Value)
}

/// 正文文本；JSON 路径无效时返回可读错误而不是空白。
pub fn render_body(value: &Value, mode: DetailMode, pointer: &str) -> String {
    match mode {
        DetailMode::Value => value.detail_text(),
        DetailMode::JsonPretty | DetailMode::JsonCompact => {
            let Value::Json(raw) = value else {
                return value.detail_text();
            };
            match locate(raw, pointer) {
                Ok(json) if mode == DetailMode::JsonPretty => {
                    serde_json::to_string_pretty(&json).unwrap_or_else(|_| json.to_string())
                }
                Ok(json) => json.to_string(),
                Err(message) => message,
            }
        }
        DetailMode::Hex => match bytes_of(value) {
            Some(bytes) => hex_dump(bytes, 8192),
            None => value.detail_text(),
        },
        DetailMode::Text => match bytes_of(value) {
            Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            None => value.detail_text(),
        },
    }
}

/// JSON Pointer（`/a/b/0`）；不带 `/` 时按 `a.b.0` 的路径写法补全。
pub fn locate(raw: &str, pointer: &str) -> Result<serde_json::Value, String> {
    let doc: serde_json::Value =
        serde_json::from_str(raw).map_err(|err| format!("JSON 解析失败：{err}"))?;
    let trimmed = pointer.trim();
    if trimmed.is_empty() {
        return Ok(doc);
    }
    let pointer = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        trimmed
            .split('.')
            .map(|part| format!("/{part}"))
            .collect::<String>()
    };
    doc.pointer(&pointer)
        .cloned()
        .ok_or_else(|| format!("路径不存在：{trimmed}"))
}

/// 详情里的附加说明（时区、图片/几何元数据、截断提示）。
pub fn notes(value: &Value) -> Vec<String> {
    match value {
        Value::Json(_) => vec!["格式化只影响显示，修改仍以原文进入待提交集".into()],
        Value::TimestampTz(dt) => vec![
            format!("本地 {}", dt.with_timezone(&Local).to_rfc3339()),
            format!("UTC  {}", dt.to_rfc3339()),
        ],
        Value::Timestamp(_) => vec!["无时区信息，按数据库本地时间显示".into()],
        Value::Date(date) => vec![format!("日期（无时区）：{date}")],
        Value::Time(time) => vec![format!("时间（无日期/时区）：{time}")],
        Value::Bytes(bytes) => binary_notes(bytes),
        Value::Omitted {
            total_bytes,
            preview,
        } => {
            let mut notes = vec![format!(
                "大字段仅加载前 {} 字节，共 {total_bytes} 字节",
                preview.len()
            )];
            notes.extend(binary_notes(preview));
            notes
        }
        _ => Vec::new(),
    }
}

fn binary_notes(bytes: &[u8]) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(info) = image_info(bytes) {
        notes.push(format!(
            "图片 {} {}×{}（仅元数据，未渲染）",
            info.format, info.width, info.height
        ));
    }
    if let Some(info) = geometry_info(bytes) {
        let srid = info
            .srid
            .map(|srid| format!(" · SRID {srid}"))
            .unwrap_or_default();
        notes.push(format!("几何 {}{srid}", info.kind));
    }
    notes
}

fn bytes_of(value: &Value) -> Option<&[u8]> {
    match value {
        Value::Bytes(bytes) => Some(bytes),
        Value::Omitted { preview, .. } => Some(preview),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub format: &'static str,
    pub width: u32,
    pub height: u32,
}

/// 按文件头识别常见图片并读出尺寸；不解码像素。
pub fn image_info(bytes: &[u8]) -> Option<ImageInfo> {
    if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(ImageInfo {
            format: "PNG",
            width: be32(bytes, 16)?,
            height: be32(bytes, 20)?,
        });
    }
    if bytes.len() >= 10 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Some(ImageInfo {
            format: "GIF",
            width: le16(bytes, 6)? as u32,
            height: le16(bytes, 8)? as u32,
        });
    }
    if bytes.len() >= 26 && bytes.starts_with(b"BM") {
        let width = le32(bytes, 18)? as i32;
        let height = le32(bytes, 22)? as i32;
        return Some(ImageInfo {
            format: "BMP",
            width: width.unsigned_abs(),
            height: height.unsigned_abs(),
        });
    }
    if let Some((width, height)) = jpeg_size(bytes) {
        return Some(ImageInfo {
            format: "JPEG",
            width,
            height,
        });
    }
    if bytes.len() >= 30 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return webp_size(bytes).map(|(width, height)| ImageInfo {
            format: "WebP",
            width,
            height,
        });
    }
    None
}

fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut index = 2;
    while index + 9 < bytes.len() {
        if bytes[index] != 0xFF {
            index += 1;
            continue;
        }
        let marker = bytes[index + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            index += 2;
            continue;
        }
        let length = be16(bytes, index + 2)? as usize;
        if length < 2 {
            return None;
        }
        if matches!(
            marker,
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
        ) {
            let height = be16(bytes, index + 5)? as u32;
            let width = be16(bytes, index + 7)? as u32;
            return Some((width, height));
        }
        index += 2 + length;
    }
    None
}

fn webp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let chunk = &bytes[12..16];
    if chunk == b"VP8X" {
        let width = le24(bytes, 24)? + 1;
        let height = le24(bytes, 27)? + 1;
        return Some((width, height));
    }
    if chunk == b"VP8 " && bytes.len() >= 30 {
        return Some((
            (le16(bytes, 26)? & 0x3FFF) as u32,
            (le16(bytes, 28)? & 0x3FFF) as u32,
        ));
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeometryInfo {
    pub kind: &'static str,
    pub srid: Option<u32>,
}

/// 识别 PostGIS EWKB（带 SRID 标记）；普通 WKB 不猜，避免把二进制误标成几何。
pub fn geometry_info(bytes: &[u8]) -> Option<GeometryInfo> {
    if bytes.len() < 9 {
        return None;
    }
    let little = match bytes[0] {
        0 => false,
        1 => true,
        _ => return None,
    };
    let raw_type = read_u32(bytes, 1, little)?;
    if raw_type & 0x2000_0000 == 0 {
        return None;
    }
    let kind = match raw_type & 0x0FFF_FFFF {
        1 => "Point",
        2 => "LineString",
        3 => "Polygon",
        4 => "MultiPoint",
        5 => "MultiLineString",
        6 => "MultiPolygon",
        7 => "GeometryCollection",
        _ => "EWKB",
    };
    let srid = read_u32(bytes, 5, little)?;
    Some(GeometryInfo {
        kind,
        srid: Some(srid),
    })
}

fn read_u32(bytes: &[u8], at: usize, little: bool) -> Option<u32> {
    let raw = bytes.get(at..at + 4)?;
    let array = [raw[0], raw[1], raw[2], raw[3]];
    Some(if little {
        u32::from_le_bytes(array)
    } else {
        u32::from_be_bytes(array)
    })
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    let raw = bytes.get(at..at + 2)?;
    Some(u16::from_be_bytes([raw[0], raw[1]]))
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    let raw = bytes.get(at..at + 2)?;
    Some(u16::from_le_bytes([raw[0], raw[1]]))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    let raw = bytes.get(at..at + 4)?;
    Some(u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    let raw = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let raw = bytes.get(at..at + 3)?;
    Some(u32::from_le_bytes([raw[0], raw[1], raw[2], 0]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn json(raw: &str) -> Value {
        Value::Json(Arc::from(raw))
    }

    #[test]
    fn json_pretty_compact_and_pointer() {
        let value = json(r#"{"a":[1,2],"b":{"c":"x"}}"#);
        let pretty = render_body(&value, DetailMode::JsonPretty, "");
        assert!(pretty.contains("\n"), "格式化应带缩进换行");
        let compact = render_body(&value, DetailMode::JsonCompact, "");
        assert_eq!(compact, r#"{"a":[1,2],"b":{"c":"x"}}"#);
        assert_eq!(render_body(&value, DetailMode::JsonCompact, "/a/0"), "1");
        assert_eq!(
            render_body(&value, DetailMode::JsonCompact, "b.c"),
            r#""x""#
        );
        assert!(render_body(&value, DetailMode::JsonCompact, "/missing").starts_with("路径不存在"));
    }

    #[test]
    fn binary_modes_switch_between_hex_and_text() {
        let value = Value::Bytes(Arc::from(b"hello\x00world".as_slice()));
        assert_eq!(modes(&value), &BINARY_MODES);
        let hex = render_body(&value, DetailMode::Hex, "");
        assert!(hex.contains("68 65 6c 6c 6f"), "十六进制应逐字节展示");
        assert!(hex.contains("|hello.world|"));
        let text = render_body(&value, DetailMode::Text, "");
        assert!(text.contains("hello"));
    }

    #[test]
    fn png_and_gif_dimensions_are_detected() {
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&320u32.to_be_bytes());
        png.extend_from_slice(&200u32.to_be_bytes());
        assert_eq!(
            image_info(&png),
            Some(ImageInfo {
                format: "PNG",
                width: 320,
                height: 200
            })
        );

        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&(64u16).to_le_bytes());
        gif.extend_from_slice(&(48u16).to_le_bytes());
        gif.extend_from_slice(&[0; 4]);
        assert_eq!(
            image_info(&gif),
            Some(ImageInfo {
                format: "GIF",
                width: 64,
                height: 48
            })
        );
    }

    #[test]
    fn jpeg_dimensions_skip_metadata_segments() {
        let mut jpeg = vec![0xFF, 0xD8];
        // APP0 段，长度 4（含长度本身），跳过。
        jpeg.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        // SOF0：长度 17，精度 8，高 90，宽 120。
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        jpeg.extend_from_slice(&90u16.to_be_bytes());
        jpeg.extend_from_slice(&120u16.to_be_bytes());
        jpeg.extend_from_slice(&[0; 9]);
        assert_eq!(
            image_info(&jpeg),
            Some(ImageInfo {
                format: "JPEG",
                width: 120,
                height: 90
            })
        );
    }

    #[test]
    fn ewkb_srid_is_parsed_but_plain_wkb_is_ignored() {
        let mut ewkb = vec![1];
        ewkb.extend_from_slice(&(0x2000_0001u32).to_le_bytes());
        ewkb.extend_from_slice(&4326u32.to_le_bytes());
        ewkb.extend_from_slice(&0.0f64.to_le_bytes());
        ewkb.extend_from_slice(&0.0f64.to_le_bytes());
        assert_eq!(
            geometry_info(&ewkb),
            Some(GeometryInfo {
                kind: "Point",
                srid: Some(4326)
            })
        );

        let mut wkb = vec![1];
        wkb.extend_from_slice(&1u32.to_le_bytes());
        wkb.extend_from_slice(&0.0f64.to_le_bytes());
        wkb.extend_from_slice(&0.0f64.to_le_bytes());
        assert_eq!(geometry_info(&wkb), None, "无 SRID 标记不猜几何");
    }

    #[test]
    fn timestamp_tz_notes_local_and_utc() {
        let value = Value::TimestampTz(
            chrono::DateTime::parse_from_rfc3339("2026-10-23T08:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let notes = notes(&value);
        assert!(notes.iter().any(|note| note.starts_with("UTC")));
        assert!(notes.iter().any(|note| note.starts_with("本地")));
    }
}
