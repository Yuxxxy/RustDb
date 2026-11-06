//! 数据导入（F-74）：解析 → 字段映射 / 必填校验 → 分批 INSERT。
//!
//! 纯逻辑、无 IO、无数据库：文件读取与执行分别由调用方和会话 Actor 负责。
//! 口径（需求 15.5 / R-15）：
//! * 文本格式先行（CSV / TSV / JSON / NDJSON / INSERT 字面量）；Excel 需要
//!   `calamine`，当前构建未启用，显式返回“不支持”而不是静默忽略；
//! * 只做**预检**与 SQL 生成；跨批失败不回滚，失败批次由执行层报告；
//! * INSERT 字面量走词法解析，只有 `NULL / 布尔 / 数字 / 字符串 / 日期前缀`
//!   五类可接受，函数调用、表达式、子查询一律拒绝，不做 `eval` 式拼接。

use crate::edit::{ident, literal};
use rustdb_db_api::{ColumnInfo, ColumnMeta, DbKind, LogicalType, Value};
use rustdb_sql_lang::lex::{self, Token, TokenKind};
use std::fmt;
use std::sync::Arc;

/// 支持的导入格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFormat {
    Csv,
    Tsv,
    Json,
    Ndjson,
    InsertSql,
    /// 需要 `calamine`；当前构建显式拒绝。
    Xlsx,
}

impl ImportFormat {
    pub const ALL: [ImportFormat; 6] = [
        Self::Csv,
        Self::Tsv,
        Self::Json,
        Self::Ndjson,
        Self::InsertSql,
        Self::Xlsx,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Tsv => "TSV",
            Self::Json => "JSON",
            Self::Ndjson => "NDJSON",
            Self::InsertSql => "SQL INSERT",
            Self::Xlsx => "Excel",
        }
    }

    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "csv" => Self::Csv,
            "tsv" | "tab" => Self::Tsv,
            "json" => Self::Json,
            "ndjson" | "jsonl" => Self::Ndjson,
            "sql" | "txt" => Self::InsertSql,
            "xlsx" | "xlsm" => Self::Xlsx,
            _ => return None,
        })
    }

    pub fn default_delimiter(self) -> Option<char> {
        match self {
            Self::Csv => Some(','),
            Self::Tsv => Some('\t'),
            _ => None,
        }
    }

    pub fn is_delimited(self) -> bool {
        matches!(self, Self::Csv | Self::Tsv)
    }
}

/// 源文件编码；`Auto` 先看 BOM，再尝试 UTF-8，最后按 GBK 读取。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportEncoding {
    Auto,
    Utf8,
    Gbk,
    Utf16Le,
    Utf16Be,
    Big5,
}

impl ImportEncoding {
    pub const ALL: [ImportEncoding; 6] = [
        Self::Auto,
        Self::Utf8,
        Self::Gbk,
        Self::Utf16Le,
        Self::Utf16Be,
        Self::Big5,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "自动",
            Self::Utf8 => "UTF-8",
            Self::Gbk => "GBK",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
            Self::Big5 => "Big5",
        }
    }

    /// 循环切换到下一项（面板上的紧凑选择器用）。
    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|item| *item == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    Append,
    Truncate,
}

impl ImportMode {
    pub const ALL: [ImportMode; 2] = [Self::Append, Self::Truncate];

    pub fn label(self) -> &'static str {
        match self {
            Self::Append => "追加",
            Self::Truncate => "清空后导入",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Append => Self::Truncate,
            Self::Truncate => Self::Append,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
    pub format: ImportFormat,
    pub encoding: ImportEncoding,
    /// 覆盖格式默认分隔符（仅 CSV / TSV 生效）。
    pub delimiter: Option<char>,
    /// 第一条（跳过 `skip_rows` 后）是否为表头。
    pub header: bool,
    /// 跳过的开头记录数（文件里的前导说明行）。
    pub skip_rows: usize,
    /// 最多导入多少条数据行；`None` 表示全部。
    pub max_rows: Option<usize>,
    pub trim: bool,
    /// 空串是否转换为 NULL。
    pub empty_as_null: bool,
    /// 与空串等价的 NULL 文本（例如 `\N`）。
    pub null_text: Option<String>,
    /// 默认 500 行 / 批。
    pub batch_rows: usize,
    pub mode: ImportMode,
    /// 显式字段映射：源列下标 → 目标列下标（`None` 表示忽略该源列）。
    /// 空表示自动映射。
    pub mapping: Vec<Option<usize>>,
    /// 无表头且自动映射时使用的源列名（可空，按位置生成 `column_N`）。
    pub columns: Vec<String>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            format: ImportFormat::Csv,
            encoding: ImportEncoding::Auto,
            delimiter: None,
            header: true,
            skip_rows: 0,
            max_rows: None,
            trim: true,
            empty_as_null: true,
            null_text: None,
            batch_rows: 500,
            mode: ImportMode::Append,
            mapping: Vec::new(),
            columns: Vec::new(),
        }
    }
}

impl ImportOptions {
    pub fn delimiter(&self) -> char {
        self.delimiter
            .or_else(|| self.format.default_delimiter())
            .unwrap_or(',')
    }
}

/// 一个源单元格：文本来自 CSV/JSON，`Typed` 来自 INSERT 字面量。
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Text(String),
    Typed(Value),
}

