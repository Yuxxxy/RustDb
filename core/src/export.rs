//! 结果集导出与复制格式（F-72）。
//!
//! 设计口径（需求文档 19.3）：**格式编码集中在这里，UI 不拼 CSV/JSON**。
//!
//! 支持：TSV / CSV / JSON / Markdown / SQL INSERT / SQL UPDATE。
//! * 流式：按行写入 `io::Write`，不要求调用方先把 100 万行拼成一个字符串；
//! * NULL 与空串严格区分（NULL 走 `null_text`，默认空字段；JSON 为 `null`）；
//! * **公式防护**：只对「以 `= @ TAB CR` 开头，或以 `+ -` 开头却不是数字字面量」的文本
//!   加前导单引号；`-5` / `-1.2e3` 这类真实负数**不改写**（R-23）；
//! * SQL 批量按「行数 + 字节」双上限切分，避免超出 `max_allowed_packet`；
//! * 单格复制保持原文逐字节一致（[`cell_raw`]）。

use crate::edit::{ident, literal};
use rustdb_db_api::{DbKind, Value};
use std::fmt;
use std::io::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Tsv,
    Csv,
    Json,
    Markdown,
    SqlInsert,
    SqlUpdate,
    /// Excel 工作簿（二进制；超单表上限自动拆 sheet）。
    Xlsx,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 7] = [
        Self::Csv,
        Self::Tsv,
        Self::Json,
        Self::Markdown,
        Self::SqlInsert,
        Self::SqlUpdate,
        Self::Xlsx,
    ];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Tsv => "tsv",
            Self::Csv => "csv",
            Self::Json => "json",
            Self::Markdown => "md",
            Self::SqlInsert | Self::SqlUpdate => "sql",
            Self::Xlsx => "xlsx",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Tsv => "TSV",
            Self::Csv => "CSV",
            Self::Json => "JSON",
            Self::Markdown => "Markdown",
            Self::SqlInsert => "SQL INSERT",
            Self::SqlUpdate => "SQL UPDATE",
            Self::Xlsx => "Excel",
        }
    }

    pub fn is_sql(self) -> bool {
        matches!(self, Self::SqlInsert | Self::SqlUpdate)
    }

    /// 二进制格式：不能走 `encode()`，要用 `encode_bytes()` / `write_rows()`。
    pub fn is_binary(self) -> bool {
        matches!(self, Self::Xlsx)
    }
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// 标识符引用与字面量格式，只影响 SQL 格式。
    pub dialect: DbKind,
    /// SQL 导出必需；可带 `schema.table`。
    pub table: Option<String>,
    pub include_header: bool,
    /// NULL 在 CSV/TSV/Markdown 里显示成什么（默认空字段）。
    pub null_text: String,
    pub formula_guard: bool,
    /// CSV 是否写 UTF-8 BOM（Excel 双击不乱码）。
    pub excel_bom: bool,
    pub line_ending: String,
    /// SQL UPDATE 的行标识列下标；空则拒绝生成 UPDATE。
    pub key_columns: Vec<usize>,
    pub sql_batch_rows: usize,
    pub sql_batch_bytes: usize,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            dialect: DbKind::Postgres,
            table: None,
            include_header: true,
            null_text: String::new(),
            formula_guard: true,
            excel_bom: false,
            line_ending: "\r\n".to_string(),
            key_columns: Vec::new(),
            sql_batch_rows: 500,
            sql_batch_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub enum ExportError {
    Io(io::Error),
    /// 生成 SQL 失败（缺表名 / 缺主键 / 值无法按方言表达）。
    Sql(String),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "写文件失败：{e}"),
            Self::Sql(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<io::Error> for ExportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// 便捷入口：编码成字符串（文本格式 / 复制到剪贴板）。
pub fn encode(
    format: ExportFormat,
    columns: &[String],
    rows: &[Vec<Value>],
    options: &ExportOptions,
) -> Result<String, ExportError> {
    if format.is_binary() {
        return Err(ExportError::Sql(
            "XLSX 是二进制格式，请使用 encode_bytes / write_to".into(),
        ));
    }
    let bytes = encode_bytes(format, columns, rows, options)?;
    String::from_utf8(bytes).map_err(|e| ExportError::Sql(format!("编码结果不是 UTF-8：{e}")))
}

/// 二进制安全入口（XLSX 用）：同样是小结果集的便捷封装。
pub fn encode_bytes(
    format: ExportFormat,
    columns: &[String],
    rows: &[Vec<Value>],
    options: &ExportOptions,
) -> Result<Vec<u8>, ExportError> {
    let mut buf: Vec<u8> = Vec::new();
    write_to(&mut buf, format, columns, rows, options)?;
    Ok(buf)
}

/// 流式写出（切片入口）：调用方已经持有全部行。
pub fn write_to<W: Write>(
    w: &mut W,
    format: ExportFormat,
    columns: &[String],
    rows: &[Vec<Value>],
    options: &ExportOptions,
) -> Result<(), ExportError> {
    write_rows(w, format, columns, rows.iter().cloned(), options)
}

/// 流式写出（迭代器入口）：从 `ResultStore` 分页拉取时用这个，
/// 内存占用与总行数无关（R-05 / 19.3「流式写盘」）。
pub fn write_rows<W, I>(
    w: &mut W,
    format: ExportFormat,
    columns: &[String],
    rows: I,
    options: &ExportOptions,
) -> Result<(), ExportError>
where
    W: Write,
    I: IntoIterator<Item = Vec<Value>>,
{
    match format {
        ExportFormat::Csv | ExportFormat::Tsv => {
            write_delimited(w, format, columns, rows, options)?
        }
        ExportFormat::Markdown => write_markdown(w, columns, rows, options)?,
        ExportFormat::Json => write_json(w, columns, rows)?,
        ExportFormat::SqlInsert | ExportFormat::SqlUpdate => {
            write_sql(w, format, columns, rows, options)?
        }
        ExportFormat::Xlsx => write_xlsx(w, columns, rows)?,
    }
    Ok(())
}

// --- 文本类格式 ---

fn write_delimited<W, I>(
    w: &mut W,
    format: ExportFormat,
    columns: &[String],
    rows: I,
    options: &ExportOptions,
) -> io::Result<()>
where
    W: Write,
    I: IntoIterator<Item = Vec<Value>>,
{
    let delimiter = if format == ExportFormat::Tsv {
        '\t'
    } else {
        ','
    };
    if format == ExportFormat::Csv && options.excel_bom {
        w.write_all("\u{feff}".as_bytes())?;
    }
    if options.include_header {
        let line = columns
            .iter()
            .map(|c| escape_field(c, delimiter))
            .collect::<Vec<_>>()
            .join(&delimiter.to_string());
        write!(w, "{line}{}", options.line_ending)?;
    }
    for row in rows {
        let mut line = String::new();
        for (index, value) in row.iter().enumerate() {
            if index > 0 {
                line.push(delimiter);
            }
            let text = cell_text(value, options);
            let text = if options.formula_guard {
                guard_formula(&text)
            } else {
                text
            };
            line.push_str(&escape_field(&text, delimiter));
        }
        write!(w, "{line}{}", options.line_ending)?;
    }
    Ok(())
}

fn write_markdown<W, I>(
    w: &mut W,
    columns: &[String],
    rows: I,
    options: &ExportOptions,
) -> io::Result<()>
where
    W: Write,
    I: IntoIterator<Item = Vec<Value>>,
{
    let esc = |s: &str| s.replace('|', "\\|").replace(['\n', '\r'], "<br>");
    if options.include_header {
        let head = columns
            .iter()
            .map(|c| esc(c))
            .collect::<Vec<_>>()
            .join(" | ");
        writeln!(w, "| {head} |")?;
        let sep = columns
            .iter()
            .map(|_| "---")
            .collect::<Vec<_>>()
            .join(" | ");
        writeln!(w, "| {sep} |")?;
    }
    for row in rows {
        let cells = row
            .iter()
            .map(|v| esc(&cell_text(v, options)))
            .collect::<Vec<_>>()
            .join(" | ");
        writeln!(w, "| {cells} |")?;
    }
    Ok(())
}

/// 逐行写 JSON 数组：不把整份结果拼成一个大 `Value`。
fn write_json<W, I>(w: &mut W, columns: &[String], rows: I) -> io::Result<()>
where
    W: Write,
    I: IntoIterator<Item = Vec<Value>>,
{
    w.write_all(b"[")?;
    let mut first = true;
    for row in rows {
        if !first {
            w.write_all(b",")?;
        }
        first = false;
        let object = row_object(columns, &row);
        serde_json::to_writer(&mut *w, &object).map_err(io::Error::other)?;
    }
    w.write_all(b"]")?;
    Ok(())
}

fn row_object(columns: &[String], row: &[Value]) -> serde_json::Value {
    let mut object = serde_json::Map::with_capacity(columns.len());
    for (index, column) in columns.iter().enumerate() {
        let value = row
            .get(index)
            .map(json_value)
            .unwrap_or(serde_json::Value::Null);
        object.insert(column.clone(), value);
    }
    serde_json::Value::Object(object)
}

// --- Excel ---

/// Excel 单表数据行上限：工作表 1,048,576 行，第 1 行留给表头。
pub const XLSX_MAX_DATA_ROWS: usize = 1_048_575;

/// 写 XLSX：`rust_xlsxwriter` 没有流式写入器（整本工作簿在内存），
/// 因此超出单表上限时**自动拆 sheet**，而不是报错或静默截断（F-72）。
fn write_xlsx<W, I>(w: &mut W, columns: &[String], rows: I) -> Result<(), ExportError>
where
    W: Write,
    I: IntoIterator<Item = Vec<Value>>,
{
    use rust_xlsxwriter::{Format, Workbook};

    let header_format = Format::new().set_bold();
    let mut workbook = Workbook::new();
    let mut rows = rows.into_iter().peekable();
    let mut sheet = 0usize;
    loop {
        let worksheet = workbook.add_worksheet();
        for (col, name) in columns.iter().enumerate() {
            let col = xlsx_col(col)?;
            worksheet
                .write_string_with_format(0, col, name, &header_format)
                .map_err(xlsx_error)?;
        }
        let mut written = 0usize;
        while written < XLSX_MAX_DATA_ROWS {
            let Some(row) = rows.next() else { break };
            let excel_row = (written + 1) as u32;
            for (col, value) in row.iter().enumerate() {
                write_xlsx_cell(worksheet, excel_row, col, value)?;
            }
            written += 1;
        }
        sheet += 1;
        if rows.peek().is_none() {
            break;
        }
        log::debug!("XLSX 超过单表上限，已拆到第 {} 个工作表", sheet + 1);
    }
    let buffer = workbook.save_to_buffer().map_err(xlsx_error)?;
    w.write_all(&buffer)?;
    Ok(())
}

fn write_xlsx_cell(
    worksheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: usize,
    value: &Value,
) -> Result<(), ExportError> {
    use rust_xlsxwriter::Format;

    let column = xlsx_col(col)?;
    let result = match value {
        Value::Null => worksheet
            .write_blank(row, column, &Format::new())
            .map(|_| ()),
        Value::Bool(v) => worksheet.write_boolean(row, column, *v).map(|_| ()),
        Value::Int(v) => worksheet.write_number(row, column, *v as f64).map(|_| ()),
        Value::UInt(v) => worksheet.write_number(row, column, *v as f64).map(|_| ()),
        Value::Float(v) if v.is_finite() => worksheet.write_number(row, column, *v).map(|_| ()),
        // 定点数优先按数字写；解析不了就退回文本，绝不丢原值。
        Value::Decimal(text) => match text.parse::<f64>() {
            Ok(number) if number.is_finite() => {
                worksheet.write_number(row, column, number).map(|_| ())
            }
            _ => worksheet
                .write_string(row, column, text.as_ref())
                .map(|_| ()),
        },
        other => {
            let text = other.export_text();
            worksheet
                .write_string(row, column, text.as_str())
                .map(|_| ())
        }
    };
    result.map_err(xlsx_error)
}

fn xlsx_col(index: usize) -> Result<u16, ExportError> {
    u16::try_from(index)
        .ok()
        .filter(|col| *col < 16_384)
        .ok_or_else(|| {
            ExportError::Sql(format!("列数超过 Excel 上限（16384）：第 {} 列", index + 1))
        })
}

fn xlsx_error(error: rust_xlsxwriter::XlsxError) -> ExportError {
    ExportError::Sql(format!("生成 XLSX 失败：{error}"))
}

// --- SQL 格式 ---

fn write_sql<W, I>(
    w: &mut W,
    format: ExportFormat,
    columns: &[String],
    rows: I,
    options: &ExportOptions,
) -> Result<(), ExportError>
where
    W: Write,
    I: IntoIterator<Item = Vec<Value>>,
{
    let Some(table) = options.table.as_deref() else {
        return Err(ExportError::Sql("SQL 导出需要目标表名".into()));
    };
    let table = qualified_ident(options.dialect, table);
    if format == ExportFormat::SqlUpdate {
        if options.key_columns.is_empty() {
            return Err(ExportError::Sql(
                "SQL UPDATE 导出需要指定行标识列（主键或唯一键）".into(),
            ));
        }
        for key in &options.key_columns {
            if *key >= columns.len() {
                return Err(ExportError::Sql(format!("行标识列下标 {key} 越界")));
            }
        }
    }

    if format == ExportFormat::SqlUpdate {
        for row in rows {
            write_update(w, &table, columns, &row, options)?;
        }
        return Ok(());
    }

    // INSERT：按「行数 + 字节」双上限攒批，边攒边写。
    let max_rows = options.sql_batch_rows.max(1);
    let max_bytes = options.sql_batch_bytes.max(1);
    let mut batch: Vec<Vec<Value>> = Vec::new();
    let mut bytes = 0usize;
    for row in rows {
        bytes += row.iter().map(|v| v.export_text().len() + 8).sum::<usize>();
        batch.push(row);
        if batch.len() >= max_rows || bytes >= max_bytes {
            write_insert(w, &table, columns, &batch, options)?;
            batch.clear();
            bytes = 0;
        }
    }
    if !batch.is_empty() {
        write_insert(w, &table, columns, &batch, options)?;
    }
    Ok(())
}

fn write_insert<W: Write>(
    w: &mut W,
    table: &str,
    columns: &[String],
    rows: &[Vec<Value>],
    options: &ExportOptions,
) -> Result<(), ExportError> {
    let cols = columns
        .iter()
        .map(|c| ident(options.dialect, c))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(w, "INSERT INTO {table} ({cols}) VALUES")?;
    for (index, row) in rows.iter().enumerate() {
        let values = row
            .iter()
            .map(|v| literal(options.dialect, v).map_err(ExportError::Sql))
            .collect::<Result<Vec<_>, _>>()?
            .join(", ");
        let comma = if index + 1 == rows.len() { ";" } else { "," };
        writeln!(w, "({values}){comma}")?;
    }
    Ok(())
}

fn write_update<W: Write>(
    w: &mut W,
    table: &str,
    columns: &[String],
    row: &[Value],
    options: &ExportOptions,
) -> Result<(), ExportError> {
    let mut sets = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        if options.key_columns.contains(&index) {
            continue;
        }
        let value = row.get(index).unwrap_or(&Value::Null);
        let text = literal(options.dialect, value).map_err(ExportError::Sql)?;
        sets.push(format!("{} = {text}", ident(options.dialect, column)));
    }
    if sets.is_empty() {
        return Err(ExportError::Sql(
            "SQL UPDATE 导出没有可更新的列（除行标识列外为空）".into(),
        ));
    }
    let mut wheres = Vec::new();
    for key in &options.key_columns {
        let column = ident(options.dialect, &columns[*key]);
        match row.get(*key).unwrap_or(&Value::Null) {
            Value::Null => wheres.push(format!("{column} IS NULL")),
            value => wheres.push(format!(
                "{column} = {}",
                literal(options.dialect, value).map_err(ExportError::Sql)?
            )),
        }
    }
    writeln!(
        w,
        "UPDATE {table} SET {} WHERE {};",
        sets.join(", "),
        wheres.join(" AND ")
    )?;
    Ok(())
}

