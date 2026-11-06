use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// 逻辑类型：UI、导出、DDL 生成共用的一套类型口径。
///
/// 采用邻接标记（而非内部标记）：递归的 `Array` 变体在内部标记下会让
/// serde_json 的序列化 trait 求解无限展开（E0275）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum LogicalType {
    Bool,
    Int {
        bits: u8,
        unsigned: bool,
    },
    Float {
        bits: u8,
    },
    Decimal {
        precision: Option<u8>,
        scale: Option<u8>,
    },
    Text {
        max_len: Option<u32>,
    },
    Bytes,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Uuid,
    Json,
    Array(Box<LogicalType>),
    /// 驱动无法归类的服务端类型：保留原名，UI 标灰但可复制。
    Unknown(String),
}

impl LogicalType {
    pub fn label(&self) -> String {
        match self {
            Self::Bool => "bool".into(),
            Self::Int { bits, unsigned } => {
                format!("{}int{bits}", if *unsigned { "u" } else { "" })
            }
            Self::Float { bits } => format!("float{bits}"),
            Self::Decimal { precision, scale } => match (precision, scale) {
                (Some(p), Some(s)) => format!("decimal({p},{s})"),
                (Some(p), None) => format!("decimal({p})"),
                _ => "decimal".into(),
            },
            Self::Text { max_len } => max_len
                .map(|n| format!("text({n})"))
                .unwrap_or_else(|| "text".into()),
            Self::Bytes => "bytes".into(),
            Self::Date => "date".into(),
            Self::Time => "time".into(),
            Self::Timestamp => "timestamp".into(),
            Self::TimestampTz => "timestamptz".into(),
            Self::Uuid => "uuid".into(),
            Self::Json => "json".into(),
            Self::Array(inner) => format!("{}[]", inner.label()),
            Self::Unknown(name) => name.clone(),
        }
    }
}

/// 一个单元格的值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    /// 定点数按字符串保留，绝不转 f64（金额精度不可丢）。
    Decimal(Arc<str>),
    Text(Arc<str>),
    Bytes(Arc<[u8]>),
    Date(NaiveDate),
    Time(NaiveTime),
    Timestamp(NaiveDateTime),
    TimestampTz(DateTime<Utc>),
    Uuid([u8; 16]),
    Json(Arc<str>),
    Array(Vec<Value>),
    /// 超出阈值的大字段：只带总长度与预览。
    Omitted {
        total_bytes: u64,
        preview: Arc<[u8]>,
    },
    Unknown {
        native_type: String,
        text: Arc<str>,
    },
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// 单元格详情面板按这个选择渲染方式（文本 / JSON / 二进制 / 已省略）。
    pub fn detail_kind(&self) -> ValueDetailKind {
        match self {
            Self::Json(_) => ValueDetailKind::Json,
            Self::Bytes(_) => ValueDetailKind::Binary,
            Self::Omitted { total_bytes, .. } => ValueDetailKind::Omitted {
                total_bytes: *total_bytes,
            },
            Self::Text(_) | Self::Decimal(_) | Self::Unknown { .. } | Self::Array(_) => {
                ValueDetailKind::Text
            }
            _ => ValueDetailKind::Scalar,
        }
    }

    /// 详情面板的内容文本：不截断；二进制转成十六进制 + ASCII。
    pub fn detail_text(&self) -> String {
        match self {
            Self::Bytes(bytes) => hex_dump(bytes, 4096),
            Self::Omitted {
                total_bytes,
                preview,
            } => format!(
                "（仅预览前 {} 字节，共 {total_bytes} 字节）\n{}",
                preview.len(),
                String::from_utf8_lossy(preview)
            ),
            Self::Null => "NULL".into(),
            other => other.export_text(),
        }
    }

    /// 详情面板标题里显示的类型名。
    pub fn type_label(&self) -> &'static str {
        match self {
            Self::Null => "NULL",
            Self::Bool(_) => "布尔",
            Self::Int(_) => "整数",
            Self::UInt(_) => "无符号整数",
            Self::Float(_) => "浮点数",
            Self::Decimal(_) => "定点数",
            Self::Text(_) => "文本",
            Self::Bytes(_) => "二进制",
            Self::Date(_) => "日期",
            Self::Time(_) => "时间",
            Self::Timestamp(_) => "时间戳",
            Self::TimestampTz(_) => "时间戳（带时区）",
            Self::Uuid(_) => "UUID",
            Self::Json(_) => "JSON",
            Self::Array(_) => "数组",
            Self::Omitted { .. } => "大字段",
            Self::Unknown { .. } => "未知类型",
        }
    }

    /// 网格显示用的短文本。长值在这里被截断，详情走单元格查看器。
    pub fn display_text(&self) -> String {
        const MAX: usize = 240;
        if self.is_null() {
            return "NULL".into();
        }
        truncate(self.raw_text(), MAX)
    }

    /// 导出用全文：不截断；NULL 导出为空字段（CSV 惯例）。
    pub fn export_text(&self) -> String {
        if self.is_null() {
            String::new()
        } else {
            self.raw_text()
        }
    }

    fn raw_text(&self) -> String {
        match self {
            Self::Null => String::new(),
            Self::Bool(v) => v.to_string(),
            Self::Int(v) => v.to_string(),
            Self::UInt(v) => v.to_string(),
            Self::Float(v) => {
                if v.fract() == 0.0 {
                    format!("{v:.1}")
                } else {
                    v.to_string()
                }
            }
            Self::Decimal(v) | Self::Text(v) | Self::Json(v) => v.to_string(),
            Self::Bytes(b) => format!("0x{} ({} B)", hex_preview(b), b.len()),
            Self::Date(v) => v.to_string(),
            Self::Time(v) => v.to_string(),
            Self::Timestamp(v) => v.to_string(),
            Self::TimestampTz(v) => v.to_rfc3339(),
            Self::Uuid(v) => format_uuid(v),
            Self::Array(items) => {
                let inner: Vec<String> = items.iter().take(8).map(|v| v.display_text()).collect();
                format!("{{{}}}", inner.join(","))
            }
            Self::Omitted {
                total_bytes,
                preview,
            } => format!("[BLOB {} B, preview {} B]", total_bytes, preview.len()),
            Self::Unknown { native_type, text } => {
                format!("{text}  ({native_type})")
            }
        }
    }

    pub fn approx_bytes(&self) -> usize {
        match self {
            Self::Null | Self::Bool(_) | Self::Int(_) | Self::UInt(_) | Self::Float(_) => 8,
            Self::Decimal(s) | Self::Text(s) | Self::Json(s) => s.len(),
            Self::Bytes(b) => b.len(),
            Self::Date(_) | Self::Time(_) | Self::Timestamp(_) | Self::TimestampTz(_) => 16,
            Self::Uuid(_) => 16,
            Self::Array(items) => items.iter().map(Self::approx_bytes).sum(),
            Self::Omitted { preview, .. } => preview.len(),
            Self::Unknown { text, .. } => text.len(),
        }
    }
}