impl Cell {
    pub fn text(&self) -> Option<String> {
        match self {
            Self::Null => None,
            Self::Text(text) => Some(text.clone()),
            Self::Typed(value) => Some(value.display_text()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportIssue {
    /// 1-based 数据行号（表头之后）；`None` 表示与具体行无关。
    pub row: Option<usize>,
    pub column: Option<String>,
    pub message: String,
}

impl ImportIssue {
    pub fn new(row: Option<usize>, column: Option<String>, message: impl Into<String>) -> Self {
        Self {
            row,
            column,
            message: message.into(),
        }
    }

    pub fn line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(row) = self.row {
            parts.push(format!("第 {row} 行"));
        }
        if let Some(column) = &self.column {
            parts.push(format!("列 {column}"));
        }
        if parts.is_empty() {
            self.message.clone()
        } else {
            format!("{}：{}", parts.join(" · "), self.message)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedData {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    /// 非阻断提示（编码回退、多余字段等）。
    pub warnings: Vec<ImportIssue>,
}

impl ParsedData {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// 前 `limit` 行的文本预览（NULL 显示为空）。
    pub fn preview(&self, limit: usize) -> Vec<Vec<String>> {
        self.rows
            .iter()
            .take(limit)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text().unwrap_or_default())
                    .collect()
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    Unsupported(String),
    Encoding(String),
    Parse {
        row: Option<usize>,
        message: String,
    },
    /// 预检失败：字段映射、必填、类型转换。调用方据此阻止执行。
    Invalid(Vec<ImportIssue>),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(message) | Self::Encoding(message) => f.write_str(message),
            Self::Parse { row, message } => match row {
                Some(row) => write!(f, "第 {row} 行：{message}"),
                None => f.write_str(message),
            },
            Self::Invalid(issues) => {
                let shown = issues
                    .iter()
                    .take(3)
                    .map(ImportIssue::line)
                    .collect::<Vec<_>>()
                    .join("；");
                if issues.len() > 3 {
                    write!(f, "{shown}；另有 {} 个问题", issues.len() - 3)
                } else {
                    f.write_str(&shown)
                }
            }
        }
    }
}

impl std::error::Error for ImportError {}

/// 解码源文件。返回文本与回退提示（例如未识别编码时按 GBK 读取）。
pub fn decode(
    bytes: &[u8],
    encoding: ImportEncoding,
) -> Result<(String, Option<String>), ImportError> {
    let decode_with = |encoding: &'static encoding_rs::Encoding, label: &str| {
        let (text, _, had_errors) = encoding.decode(bytes);
        if had_errors {
            return Err(ImportError::Encoding(format!(
                "文件包含无法按 {label} 解码的字节，请换一种编码"
            )));
        }
        Ok(strip_bom(text.into_owned()))
    };

    match encoding {
        ImportEncoding::Auto => {
            if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
                return Ok((decode_with(encoding_rs::UTF_8, "UTF-8")?, None));
            }
            if bytes.starts_with(&[0xff, 0xfe]) {
                return Ok((decode_with(encoding_rs::UTF_16LE, "UTF-16 LE")?, None));
            }
            if bytes.starts_with(&[0xfe, 0xff]) {
                return Ok((decode_with(encoding_rs::UTF_16BE, "UTF-16 BE")?, None));
            }
            if let Ok(text) = std::str::from_utf8(bytes) {
                return Ok((strip_bom(text.to_string()), None));
            }
            Ok((
                decode_with(encoding_rs::GBK, "GBK")?,
                Some("未检测到 BOM 且不是合法 UTF-8，已按 GBK 读取".into()),
            ))
        }
        ImportEncoding::Utf8 => Ok((decode_with(encoding_rs::UTF_8, "UTF-8")?, None)),
        ImportEncoding::Gbk => Ok((decode_with(encoding_rs::GBK, "GBK")?, None)),
        ImportEncoding::Utf16Le => Ok((decode_with(encoding_rs::UTF_16LE, "UTF-16 LE")?, None)),
        ImportEncoding::Utf16Be => Ok((decode_with(encoding_rs::UTF_16BE, "UTF-16 BE")?, None)),
        ImportEncoding::Big5 => Ok((decode_with(encoding_rs::BIG5, "Big5")?, None)),
    }
}

fn strip_bom(text: String) -> String {
    text.strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(text)
}

/// 解析源文件（按 `options.format`）。不会写文件、不连数据库。
pub fn parse(bytes: &[u8], options: &ImportOptions) -> Result<ParsedData, ImportError> {
    if options.format == ImportFormat::Xlsx {
        return Err(ImportError::Unsupported(
            "Excel 导入需要 calamine 依赖，当前构建未启用；请先另存为 CSV".into(),
        ));
    }
    let (text, warning) = decode(bytes, options.encoding)?;
    let mut data = match options.format {
        ImportFormat::Csv | ImportFormat::Tsv => parse_delimited(&text, options)?,
        ImportFormat::Json | ImportFormat::Ndjson => parse_json(&text, options)?,
        ImportFormat::InsertSql => parse_insert_sql(&text, options)?,
        ImportFormat::Xlsx => unreachable!("已在上面拒绝"),
    };
    if let Some(warning) = warning {
        data.warnings.push(ImportIssue::new(None, None, warning));
    }
    ensure_columns(&mut data);
    if data.rows.is_empty() {
        return Err(ImportError::Parse {
            row: None,
            message: "文件里没有可导入的数据行".into(),
        });
    }
    Ok(data)
}

fn ensure_columns(data: &mut ParsedData) {
    if data.columns.is_empty() {
        let width = data.rows.iter().map(Vec::len).max().unwrap_or(0);
        data.columns = (1..=width).map(|index| format!("column_{index}")).collect();
    }
    // 重名列会让按名映射有歧义：保留首个，其余追加序号。
    let mut seen: Vec<String> = Vec::new();
    for name in &mut data.columns {
        let base = name.trim().to_string();
        let mut candidate = if base.is_empty() {
            String::from("column")
        } else {
            base
        };
        let mut suffix = 2;
        while seen
            .iter()
            .any(|used| used.eq_ignore_ascii_case(&candidate))
        {
            candidate = format!("{}_{suffix}", name.trim());
            suffix += 1;
        }
        seen.push(candidate.clone());
        *name = candidate;
    }
}

// --- 分隔符文本 ---

struct Record {
    /// 1-based 文件行号（记录起始行）。
    line: usize,
    fields: Vec<String>,
}

fn parse_delimited(text: &str, options: &ImportOptions) -> Result<ParsedData, ImportError> {
    let delimiter = options.delimiter();
    let records = read_records(text, delimiter)?;
    let warnings = Vec::new();

    let mut iter = records.into_iter().skip(options.skip_rows);
    let columns = if options.header {
        let Some(header) = iter.next() else {
            return Err(ImportError::Parse {
                row: None,
                message: "文件没有表头行".into(),
            });
        };
        header
            .fields
            .iter()
            .map(|field| normalize_field(field, options, true))
            .map(|cell| cell.text().unwrap_or_default())
            .collect::<Vec<_>>()
    } else if options.columns.is_empty() {
        let Some(first) = iter.next() else {
            return Err(ImportError::Parse {
                row: None,
                message: "文件没有数据行".into(),
            });
        };
        let columns = (1..=first.fields.len())
            .map(|index| format!("column_{index}"))
            .collect::<Vec<_>>();
        let mut rows = vec![to_row(&first.fields, first.line, options)?];
        rows.extend(collect_rows(iter, columns.len(), options)?);
        if let Some(limit) = options.max_rows {
            rows.truncate(limit);
        }
        return finish(columns, rows, warnings, options);
    } else {
        options.columns.clone()
    };

    let width = columns.len();
    let rows = collect_rows(iter, width, options)?;
    finish(columns, rows, warnings, options)
}

fn collect_rows(
    iter: impl Iterator<Item = Record>,
    width: usize,
    options: &ImportOptions,
) -> Result<Vec<Vec<Cell>>, ImportError> {
    let mut rows = Vec::new();
    for record in iter {
        if options.max_rows.is_some_and(|limit| rows.len() >= limit) {
            break;
        }
        rows.push(to_row(&record.fields, record.line, options)?);
        if rows.last().is_some_and(|row| row.len() != width) {
            // 行内多/少字段：显式失败，不静默补空。
            let got = rows.last().map(Vec::len).unwrap_or(0);
            let row = rows.len();
            return Err(ImportError::Invalid(vec![ImportIssue::new(
                Some(row),
                None,
                format!("第 {row} 条数据有 {got} 个字段，表头是 {width} 个"),
            )]));
        }
    }
    Ok(rows)
}

fn finish(
    columns: Vec<String>,
    rows: Vec<Vec<Cell>>,
    warnings: Vec<ImportIssue>,
    _options: &ImportOptions,
) -> Result<ParsedData, ImportError> {
    let data = ParsedData {
        columns,
        rows,
        warnings,
    };
    if data.is_empty() {
        return Err(ImportError::Parse {
            row: None,
            message: "文件里没有可导入的数据行".into(),
        });
    }
    Ok(data)
}

fn to_row(
    fields: &[String],
    _line: usize,
    options: &ImportOptions,
) -> Result<Vec<Cell>, ImportError> {
    Ok(fields
        .iter()
        .map(|field| normalize_field(field, options, false))
        .collect())
}

fn normalize_field(field: &str, options: &ImportOptions, header: bool) -> Cell {
    let text = if options.trim && !header {
        field.trim()
    } else if header {
        field.trim()
    } else {
        field
    };
    if !header {
        if let Some(null_text) = options.null_text.as_deref() {
            if text == null_text {
                return Cell::Null;
            }
        }
        if options.empty_as_null && text.is_empty() {
            return Cell::Null;
        }
    }
    Cell::Text(text.to_string())
}

/// RFC 4180 读取：支持引号内分隔符 / 换行、`""` 转义、CRLF。
fn read_records(text: &str, delimiter: char) -> Result<Vec<Record>, ImportError> {
    let mut records = Vec::new();
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut line = 1usize;
    let mut record_line = 1usize;
    let mut chars = text.chars().peekable();
    let mut started = false;

    while let Some(ch) = chars.next() {
        started = true;
        if in_quotes {
            match ch {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                '\n' => {
                    line += 1;
                    field.push(ch);
                }
                other => field.push(other),
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() => in_quotes = true,
            c if c == delimiter => {
                fields.push(std::mem::take(&mut field));
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                line += 1;
                fields.push(std::mem::take(&mut field));
                push_record(&mut records, &mut fields, record_line);
                record_line = line;
            }
            '\n' => {
                line += 1;
                fields.push(std::mem::take(&mut field));
                push_record(&mut records, &mut fields, record_line);
                record_line = line;
            }
            other => field.push(other),
        }
        started = false;
    }
    if in_quotes {
        return Err(ImportError::Parse {
            row: Some(records.len() + 1),
            message: format!("第 {record_line} 行的引号没有闭合"),
        });
    }
    if started || !field.is_empty() || !fields.is_empty() {
        fields.push(field);
        push_record(&mut records, &mut fields, record_line);
    }
    Ok(records)
}

fn push_record(records: &mut Vec<Record>, fields: &mut Vec<String>, line: usize) {
    let record = std::mem::take(fields);
    // 完全空行（单个空字段）按噪声跳过；有引号的空串仍是有效数据。
    if record.len() == 1 && record[0].is_empty() {
        return;
    }
    records.push(Record {
        line,
        fields: record,
    });
}

// --- JSON ---

fn parse_json(text: &str, options: &ImportOptions) -> Result<ParsedData, ImportError> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('[') {
        let value: serde_json::Value = serde_json::from_str(text).map_err(json_error)?;
        let items = value.as_array().cloned().unwrap_or_default();
        return from_json_items(items, options);
    }

    let mut items = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|error| ImportError::Parse {
                row: Some(index + 1),
                message: format!("JSON 解析失败：{error}"),
            })?;
        items.push(value);
    }
    from_json_items(items, options)
}