fn qualified_ident(dialect: DbKind, name: &str) -> String {
    name.split('.')
        .map(|part| ident(dialect, part.trim()))
        .collect::<Vec<_>>()
        .join(".")
}

// --- 单元格与转义 ---

/// 文本口径的单元格：NULL 用 `null_text`，其余不截断。
pub fn cell_text(value: &Value, options: &ExportOptions) -> String {
    match value {
        Value::Null => options.null_text.clone(),
        other => other.export_text(),
    }
}

/// 单格原文（复制用）：Text 逐字节一致；NULL 为空；其余用导出文本。
pub fn cell_raw(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => other.export_text(),
    }
}

/// RFC 4180 风格转义：含分隔符 / 引号 / 换行时整体加引号，内部引号翻倍。
pub fn escape_field(field: &str, delimiter: char) -> String {
    if field.contains(delimiter)
        || field.contains('"')
        || field.contains('\n')
        || field.contains('\r')
    {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// 是否需要公式防护（CSV/TSV → 表格软件会当公式执行）。
///
/// 只加前导单引号；**数字字面量不改写**，否则 `-5` 会变成文本左对齐。
pub fn needs_formula_guard(text: &str) -> bool {
    let Some(first) = text.chars().next() else {
        return false;
    };
    match first {
        '=' | '@' | '\t' | '\r' => true,
        '+' | '-' => !rustdb_sql_lang::params::is_number_literal(text),
        _ => false,
    }
}

pub fn guard_formula(text: &str) -> String {
    if needs_formula_guard(text) {
        format!("'{text}")
    } else {
        text.to_string()
    }
}

fn json_value(value: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match value {
        Value::Null => J::Null,
        Value::Bool(v) => J::Bool(*v),
        Value::Int(v) => J::from(*v),
        Value::UInt(v) => J::from(*v),
        Value::Float(v) => serde_json::Number::from_f64(*v)
            .map(J::Number)
            .unwrap_or_else(|| J::String(v.to_string())),
        // 定点数保留原始字符串精度；能被 JSON 数字无损表达才转数字。
        Value::Decimal(text) => text
            .parse::<serde_json::Number>()
            .map(J::Number)
            .unwrap_or_else(|_| J::String(text.to_string())),
        Value::Text(text) | Value::Json(text) | Value::Unknown { text, .. } => {
            J::String(text.to_string())
        }
        Value::Bytes(bytes) => {
            let mut text = String::with_capacity(bytes.len() * 2 + 2);
            text.push_str("0x");
            for byte in bytes.iter() {
                text.push_str(&format!("{byte:02x}"));
            }
            J::String(text)
        }
        Value::Date(date) => J::String(date.to_string()),
        Value::Time(time) => J::String(time.to_string()),
        Value::Timestamp(ts) => J::String(ts.to_string()),
        Value::TimestampTz(ts) => J::String(ts.to_rfc3339()),
        Value::Uuid(bytes) => J::String(crate::edit::uuid_text(bytes)),
        Value::Array(items) => J::Array(items.iter().map(json_value).collect()),
        Value::Omitted { total_bytes, .. } => J::String(format!("<omitted {total_bytes} bytes>")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn text(v: &str) -> Value {
        Value::Text(Arc::from(v))
    }

    fn cols() -> Vec<String> {
        vec!["id".into(), "name".into(), "note".into()]
    }

    fn rows() -> Vec<Vec<Value>> {
        vec![
            vec![Value::Int(1), text("自然搜索"), Value::Null],
            vec![Value::Int(2), text("a,b"), text("say \"hi\"")],
        ]
    }

    #[test]
    fn csv_has_bom_crlf_and_empty_null() {
        let mut options = ExportOptions {
            excel_bom: true,
            ..ExportOptions::default()
        };
        options.dialect = DbKind::Postgres;
        let csv = encode(ExportFormat::Csv, &cols(), &rows(), &options).unwrap();
        assert!(csv.starts_with('\u{feff}'));
        assert!(csv.contains("id,name,note\r\n"));
        assert!(csv.contains("1,自然搜索,\r\n"), "{csv}");
        assert!(csv.contains("\"a,b\",\"say \"\"hi\"\"\""), "{csv}");
    }

    #[test]
    fn null_text_is_configurable() {
        let options = ExportOptions {
            null_text: "\\N".into(),
            ..ExportOptions::default()
        };
        let tsv = encode(ExportFormat::Tsv, &cols(), &rows(), &options).unwrap();
        assert!(tsv.contains("1\t自然搜索\t\\N"), "{tsv}");
    }

    #[test]
    fn formula_guard_spares_real_numbers() {
        for safe in ["-5", "-1.2e3", "+7", "12", "abc", ""] {
            assert!(!needs_formula_guard(safe), "{safe} 不应被防护");
        }
        for danger in ["=1+1", "@user", "-5+1", "\tX", "\rX", "+A1"] {
            assert!(needs_formula_guard(danger), "{danger} 应被防护");
        }
        assert_eq!(guard_formula("=1+1"), "'=1+1");
        assert_eq!(guard_formula("-5"), "-5");
    }

    #[test]
    fn csv_applies_guard_only_to_dangerous_cells() {
        let rows = vec![vec![Value::Int(1), text("=cmd"), Value::Int(-42)]];
        let csv = encode(
            ExportFormat::Csv,
            &["a".into(), "b".into(), "c".into()],
            &rows,
            &ExportOptions::default(),
        )
        .unwrap();
        assert!(csv.contains("'=cmd"), "{csv}");
        assert!(csv.contains("-42"), "真实负数不能被改写：{csv}");
    }

    #[test]
    fn json_keeps_types_and_null() {
        let json = encode(
            ExportFormat::Json,
            &cols(),
            &rows(),
            &ExportOptions::default(),
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["id"], 1);
        assert_eq!(parsed[0]["note"], serde_json::Value::Null);
        assert_eq!(parsed[1]["name"], "a,b");
    }

    #[test]
    fn markdown_escapes_pipes_and_newlines() {
        let rows = vec![vec![text("a|b"), text("l1\nl2"), Value::Null]];
        let md = encode(
            ExportFormat::Markdown,
            &["x".into(), "y".into(), "z".into()],
            &rows,
            &ExportOptions::default(),
        )
        .unwrap();
        assert!(md.contains("a\\|b"), "{md}");
        assert!(md.contains("l1<br>l2"), "{md}");
    }

    #[test]
    fn sql_insert_batches_by_row_count() {
        let rows: Vec<Vec<Value>> = (1..=3)
            .map(|i| vec![Value::Int(i), text("n"), Value::Null])
            .collect();
        let options = ExportOptions {
            dialect: DbKind::Postgres,
            table: Some("public.orders".into()),
            sql_batch_rows: 2,
            ..ExportOptions::default()
        };
        let sql = encode(ExportFormat::SqlInsert, &cols(), &rows, &options).unwrap();
        assert!(
            sql.contains("INSERT INTO \"public\".\"orders\" (\"id\", \"name\", \"note\") VALUES"),
            "{sql}"
        );
        assert_eq!(
            sql.matches("INSERT INTO").count(),
            2,
            "3 行按 2 行一批 = 2 条语句：{sql}"
        );
        assert!(sql.contains("NULL"), "{sql}");
    }

    #[test]
    fn sql_update_requires_key_and_skips_it_in_set() {
        let rows = vec![vec![Value::Int(1), text("n"), Value::Null]];
        let mut options = ExportOptions {
            dialect: DbKind::Mysql,
            table: Some("orders".into()),
            ..ExportOptions::default()
        };
        let err = encode(ExportFormat::SqlUpdate, &cols(), &rows, &options).unwrap_err();
        assert!(err.to_string().contains("行标识列"), "{err}");

        options.key_columns = vec![0];
        let sql = encode(ExportFormat::SqlUpdate, &cols(), &rows, &options).unwrap();
        assert!(
            sql.starts_with("UPDATE `orders` SET `name` = 'n', `note` = NULL WHERE `id` = 1;"),
            "{sql}"
        );
    }

    #[test]
    fn single_cell_copy_is_byte_exact() {
        let value = text("a\tb \"q\"\nline");
        assert_eq!(cell_raw(&value), "a\tb \"q\"\nline");
        assert_eq!(cell_raw(&Value::Null), "");
    }

    #[test]
    fn writing_to_a_writer_matches_encode() {
        let options = ExportOptions::default();
        let mut buf = Vec::new();
        write_to(&mut buf, ExportFormat::Csv, &cols(), &rows(), &options).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            encode(ExportFormat::Csv, &cols(), &rows(), &options).unwrap()
        );
    }

    #[test]
    fn xlsx_is_a_zip_with_a_worksheet() {
        let bytes = encode_bytes(
            ExportFormat::Xlsx,
            &cols(),
            &rows(),
            &ExportOptions::default(),
        )
        .unwrap();
        assert!(bytes.starts_with(b"PK"), "XLSX 应是 zip 容器");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("xl/worksheets/sheet1.xml"),
            "缺少工作表部件：{text}"
        );
    }

    #[test]
    fn text_encoder_rejects_binary_format() {
        let error = encode(
            ExportFormat::Xlsx,
            &cols(),
            &rows(),
            &ExportOptions::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("二进制"), "{error}");
    }

    #[test]
    fn xlsx_column_limit_matches_excel() {
        assert!(xlsx_col(16_383).is_ok());
        assert!(xlsx_col(16_384).is_err());
        assert_eq!(XLSX_MAX_DATA_ROWS, 1_048_575);
    }
}