pub type Row = Vec<Value>;

/// 网格排序比较：数值按数值大小（含 decimal 字符串）、时间按时间、其余按文本。
/// NULL 不在这里处理 —— 调用方统一把 NULL 排在最后。
pub fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    fn as_number(v: &Value) -> Option<f64> {
        match v {
            Value::Bool(v) => Some(if *v { 1.0 } else { 0.0 }),
            Value::Int(v) => Some(*v as f64),
            Value::UInt(v) => Some(*v as f64),
            Value::Float(v) => Some(*v),
            Value::Decimal(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }

    if let (Some(x), Some(y)) = (as_number(a), as_number(b)) {
        if let Some(ordering) = x.partial_cmp(&y) {
            return ordering;
        }
    }

    match (a, b) {
        (Value::Date(x), Value::Date(y)) => x.cmp(y),
        (Value::Time(x), Value::Time(y)) => x.cmp(y),
        (Value::Timestamp(x), Value::Timestamp(y)) => x.cmp(y),
        (Value::TimestampTz(x), Value::TimestampTz(y)) => x.cmp(y),
        _ => a.display_text().cmp(&b.display_text()),
    }
}

fn truncate(mut s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max).collect();
    s = format!("{cut}…");
    s
}

/// 单元格详情面板的渲染分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueDetailKind {
    /// 布尔 / 数字 / 时间等一行能显示完的值。
    Scalar,
    /// 长文本。
    Text,
    /// JSON：查看器里格式化缩进。
    Json,
    /// 二进制：十六进制转储。
    Binary,
    /// 超出阈值的大字段，只有预览。
    Omitted { total_bytes: u64 },
}

/// `xxd` 风格的十六进制转储（最多 `max` 字节）。
pub fn hex_dump(bytes: &[u8], max: usize) -> String {
    let take = bytes.len().min(max);
    let mut out = String::with_capacity(take * 4 + 64);
    for (row, chunk) in bytes[..take].chunks(16).enumerate() {
        out.push_str(&format!("{:08x}  ", row * 16));
        for i in 0..16 {
            match chunk.get(i) {
                Some(byte) => out.push_str(&format!("{byte:02x} ")),
                None => out.push_str("   "),
            }
            if i == 7 {
                out.push(' ');
            }
        }
        out.push_str(" |");
        for byte in chunk {
            let ch = *byte as char;
            out.push(if ch.is_ascii_graphic() || ch == ' ' {
                ch
            } else {
                '.'
            });
        }
        out.push_str("|\n");
    }
    if bytes.len() > take {
        out.push_str(&format!("… 共 {} 字节，仅显示前 {take} 字节", bytes.len()));
    }
    out
}

fn hex_preview(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

fn format_uuid(bytes: &[u8; 16]) -> String {
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn numeric_values_sort_by_magnitude_not_text() {
        assert_eq!(
            compare_values(&Value::Int(9), &Value::Int(10)),
            Ordering::Less
        );
        assert_eq!(
            compare_values(&Value::Decimal("2.50".into()), &Value::Int(10)),
            Ordering::Less
        );
    }

    #[test]
    fn text_values_fall_back_to_display_order() {
        assert_eq!(
            compare_values(&Value::Text("apple".into()), &Value::Text("banana".into())),
            Ordering::Less
        );
    }

    #[test]
    fn export_text_keeps_full_value_and_blanks_null() {
        let long = "x".repeat(500);
        let value = Value::Text(long.as_str().into());
        assert_eq!(value.display_text().chars().count(), 241);
        assert_eq!(value.export_text().chars().count(), 500);
        assert_eq!(Value::Null.export_text(), "");
        assert_eq!(Value::Null.display_text(), "NULL");
    }
}