fn json_error(error: serde_json::Error) -> ImportError {
    ImportError::Parse {
        row: Some(error.line()),
        message: format!("JSON 解析失败：{error}"),
    }
}

fn from_json_items(
    items: Vec<serde_json::Value>,
    options: &ImportOptions,
) -> Result<ParsedData, ImportError> {
    let Some(first) = items.first() else {
        return Err(ImportError::Parse {
            row: None,
            message: "JSON 里没有对象".into(),
        });
    };

    match first {
        serde_json::Value::Object(_) => {
            let mut columns: Vec<String> = Vec::new();
            let mut warnings = Vec::new();
            for item in &items {
                let Some(object) = item.as_object() else {
                    let row = items
                        .iter()
                        .position(|value| std::ptr::eq(value, item))
                        .map(|index| index + 1);
                    return Err(ImportError::Parse {
                        row,
                        message: "JSON 数组里混有非对象元素".into(),
                    });
                };
                for key in object.keys() {
                    if !columns.iter().any(|column| column == key) {
                        columns.push(key.clone());
                    }
                }
            }
            let mut rows = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let object = item.as_object().expect("已校验");
                let mut row = Vec::with_capacity(columns.len());
                for column in &columns {
                    row.push(match object.get(column) {
                        Some(value) => json_cell(value, options),
                        None => Cell::Null,
                    });
                }
                if object.len() > columns.len() {
                    warnings.push(ImportIssue::new(
                        Some(index + 1),
                        None,
                        "存在表头之外的字段，已忽略",
                    ));
                }
                rows.push(row);
            }
            finish(columns, rows, warnings, options)
        }
        serde_json::Value::Array(_) => {
            let width = items
                .iter()
                .filter_map(|item| item.as_array().map(Vec::len))
                .max()
                .unwrap_or(0);
            let columns = (1..=width)
                .map(|index| format!("column_{index}"))
                .collect::<Vec<_>>();
            let mut rows = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let Some(values) = item.as_array() else {
                    return Err(ImportError::Parse {
                        row: Some(index + 1),
                        message: "JSON 数组里混有非数组元素".into(),
                    });
                };
                let mut row: Vec<Cell> = values
                    .iter()
                    .map(|value| json_cell(value, options))
                    .collect();
                row.resize(width, Cell::Null);
                rows.push(row);
            }
            finish(columns, rows, Vec::new(), options)
        }
        _ => Err(ImportError::Parse {
            row: Some(1),
            message: "JSON 顶层需要对象数组或数组数组".into(),
        }),
    }
}

fn json_cell(value: &serde_json::Value, options: &ImportOptions) -> Cell {
    use serde_json::Value as J;
    match value {
        J::Null => Cell::Null,
        J::Bool(value) => Cell::Typed(Value::Bool(*value)),
        J::Number(number) => {
            if let Some(value) = number.as_i64() {
                Cell::Typed(Value::Int(value))
            } else if let Some(value) = number.as_u64() {
                Cell::Typed(Value::UInt(value))
            } else {
                Cell::Typed(Value::Float(number.as_f64().unwrap_or(f64::NAN)))
            }
        }
        J::String(text) => {
            let text = if options.trim {
                text.trim()
            } else {
                text.as_str()
            };
            if options.empty_as_null && text.is_empty() {
                Cell::Null
            } else {
                Cell::Text(text.to_string())
            }
        }
        other => Cell::Text(other.to_string()),
    }
}

// --- INSERT 字面量 ---

fn parse_insert_sql(text: &str, options: &ImportOptions) -> Result<ParsedData, ImportError> {
    let statements = rustdb_sql_lang::split_statements(text);
    if statements.is_empty() {
        return Err(ImportError::Parse {
            row: None,
            message: "没有找到 INSERT 语句".into(),
        });
    }

    let mut columns: Option<Vec<String>> = None;
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    for statement in &statements {
        let tokens: Vec<Token<'_>> = lex::tokens(&statement.text)
            .into_iter()
            .filter(|token| !token.is_trivia())
            .collect();
        let line = line_of(&statement.text, statement.start);
        let parsed = parse_one_insert(&tokens, line)?;
        if let Some(existing) = &columns {
            if let Some(found) = &parsed.columns {
                if !same_columns(existing, found) {
                    return Err(ImportError::Parse {
                        row: Some(line),
                        message: "多条 INSERT 的列清单不一致".into(),
                    });
                }
            }
        } else {
            columns = parsed.columns.clone();
        }
        rows.extend(parsed.rows);
    }

    let Some(columns) = columns else {
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        let generated = (1..=width)
            .map(|index| format!("column_{index}"))
            .collect::<Vec<_>>();
        return finish(generated, rows, Vec::new(), options);
    };
    finish(columns, rows, Vec::new(), options)
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn same_columns(left: &[String], right: &[String]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

struct ParsedInsert {
    columns: Option<Vec<String>>,
    rows: Vec<Vec<Cell>>,
}

fn parse_one_insert(tokens: &[Token<'_>], line: usize) -> Result<ParsedInsert, ImportError> {
    let mut cursor = Cursor::new(tokens);
    let fail = |message: String| ImportError::Parse {
        row: Some(line),
        message,
    };

    if !cursor.take_word("INSERT") {
        return Err(fail("只支持 INSERT 语句".into()));
    }
    cursor.take_word("INTO");

    // 表名：Word / QuotedIdent 与 `.` 的组合，直到列清单或 VALUES。
    let mut table_parts = 0usize;
    while let Some(token) = cursor.peek() {
        match token.kind {
            TokenKind::Word | TokenKind::QuotedIdent => {
                cursor.bump();
                table_parts += 1;
            }
            TokenKind::Punct if token.text == "." => {
                cursor.bump();
            }
            _ => break,
        }
    }
    if table_parts == 0 {
        return Err(fail("缺少目标表名".into()));
    }

    let columns = if cursor.peek_punct("(") {
        cursor.bump();
        let mut columns = Vec::new();
        loop {
            let Some(token) = cursor.bump() else {
                return Err(fail("列清单没有闭合".into()));
            };
            match token.kind {
                TokenKind::Word | TokenKind::QuotedIdent => {
                    columns.push(unquote_ident(token.text));
                }
                TokenKind::Punct if token.text == "," => continue,
                _ => return Err(fail(format!("列清单里有无法识别的内容：{}", token.text))),
            }
            if cursor.peek_punct(")") {
                cursor.bump();
                break;
            }
            if !cursor.peek_punct(",") {
                return Err(fail("列清单缺少逗号".into()));
            }
        }
        if columns.is_empty() {
            return Err(fail("列清单为空".into()));
        }
        Some(columns)
    } else {
        None
    };

    if !cursor.take_word("VALUES") {
        return Err(fail("只支持 INSERT ... VALUES".into()));
    }

    let mut rows = Vec::new();
    loop {
        if !cursor.peek_punct("(") {
            return Err(fail("VALUES 后需要括号".into()));
        }
        cursor.bump();
        let mut row = Vec::new();
        loop {
            row.push(parse_literal(&mut cursor).map_err(|message| fail(message))?);
            if cursor.peek_punct(",") {
                cursor.bump();
                continue;
            }
            if cursor.peek_punct(")") {
                cursor.bump();
                break;
            }
            return Err(fail("值清单缺少逗号或右括号".into()));
        }
        if let Some(expected) = columns.as_ref() {
            if row.len() != expected.len() {
                return Err(fail(format!(
                    "第 {} 组值有 {} 个，列清单是 {} 个",
                    rows.len() + 1,
                    row.len(),
                    expected.len()
                )));
            }
        }
        rows.push(row);
        if cursor.peek_punct(",") {
            cursor.bump();
            continue;
        }
        break;
    }
    if !cursor.is_end() {
        return Err(fail("INSERT 语句末尾有多余内容".into()));
    }
    Ok(ParsedInsert { columns, rows })
}

struct Cursor<'a, 'b> {
    tokens: &'a [Token<'b>],
    pos: usize,
}

impl<'a, 'b> Cursor<'a, 'b> {
    fn new(tokens: &'a [Token<'b>]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&'a Token<'b>> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<&'a Token<'b>> {
        let token = self.tokens.get(self.pos);
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    fn peek_punct(&self, text: &str) -> bool {
        self.peek()
            .is_some_and(|token| token.kind == TokenKind::Punct && token.text == text)
    }

    fn take_word(&mut self, word: &str) -> bool {
        if self.peek().is_some_and(|token| token.is_word(word)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn is_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }
}

fn parse_literal(cursor: &mut Cursor<'_, '_>) -> Result<Cell, String> {
    let Some(token) = cursor.bump() else {
        return Err("缺少值".into());
    };
    match token.kind {
        TokenKind::Str => Ok(Cell::Text(unescape_sql_string(token.text))),
        TokenKind::Number => number_cell(token.text),
        TokenKind::Punct if token.text == "-" || token.text == "+" => {
            let Some(next) = cursor.bump() else {
                return Err("正负号后缺少数字".into());
            };
            if next.kind != TokenKind::Number {
                return Err(format!("不支持表达式：{} {}", token.text, next.text));
            }
            let signed = format!("{}{}", token.text, next.text);
            number_cell(&signed)
        }
        TokenKind::Word => {
            let upper = token.text.to_ascii_uppercase();
            match upper.as_str() {
                "NULL" => Ok(Cell::Null),
                "TRUE" => Ok(Cell::Typed(Value::Bool(true))),
                "FALSE" => Ok(Cell::Typed(Value::Bool(false))),
                "DATE" | "TIME" | "TIMESTAMP" | "TIMESTAMPTZ" | "DATETIME" | "UUID" | "JSON" => {
                    let Some(next) = cursor.bump() else {
                        return Err(format!("{upper} 后缺少字符串"));
                    };
                    if next.kind != TokenKind::Str {
                        return Err(format!("{upper} 后只能是字符串字面量"));
                    }
                    Ok(Cell::Text(unescape_sql_string(next.text)))
                }
                _ => Err(format!(
                    "不支持的取值 `{}`：只接受 NULL / 布尔 / 数字 / 字符串 / 日期字面量",
                    token.text
                )),
            }
        }
        _ => Err(format!("不支持的取值 `{}`", token.text)),
    }
}

fn number_cell(raw: &str) -> Result<Cell, String> {
    if raw.contains(['.', 'e', 'E']) {
        let value: f64 = raw.parse().map_err(|_| format!("`{raw}` 不是合法数字"))?;
        if !value.is_finite() {
            return Err(format!("`{raw}` 不是有限数字"));
        }
        return Ok(Cell::Typed(Value::Float(value)));
    }
    if let Ok(value) = raw.parse::<i64>() {
        return Ok(Cell::Typed(Value::Int(value)));
    }
    if let Ok(value) = raw.parse::<u64>() {
        return Ok(Cell::Typed(Value::UInt(value)));
    }
    let value: f64 = raw.parse().map_err(|_| format!("`{raw}` 不是合法数字"))?;
    Ok(Cell::Typed(Value::Float(value)))
}

fn unescape_sql_string(raw: &str) -> String {
    let mut chars = raw.chars();
    if chars.next() != Some('\'') {
        return raw.to_string();
    }
    let inner = raw
        .strip_prefix('\'')
        .and_then(|text| text.strip_suffix('\''))
        .unwrap_or(raw);
    let mut out = String::with_capacity(inner.len());
    let mut iter = inner.chars().peekable();
    while let Some(ch) = iter.next() {
        match ch {
            '\\' => match iter.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some('Z') => out.push('\u{1a}'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            '\'' if iter.peek() == Some(&'\'') => {
                iter.next();
                out.push('\'');
            }
            other => out.push(other),
        }
    }
    out
}

fn unquote_ident(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'`' && last == b'`') {
            let inner = &trimmed[1..trimmed.len() - 1];
            return inner.replace("\"\"", "\"").replace("``", "`");
        }
    }
    trimmed.to_string()
}

// --- 字段映射与批次 SQL ---

#[derive(Debug, Clone, PartialEq)]
pub struct MappedColumn {
    pub source: usize,
    pub source_name: String,
    pub target: usize,
    pub target_name: String,
    pub logical_type: LogicalType,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportBatch {
    /// 1-based 批次序号。
    pub index: usize,
    /// 1-based 数据行区间（含两端）。
    pub start_row: usize,
    pub end_row: usize,
    pub sql: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportPlan {
    pub table: String,
    pub dialect: DbKind,
    pub mapping: Vec<MappedColumn>,
    pub total_rows: usize,
    pub batches: Vec<ImportBatch>,
    /// `ImportMode::Truncate` 时先执行的清空语句。
    pub truncate_sql: Option<String>,
    pub warnings: Vec<ImportIssue>,
}

impl ImportPlan {
    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }

    pub fn mapped_targets(&self) -> Vec<String> {
        self.mapping
            .iter()
            .map(|column| column.target_name.clone())
            .collect()
    }
}

/// 目标列的必填判定：非空、无默认值、非自增、非生成列。
pub fn is_required(column: &ColumnInfo) -> bool {
    !column.meta.nullable
        && column.default.is_none()
        && !column.is_auto_increment
        && !column.is_generated
}

/// 自动映射：先按列名（忽略大小写）匹配；一个都对不上时按位置。
pub fn auto_mapping(source: &[String], target: &[ColumnInfo]) -> Vec<Option<usize>> {
    let importable: Vec<usize> = target
        .iter()
        .enumerate()
        .filter(|(_, column)| !column.is_generated)
        .map(|(index, _)| index)
        .collect();
    let mut mapping: Vec<Option<usize>> = source
        .iter()
        .map(|name| {
            target.iter().position(|column| {
                !column.is_generated && column.meta.name.eq_ignore_ascii_case(name)
            })
        })
        .collect();
    if mapping.iter().all(Option::is_none) {
        for (slot, target_index) in mapping.iter_mut().zip(importable) {
            *slot = Some(target_index);
        }
    }
    mapping
}

/// 预检 + 生成批次。任何结构性问题返回 [`ImportError::Invalid`] 阻止执行。
pub fn build_plan(
    dialect: DbKind,
    table: &str,
    target: &[ColumnInfo],
    data: &ParsedData,
    options: &ImportOptions,
) -> Result<ImportPlan, ImportError> {
    let source_count = data.columns.len();
    let mapping_source: Vec<Option<usize>> = if options.mapping.len() == source_count {
        options.mapping.clone()
    } else {
        auto_mapping(&data.columns, target)
    };

    let mut issues: Vec<ImportIssue> = Vec::new();
    let mut warnings = data.warnings.clone();
    let mut mapped_targets: Vec<usize> = Vec::new();
    let mut mapping: Vec<MappedColumn> = Vec::new();

    for (source, slot) in mapping_source.iter().enumerate() {
        let Some(target_index) = *slot else {
            warnings.push(ImportIssue::new(
                None,
                Some(data.columns[source].clone()),
                "未映射到目标列，整列忽略",
            ));
            continue;
        };
        let Some(column) = target.get(target_index) else {
            issues.push(ImportIssue::new(
                None,
                Some(data.columns[source].clone()),
                format!("目标列下标 {target_index} 越界"),
            ));
            continue;
        };
        if column.is_generated {
            issues.push(ImportIssue::new(
                None,
                Some(column.meta.name.to_string()),
                "生成列不能写入",
            ));
            continue;
        }
        if mapped_targets.contains(&target_index) {
            issues.push(ImportIssue::new(
                None,
                Some(column.meta.name.to_string()),
                "同一目标列被多个源列映射",
            ));
            continue;
        }
        mapped_targets.push(target_index);
        mapping.push(MappedColumn {
            source,
            source_name: data.columns[source].clone(),
            target: target_index,
            target_name: column.meta.name.to_string(),
            logical_type: column.meta.logical_type.clone(),
            required: is_required(column),
        });
    }

    for (index, column) in target.iter().enumerate() {
        if is_required(column) && !mapped_targets.contains(&index) {
            issues.push(ImportIssue::new(
                None,
                Some(column.meta.name.to_string()),
                "必填列未映射",
            ));
        }
    }

    // 按目标列顺序生成 INSERT 列表，预览与 SQL 顺序一致。
    mapping.sort_by_key(|column| column.target);

    if mapping.is_empty() {
        issues.push(ImportIssue::new(None, None, "没有任何字段被映射"));
    }

    if !issues.is_empty() {
        return Err(ImportError::Invalid(issues));
    }

    let target_names = mapping
        .iter()
        .map(|column| ident(dialect, &column.target_name))
        .collect::<Vec<_>>()
        .join(", ");
    let table = table.to_string();
    let batch_rows = options.batch_rows.max(1);

    let mut batches = Vec::new();
    for (batch_index, chunk) in data.rows.chunks(batch_rows).enumerate() {
        let start_row = batch_index * batch_rows + 1;
        let end_row = start_row + chunk.len() - 1;
        let mut values = Vec::with_capacity(chunk.len());
        for (offset, row) in chunk.iter().enumerate() {
            let data_row = start_row + offset;
            let mut literals = Vec::with_capacity(mapping.len());
            for column in &mapping {
                let cell = row.get(column.source).cloned().unwrap_or(Cell::Null);
                let value = cell_to_value(&cell, column, data_row)
                    .map_err(|issues| ImportError::Invalid(issues))?;
                if column.required && value == Value::Null {
                    issues.push(ImportIssue::new(
                        Some(data_row),
                        Some(column.target_name.clone()),
                        "必填列不能为空",
                    ));
                    continue;
                }
                literals.push(literal(dialect, &value).map_err(|message| {
                    ImportError::Invalid(vec![ImportIssue::new(
                        Some(data_row),
                        Some(column.target_name.clone()),
                        message,
                    )])
                })?);
            }
            values.push(format!("({})", literals.join(", ")));
        }
        if !issues.is_empty() {
            return Err(ImportError::Invalid(issues));
        }
        let sql = format!(
            "INSERT INTO {table} ({target_names}) VALUES\n{};",
            values.join(",\n")
        );
        batches.push(ImportBatch {
            index: batch_index + 1,
            start_row,
            end_row,
            sql,
        });
    }

    let truncate_sql =
        (options.mode == ImportMode::Truncate).then(|| format!("DELETE FROM {table};"));

    Ok(ImportPlan {
        table,
        dialect,
        mapping,
        total_rows: data.rows.len(),
        batches,
        truncate_sql,
        warnings,
    })
}

fn cell_to_value(
    cell: &Cell,
    column: &MappedColumn,
    row: usize,
) -> Result<Value, Vec<ImportIssue>> {
    let issue = |message: String| {
        vec![ImportIssue::new(
            Some(row),
            Some(column.target_name.clone()),
            message,
        )]
    };
    let text = match cell {
        Cell::Null => return Ok(Value::Null),
        Cell::Typed(value) => return Ok(value.clone()),
        Cell::Text(text) => text,
    };
    let trimmed = text.trim();
    match &column.logical_type {
        LogicalType::Int { .. } => {
            if let Ok(value) = trimmed.parse::<i64>() {
                Ok(Value::Int(value))
            } else if let Ok(value) = trimmed.parse::<u64>() {
                Ok(Value::UInt(value))
            } else {
                Err(issue(format!("`{text}` 不是整数")))
            }
        }
        LogicalType::Float { .. } => trimmed
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(Value::Float)
            .ok_or_else(|| issue(format!("`{text}` 不是有限小数"))),
        LogicalType::Decimal { .. } => {
            let parsed = trimmed.parse::<f64>();
            if parsed.is_ok_and(|value| value.is_finite()) {
                Ok(Value::Decimal(Arc::from(trimmed)))
            } else {
                Err(issue(format!("`{text}` 不是合法定点数")))
            }
        }
        LogicalType::Bool => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" | "y" => Ok(Value::Bool(true)),
            "false" | "f" | "0" | "no" | "n" => Ok(Value::Bool(false)),
            _ => Err(issue(format!("`{text}` 不是布尔值（true/false/1/0）"))),
        },
        // 日期 / 时间 / UUID / JSON / 二进制：以文本字面量交给数据库校验。
        _ => Ok(Value::Text(Arc::from(text.as_str()))),
    }
}

// --- 样本推断与建表 ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredColumn {
    pub name: String,
    pub sql_type: String,
}

/// 按样本推断列类型（多文件建表用）。样本里全空则退回 TEXT。
pub fn infer_columns(
    dialect: DbKind,
    columns: &[String],
    rows: &[Vec<Cell>],
    sample: usize,
) -> Vec<InferredColumn> {
    let sample = if sample == 0 { usize::MAX } else { sample };
    columns
        .iter()
        .enumerate()
        .map(|(index, name)| InferredColumn {
            name: name.clone(),
            sql_type: infer_type(
                dialect,
                rows.iter()
                    .take(sample)
                    .filter_map(|row| row.get(index))
                    .collect::<Vec<_>>()
                    .as_slice(),
            ),
        })
        .collect()
}

fn infer_type(dialect: DbKind, cells: &[&Cell]) -> String {
    let texts: Vec<String> = cells
        .iter()
        .filter_map(|cell| match cell {
            Cell::Null => None,
            Cell::Typed(Value::Bool(_)) => Some("true".into()),
            other => other.text(),
        })
        .filter(|text| !text.is_empty())
        .collect();
    let any_typed_number = cells.iter().any(|cell| {
        matches!(
            cell,
            Cell::Typed(Value::Int(_) | Value::UInt(_) | Value::Float(_))
        )
    });
    let all_int = any_typed_number
        && cells.iter().all(|cell| match cell {
            Cell::Null => true,
            Cell::Typed(Value::Int(_) | Value::UInt(_)) => true,
            Cell::Text(text) => text.trim().parse::<i64>().is_ok(),
            _ => false,
        })
        || (!any_typed_number
            && !texts.is_empty()
            && texts.iter().all(|text| text.parse::<i64>().is_ok()));
    if all_int {
        return match dialect {
            DbKind::Sqlite => "INTEGER".into(),
            DbKind::Oracle => "NUMBER(19)".into(),
            _ => "BIGINT".into(),
        };
    }
    let all_number = !texts.is_empty()
        && texts
            .iter()
            .all(|text| text.parse::<f64>().is_ok_and(|value| value.is_finite()));
    if all_number {
        return match dialect {
            DbKind::Sqlite => "REAL".into(),
            DbKind::Mysql | DbKind::Mariadb => "DOUBLE".into(),
            DbKind::Mssql => "FLOAT".into(),
            DbKind::Oracle => "BINARY_DOUBLE".into(),
            _ => "DOUBLE PRECISION".into(),
        };
    }
    let all_bool = !texts.is_empty()
        && texts.iter().all(|text| {
            matches!(
                text.to_ascii_lowercase().as_str(),
                "true" | "false" | "t" | "f" | "yes" | "no"
            )
        });
    if all_bool {
        return match dialect {
            DbKind::Sqlite => "INTEGER".into(),
            DbKind::Mssql => "BIT".into(),
            DbKind::Oracle => "NUMBER(1)".into(),
            _ => "BOOLEAN".into(),
        };
    }
    let all_date = !texts.is_empty()
        && texts
            .iter()
            .all(|text| chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok());
    if all_date {
        return "DATE".into();
    }
    let all_timestamp = !texts.is_empty()
        && texts.iter().all(|text| {
            chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").is_ok()
                || chrono::DateTime::parse_from_rfc3339(text).is_ok()
        });
    if all_timestamp {
        return match dialect {
            DbKind::Mysql | DbKind::Mariadb | DbKind::Mssql => "DATETIME".into(),
            _ => "TIMESTAMP".into(),
        };
    }
    match dialect {
        DbKind::Mssql => "NVARCHAR(255)".into(),
        DbKind::Oracle => "VARCHAR2(4000)".into(),
        _ => "TEXT".into(),
    }
}

/// 多文件建表：要求列清单一致，合并样本后推断类型。
pub fn create_table_from_samples(
    dialect: DbKind,
    table: &str,
    sources: &[ParsedData],
    sample: usize,
) -> Result<String, ImportError> {
    let Some(first) = sources.first() else {
        return Err(ImportError::Parse {
            row: None,
            message: "没有来源文件".into(),
        });
    };
    for source in sources.iter().skip(1) {
        if !same_columns(&first.columns, &source.columns) {
            return Err(ImportError::Parse {
                row: None,
                message: "多个文件的列清单不一致，无法合并建表".into(),
            });
        }
    }
    let rows: Vec<Vec<Cell>> = sources
        .iter()
        .flat_map(|source| source.rows.iter().cloned())
        .collect();
    let columns = infer_columns(dialect, &first.columns, &rows, sample);
    let body = columns
        .iter()
        .map(|column| format!("  {} {}", ident(dialect, &column.name), column.sql_type))
        .collect::<Vec<_>>()
        .join(",\n");
    Ok(format!(
        "CREATE TABLE {} (\n{body}\n);",
        ident(dialect, table)
    ))
}

/// 供 UI 预览类型：把 `ColumnMeta` 转成简短类型名。
pub fn type_label(meta: &ColumnMeta) -> String {
    if meta.native_type.is_empty() {
        meta.logical_type.label()
    } else {
        meta.native_type.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(names: &[&str], nullable: bool) -> Vec<ColumnInfo> {
        names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let mut column = ColumnInfo::simple(
                    *name,
                    LogicalType::Text { max_len: None },
                    index as u32 + 1,
                );
                column.meta.nullable = nullable;
                column
            })
            .collect()
    }

    fn parse_csv(input: &str) -> ParsedData {
        parse(
            input.as_bytes(),
            &ImportOptions {
                header: true,
                ..ImportOptions::default()
            },
        )
        .expect("CSV 应解析成功")
    }

    #[test]
    fn csv_handles_quotes_crlf_and_empty_as_null() {
        let data = parse_csv("id,name,note\r\n1,\"a,b\",\r\n2,\"say \"\"hi\"\"\",x\r\n");
        assert_eq!(data.columns, vec!["id", "name", "note"]);
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.rows[0][1], Cell::Text("a,b".into()));
        assert_eq!(data.rows[0][2], Cell::Null);
        assert_eq!(data.rows[1][1], Cell::Text("say \"hi\"".into()));
    }

    #[test]
    fn csv_skip_rows_headerless_and_limit() {
        let bytes = b"# exported\n# note\n1,a\n2,b\n3,c\n";
        let data = parse(
            bytes,
            &ImportOptions {
                header: false,
                skip_rows: 2,
                max_rows: Some(2),
                ..ImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(data.columns, vec!["column_1", "column_2"]);
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.rows[1][0], Cell::Text("2".into()));
    }

    #[test]
    fn gbk_is_detected_when_utf8_fails() {
        let (gbk, _, _) = encoding_rs::GBK.encode("渠道\n自然搜索\n");
        let data = parse(
            &gbk,
            &ImportOptions {
                header: false,
                ..ImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(data.rows[0][0], Cell::Text("渠道".into()));
        assert!(data
            .warnings
            .iter()
            .any(|issue| issue.message.contains("GBK")));
    }

    #[test]
    fn json_array_of_objects_unions_keys() {
        let data = parse(
            br#"[{"id":1,"name":"a"},{"id":2,"extra":true}]"#,
            &ImportOptions {
                format: ImportFormat::Json,
                ..ImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(data.columns, vec!["id", "name", "extra"]);
        assert_eq!(data.rows[1][1], Cell::Null);
        assert_eq!(data.rows[1][2], Cell::Typed(Value::Bool(true)));
    }

    #[test]
    fn ndjson_reads_one_object_per_line() {
        let data = parse(
            b"{\"a\":1}\n\n{\"a\":2}\n",
            &ImportOptions {
                format: ImportFormat::Ndjson,
                ..ImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.rows[1][0], Cell::Typed(Value::Int(2)));
    }

    #[test]
    fn insert_literals_are_parsed_not_evaluated() {
        let sql = "INSERT INTO t (id, name, note) VALUES (1, 'a''b', NULL), (2, 'x\\\\y', TRUE);";
        let data = parse(
            sql.as_bytes(),
            &ImportOptions {
                format: ImportFormat::InsertSql,
                ..ImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(data.columns, vec!["id", "name", "note"]);
        assert_eq!(data.rows[0][1], Cell::Text("a'b".into()));
        assert_eq!(data.rows[0][2], Cell::Null);
        assert_eq!(data.rows[1][1], Cell::Text("x\\y".into()));
        assert_eq!(data.rows[1][2], Cell::Typed(Value::Bool(true)));
    }

    #[test]
    fn insert_literal_rejects_expressions() {
        let sql = "INSERT INTO t (id) VALUES (now());";
        let error = parse(
            sql.as_bytes(),
            &ImportOptions {
                format: ImportFormat::InsertSql,
                ..ImportOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("不支持"), "{error}");
    }

    #[test]
    fn required_columns_block_unmapped_import() {
        let target = columns(&["id", "name"], false);
        let data = parse_csv("name\na\n");
        let error = build_plan(
            DbKind::Sqlite,
            "\"t\"",
            &target,
            &data,
            &ImportOptions::default(),
        )
        .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("id"), "{text}");
        assert!(text.contains("必填列未映射"), "{text}");
    }

    #[test]
    fn plan_batches_by_500_and_applies_types() {
        let mut target = columns(&["id", "name"], false);
        target[0].meta.logical_type = LogicalType::Int {
            bits: 64,
            unsigned: false,
        };
        let input = (1..=3)
            .map(|index| format!("{index},n{index}\n"))
            .collect::<String>();
        let data = parse_csv(&format!("id,name\n{input}"));
        let plan = build_plan(
            DbKind::Sqlite,
            "\"t\"",
            &target,
            &data,
            &ImportOptions {
                batch_rows: 2,
                ..ImportOptions::default()
            },
        )
        .unwrap();
        assert_eq!(plan.total_rows, 3);
        assert_eq!(plan.batch_count(), 2);
        assert!(
            plan.batches[0].sql.contains("(1, 'n1')"),
            "{}",
            plan.batches[0].sql
        );
        assert_eq!(plan.batches[0].end_row, 2);
        assert_eq!(plan.batches[1].start_row, 3);
        assert!(plan.batches[1].sql.contains("(3, 'n3')"));
    }

    #[test]
    fn plan_rejects_bad_numeric_cell_with_row_number() {
        let mut target = columns(&["id"], false);
        target[0].meta.logical_type = LogicalType::Int {
            bits: 64,
            unsigned: false,
        };
        let data = parse_csv("id\n1\nabc\n");
        let error = build_plan(
            DbKind::Sqlite,
            "\"t\"",
            &target,
            &data,
            &ImportOptions::default(),
        )
        .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("第 2 行"), "{text}");
        assert!(text.contains("不是整数"), "{text}");
    }

    #[test]
    fn truncate_mode_emits_delete_first() {
        let target = columns(&["id"], true);
        let data = parse_csv("id\n1\n");
        let plan = build_plan(
            DbKind::Sqlite,
            "\"t\"",
            &target,
            &data,
            &ImportOptions {
                mode: ImportMode::Truncate,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plan.truncate_sql.as_deref(), Some("DELETE FROM \"t\";"));
    }

    #[test]
    fn explicit_mapping_skips_and_reorders_columns() {
        let mut target = columns(&["id", "name", "note"], true);
        target[0].meta.logical_type = LogicalType::Int {
            bits: 64,
            unsigned: false,
        };
        let data = parse_csv("note,id\nx,7\n");
        let plan = build_plan(
            DbKind::Sqlite,
            "\"t\"",
            &target,
            &data,
            &ImportOptions {
                mapping: vec![Some(2), Some(0)],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(plan.mapped_targets(), vec!["id", "note"]);
        assert!(
            plan.batches[0].sql.contains("(7, 'x')"),
            "{}",
            plan.batches[0].sql
        );
    }

    #[test]
    fn sample_inference_and_multi_file_create() {
        let first = parse_csv("id,amount,day\n1,1.5,2024-01-01\n2,2,2024-01-02\n");
        let second = parse_csv("id,amount,day\n3,3.25,2024-01-03\n");
        let sql = create_table_from_samples(DbKind::Sqlite, "t", &[first, second], 100).unwrap();
        assert!(sql.contains("\"id\" INTEGER"), "{sql}");
        assert!(sql.contains("\"amount\" REAL"), "{sql}");
        assert!(sql.contains("\"day\" DATE"), "{sql}");
    }

    #[test]
    fn xlsx_is_explicitly_unsupported() {
        let error = parse(
            b"whatever",
            &ImportOptions {
                format: ImportFormat::Xlsx,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("calamine"), "{error}");
    }

    #[test]
    fn quoted_newline_keeps_record_together() {
        let data = parse_csv("id,note\n1,\"line1\nline2\"\n2,ok\n");
        assert_eq!(data.rows.len(), 2);
        assert_eq!(data.rows[0][1], Cell::Text("line1\nline2".into()));
        assert_eq!(data.rows[1][0], Cell::Text("2".into()));
    }
}
