//! F-87 数据库文档生成（Markdown / DBML）。
//!
//! 边界（需求 19.4）：只读生成，不写数据库；本地笔记与库注释分离，笔记优先；
//! 导出走原子写；缺元数据必须显式警告；DBML 只表达表 / 列 / 索引 / 外键关系，
//! 不承诺跨方言类型等价（那是 F-86 的职责）。

use rustdb_db_api::{ColumnInfo, Namespace, ObjectPath, ObjectSummary, TableDetail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

/// 单次生成最多汇总的表数；超出部分只给警告，不静默截断。
pub const MAX_DOC_TABLES: usize = 300;

/// 一张表在文档输入里的形态：摘要必有，详情可能取回失败。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocTable {
    pub summary: ObjectSummary,
    pub detail: Option<TableDetail>,
    /// 详情取回失败的原因；`None` 表示已取回。
    #[serde(default)]
    pub error: Option<String>,
}

/// 文档生成的输入（由 core 从元数据接口汇总，UI 只读消费）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DocInput {
    /// 命名空间展示名（`库` 或 `库.schema`）。
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub tables: Vec<DocTable>,
    /// 超过 [`MAX_DOC_TABLES`] 而未汇总的表数。
    #[serde(default)]
    pub skipped: usize,
}

/// 单表本地笔记；列笔记按列名（不区分大小写）索引。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableNote {
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub columns: BTreeMap<String, String>,
}

impl TableNote {
    fn is_empty(&self) -> bool {
        self.note.trim().is_empty() && self.columns.is_empty()
    }
}

/// 一个连接的文档笔记：项目级 + 表级 + 列级。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DocNotes {
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub tables: BTreeMap<String, TableNote>,
}

impl DocNotes {
    /// 表键：`库展示名|表名`，两侧都小写，避免大小写差异造成笔记失配。
    pub fn table_note(&self, path: &ObjectPath) -> Option<&str> {
        self.tables
            .get(&table_key(path))
            .map(|table| table.note.as_str())
            .filter(|note| !note.trim().is_empty())
    }

    pub fn column_note(&self, path: &ObjectPath, column: &str) -> Option<&str> {
        self.tables
            .get(&table_key(path))
            .and_then(|table| table.columns.get(&column.to_lowercase()))
            .map(String::as_str)
            .filter(|note| !note.trim().is_empty())
    }

    pub fn set_project(&mut self, note: &str) {
        self.project = note.trim().to_string();
    }

    pub fn set_table_note(&mut self, path: &ObjectPath, note: &str) {
        let key = table_key(path);
        let entry = self.tables.entry(key.clone()).or_default();
        entry.note = note.trim().to_string();
        if entry.is_empty() {
            self.tables.remove(&key);
        }
    }

    pub fn set_column_note(&mut self, path: &ObjectPath, column: &str, note: &str) {
        let key = table_key(path);
        let entry = self.tables.entry(key.clone()).or_default();
        let column = column.to_lowercase();
        let note = note.trim();
        if note.is_empty() {
            entry.columns.remove(&column);
        } else {
            entry.columns.insert(column, note.to_string());
        }
        if entry.is_empty() {
            self.tables.remove(&key);
        }
    }
}

/// 笔记键：与 `DocNotes` 内部一致，供持久化与 UI 复用。
pub fn table_key(path: &ObjectPath) -> String {
    format!(
        "{}|{}",
        path.ns.display_label().to_lowercase(),
        path.name.to_lowercase()
    )
}

/// 按连接配置 id 保存的笔记集合。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DocNoteStore {
    #[serde(default)]
    pub by_profile: BTreeMap<String, DocNotes>,
}

impl DocNoteStore {
    pub fn notes(&self, profile: &str) -> DocNotes {
        self.by_profile.get(profile).cloned().unwrap_or_default()
    }

    pub fn set(&mut self, profile: &str, notes: DocNotes) {
        self.by_profile.insert(profile.to_string(), notes);
    }
}

/// 生成的文档（两份文本 + 警告）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Documentation {
    pub markdown: String,
    pub dbml: String,
    pub warnings: Vec<String>,
}

/// 汇总表 / 列 / 索引 / 外键，生成 Markdown 与 DBML。
pub fn generate(input: &DocInput, notes: &DocNotes) -> Documentation {
    let tables = sorted_tables(input);
    let warnings = collect_warnings(input, notes, &tables);
    let markdown = render_markdown(input, notes, &tables, &warnings);
    let dbml = render_dbml(input, notes, &tables, &warnings);
    Documentation {
        markdown,
        dbml,
        warnings,
    }
}

fn sorted_tables(input: &DocInput) -> Vec<&DocTable> {
    let mut tables: Vec<&DocTable> = input.tables.iter().collect();
    tables.sort_by(|a, b| {
        a.summary
            .path
            .display_label()
            .to_lowercase()
            .cmp(&b.summary.path.display_label().to_lowercase())
    });
    tables
}

fn known_tables(tables: &[&DocTable]) -> BTreeSet<String> {
    tables
        .iter()
        .map(|table| table_key(&table.summary.path))
        .collect()
}

/// 外键目标是否在本次范围内：带 schema 的比较限定名，纯表名只比表名。
fn target_in_scope(tables: &[&DocTable], ns: &Namespace, referenced: &str) -> bool {
    if !referenced.contains('.') {
        return tables.iter().any(|table| {
            table
                .summary
                .path
                .name
                .eq_ignore_ascii_case(referenced.trim())
        });
    }
    let (schema, name) = split_reference(ns, referenced);
    let qualified = format!(
        "{}.{}",
        schema.as_deref().unwrap_or("").to_lowercase(),
        name.to_lowercase()
    );
    tables.iter().any(|table| {
        let path = &table.summary.path;
        let label = path.ns.display_label().to_lowercase();
        let mut candidates = vec![format!("{label}.{}", path.name.to_lowercase())];
        if let Some(schema) = &path.ns.schema {
            candidates.push(format!(
                "{}.{}",
                schema.to_lowercase(),
                path.name.to_lowercase()
            ));
        }
        candidates.contains(&qualified)
    })
}

fn collect_warnings(input: &DocInput, notes: &DocNotes, tables: &[&DocTable]) -> Vec<String> {
    let mut warnings = Vec::new();
    if tables.is_empty() {
        warnings.push("没有可生成文档的表（对象列表为空或尚未加载）".into());
    }
    if input.skipped > 0 {
        warnings.push(format!(
            "表数量超过上限 {MAX_DOC_TABLES}，已跳过 {} 张；文档只覆盖前 {MAX_DOC_TABLES} 张",
            input.skipped
        ));
    }
    let known = known_tables(tables);
    for table in tables {
        let name = table.summary.path.display_label();
        match &table.detail {
            None => warnings.push(format!(
                "表 {name} 的列 / 索引 / 外键未取回：{}（只输出表级信息）",
                table.error.as_deref().unwrap_or("驱动未返回元数据")
            )),
            Some(detail) if detail.columns.is_empty() => {
                warnings.push(format!("表 {name} 没有列信息（驱动可能未提供）"))
            }
            Some(_) => {}
        }
    }
    for table in tables {
        let Some(detail) = &table.detail else {
            continue;
        };
        for fk in &detail.foreign_keys {
            if !target_in_scope(tables, &table.summary.path.ns, &fk.referenced_table) {
                warnings.push(format!(
                    "表 {} 的外键 {} 指向 {}，不在本文档范围内（跨 schema 或未加载）",
                    table.summary.path.display_label(),
                    fk.name,
                    fk.referenced_table
                ));
            }
        }
    }
    for key in notes.tables.keys() {
        if known.contains(key) {
            continue;
        }
        warnings.push(format!("表笔记「{key}」未匹配任何已收录表（已忽略）"));
    }
    for table in tables {
        let Some(note) = notes.tables.get(&table_key(&table.summary.path)) else {
            continue;
        };
        let columns: BTreeSet<String> = table
            .detail
            .iter()
            .flat_map(|detail| detail.columns.iter())
            .map(|column| column.meta.name.to_lowercase())
            .collect();
        for column in note.columns.keys() {
            if !columns.contains(column) {
                warnings.push(format!(
                    "表 {} 的列笔记引用了不存在的列 {}（已忽略）",
                    table.summary.path.display_label(),
                    column
                ));
            }
        }
    }
    warnings
}

/// 把驱动返回的引用表名拆成 `(schema, table)`：去掉与当前库相同的 catalog，
/// 纯表名回落到当前 schema；三段以上时只保留最后两段（catalog.schema.table）。
fn split_reference(ns: &Namespace, referenced: &str) -> (Option<String>, String) {
    let mut parts: Vec<&str> = referenced
        .split('.')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() > 1 {
        if parts[0].eq_ignore_ascii_case(ns.catalog.as_deref().unwrap_or("")) {
            parts.remove(0);
        }
    }
    match parts.len() {
        0 => (None, referenced.trim().to_string()),
        1 => (
            ns.schema.as_deref().map(str::to_string),
            parts[0].to_string(),
        ),
        _ => {
            let table = parts.pop().unwrap_or_default().to_string();
            (Some(parts.join(".")), table)
        }
    }
}

fn table_comment(table: &DocTable) -> Option<&str> {
    table
        .detail
        .as_ref()
        .and_then(|detail| detail.comment.as_deref())
        .or(table.summary.comment.as_deref())
        .filter(|comment| !comment.trim().is_empty())
}

fn column_type(column: &ColumnInfo) -> String {
    let native = column.meta.native_type.trim();
    if native.is_empty() {
        column.meta.logical_type.label()
    } else {
        native.to_string()
    }
}

fn render_markdown(
    input: &DocInput,
    notes: &DocNotes,
    tables: &[&DocTable],
    warnings: &[String],
) -> String {
    let mut out = String::new();
    let database = if input.database.trim().is_empty() {
        "默认库"
    } else {
        input.database.trim()
    };
    out.push_str(&format!("# 数据库文档：{database}\n\n"));
    out.push_str("> 由 RustDb 只读生成；不含数据，也不是一致性快照。\n\n");
    if !notes.project.trim().is_empty() {
        out.push_str("## 项目说明\n\n");
        out.push_str(notes.project.trim());
        out.push_str("\n\n");
    }
    out.push_str("## 概览\n\n");
    let (mut columns, mut indexes, mut fks) = (0usize, 0usize, 0usize);
    for table in tables {
        if let Some(detail) = &table.detail {
            columns += detail.columns.len();
            indexes += detail.indexes.len();
            fks += detail.foreign_keys.len();
        }
    }
    out.push_str(&format!("- 表：{}\n", tables.len()));
    out.push_str(&format!("- 列：{columns}\n"));
    out.push_str(&format!("- 索引：{indexes}\n"));
    out.push_str(&format!("- 外键：{fks}\n\n"));

    for table in tables {
        render_table_markdown(&mut out, table, notes);
    }
    render_relations_markdown(&mut out, tables);
    if !warnings.is_empty() {
        out.push_str("## 元数据警告\n\n");
        for warning in warnings {
            out.push_str(&format!("- {warning}\n"));
        }
        out.push('\n');
    }
    out
}

fn render_table_markdown(out: &mut String, table: &DocTable, notes: &DocNotes) {
    let path = &table.summary.path;
    out.push_str(&format!("## {}\n\n", path.display_label()));
    if let Some(comment) = table_comment(table) {
        out.push_str(&format!("**库注释**：{}\n\n", md_note(comment)));
    }
    if let Some(note) = notes.table_note(path) {
        out.push_str(&format!("**本地笔记**：{}\n\n", md_note(note)));
    }
    if let Some(estimate) = table.summary.row_estimate {
        out.push_str(&format!("**行数估算**：{estimate}\n\n"));
    }
    let Some(detail) = &table.detail else {
        out.push_str(&format!(
            "> 警告：元数据未取回（{}），只输出表级信息。\n\n",
            table.error.as_deref().unwrap_or("驱动未返回元数据")
        ));
        return;
    };
    if let Some(engine) = detail
        .engine
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        out.push_str(&format!("**引擎**：{engine}\n\n"));
    }
    if let Some(collation) = detail
        .collation
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        out.push_str(&format!("**排序规则**：{collation}\n\n"));
    }
    out.push_str("### 列\n\n");
    if detail.columns.is_empty() {
        out.push_str("> 警告：没有列信息，驱动可能未提供。\n\n");
    } else {
        out.push_str("| # | 列 | 类型 | 可空 | 主键 | 默认 | 注释 |\n");
        out.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
        for (index, column) in detail.columns.iter().enumerate() {
            let note = notes
                .column_note(path, &column.meta.name)
                .or(column.comment.as_deref())
                .unwrap_or("");
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                index + 1,
                md_code(&column.meta.name),
                md_code(&column_type(column)),
                if column.meta.nullable { "是" } else { "否" },
                if column.is_primary_key { "是" } else { "" },
                column.default.as_deref().map(md_code).unwrap_or_default(),
                md_cell(note),
            ));
        }
        out.push('\n');
    }
    out.push_str("### 索引\n\n");
    if detail.indexes.is_empty() {
        out.push_str("（无索引信息）\n\n");
    } else {
        for index in &detail.indexes {
            let mut flags = Vec::new();
            if index.primary {
                flags.push("主键");
            }
            if index.unique {
                flags.push("唯一");
            }
            if let Some(method) = index
                .method
                .as_deref()
                .filter(|text| !text.trim().is_empty())
            {
                flags.push(method);
            }
            let cols = index
                .columns
                .iter()
                .map(|column| md_code(column))
                .collect::<Vec<_>>()
                .join(", ");
            let flags = if flags.is_empty() {
                String::new()
            } else {
                format!("（{}）", flags.join("，"))
            };
            out.push_str(&format!("- {}：{cols}{flags}\n", md_code(&index.name)));
        }
        out.push('\n');
    }
    out.push_str("### 外键\n\n");
    if detail.foreign_keys.is_empty() {
        out.push_str("（无外键）\n\n");
    } else {
        for fk in &detail.foreign_keys {
            out.push_str(&format!(
                "- {}：`{}` → `{}`{}\n",
                md_code(&fk.name),
                fk.columns.join(", "),
                referenced_columns(&fk.referenced_table, &fk.referenced_columns),
                fk_actions(fk.on_delete.as_deref(), fk.on_update.as_deref()),
            ));
        }
        out.push('\n');
    }
}

fn referenced_columns(table: &str, columns: &[String]) -> String {
    if columns.is_empty() {
        table.to_string()
    } else {
        format!("{table}.{}", columns.join(", "))
    }
}

fn fk_actions(on_delete: Option<&str>, on_update: Option<&str>) -> String {
    let mut actions = Vec::new();
    if let Some(action) = on_delete.filter(|text| !text.trim().is_empty()) {
        actions.push(format!("ON DELETE {action}"));
    }
    if let Some(action) = on_update.filter(|text| !text.trim().is_empty()) {
        actions.push(format!("ON UPDATE {action}"));
    }
    if actions.is_empty() {
        String::new()
    } else {
        format!("（{}）", actions.join("，"))
    }
}

fn render_relations_markdown(out: &mut String, tables: &[&DocTable]) {
    let mut edges = Vec::new();
    for table in tables {
        let Some(detail) = &table.detail else {
            continue;
        };
        for fk in &detail.foreign_keys {
            for (index, column) in fk.columns.iter().enumerate() {
                let target = fk
                    .referenced_columns
                    .get(index)
                    .cloned()
                    .unwrap_or_default();
                edges.push(format!(
                    "- `{}.{}` → `{}.{}`（`{}`）",
                    table.summary.path.name, column, fk.referenced_table, target, fk.name
                ));
            }
        }
    }
    if edges.is_empty() {
        return;
    }
    out.push_str("## 关系总览\n\n");
    for edge in edges {
        out.push_str(&edge);
        out.push('\n');
    }
    out.push('\n');
}

fn md_note(text: &str) -> String {
    text.replace(['\r', '\n'], " ").trim().to_string()
}

fn md_cell(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], "<br>")
        .trim()
        .to_string()
}

fn md_code(text: &str) -> String {
    let text = md_cell(text);
    if text.contains('`') {
        format!("`` {text} ``")
    } else {
        format!("`{text}`")
    }
}

fn render_dbml(
    input: &DocInput,
    notes: &DocNotes,
    tables: &[&DocTable],
    warnings: &[String],
) -> String {
    let mut out = String::new();
    let database = if input.database.trim().is_empty() {
        "默认库"
    } else {
        input.database.trim()
    };
    out.push_str(&format!("// 数据库文档：{database}\n"));
    out.push_str("// 由 RustDb 只读生成；不含数据。\n\n");
    if !notes.project.trim().is_empty() {
        out.push_str("Project rustdb_doc {\n");
        out.push_str(&format!("  Note: '{}'\n", dbml_note(notes.project.trim())));
        out.push_str("}\n\n");
    }
    for table in tables {
        render_table_dbml(&mut out, table, notes);
    }
    render_refs_dbml(&mut out, tables);
    for warning in warnings {
        out.push_str(&format!(
            "// 警告：{}\n",
            warning.replace(['\r', '\n'], " ")
        ));
    }
    out
}

fn render_table_dbml(out: &mut String, table: &DocTable, notes: &DocNotes) {
    let path = &table.summary.path;
    let name = dbml_qualified(&path.ns, &path.name);
    out.push_str(&format!("Table {name} {{\n"));
    if let Some(note) = notes.table_note(path) {
        out.push_str(&format!("  Note: '{}'\n", dbml_note(note)));
    } else if let Some(comment) = table_comment(table) {
        out.push_str(&format!("  Note: '{}'\n", dbml_note(comment)));
    }
    let Some(detail) = &table.detail else {
        out.push_str("  // 元数据未取回\n}\n\n");
        return;
    };
    for column in &detail.columns {
        let mut settings = Vec::new();
        if column.is_primary_key {
            settings.push("pk".to_string());
        }
        if !column.meta.nullable {
            settings.push("not null".to_string());
        }
        if let Some(default) = column
            .default
            .as_deref()
            .map(dbml_default)
            .filter(|text| !text.is_empty())
        {
            settings.push(default);
        }
        let note = notes
            .column_note(path, &column.meta.name)
            .or(column.comment.as_deref())
            .filter(|text| !text.trim().is_empty());
        if let Some(note) = note {
            settings.push(format!("note: '{}'", dbml_note(note)));
        }
        let settings = if settings.is_empty() {
            String::new()
        } else {
            format!(" [{}]", settings.join(", "))
        };
        out.push_str(&format!(
            "  {} {}{}\n",
            dbml_ident(&column.meta.name),
            dbml_type(column),
            settings
        ));
    }
    if !detail.indexes.is_empty() {
        out.push_str("\n  indexes {\n");
        for index in &detail.indexes {
            let mut settings = Vec::new();
            if index.primary {
                settings.push("pk".to_string());
            }
            if index.unique && !index.primary {
                settings.push("unique".to_string());
            }
            settings.push(format!("name: '{}'", dbml_note(&index.name)));
            let columns = index
                .columns
                .iter()
                .map(|column| dbml_ident(column))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("    ({columns}) [{}]\n", settings.join(", ")));
        }
        out.push_str("  }\n");
    }
    out.push_str("}\n\n");
}

fn render_refs_dbml(out: &mut String, tables: &[&DocTable]) {
    for table in tables {
        let Some(detail) = &table.detail else {
            continue;
        };
        for fk in &detail.foreign_keys {
            let left = dbml_endpoint(
                &table.summary.path.ns,
                &table.summary.path.name,
                &fk.columns,
            );
            let (schema, target) = split_reference(&table.summary.path.ns, &fk.referenced_table);
            let right = dbml_endpoint_parts(schema.as_deref(), &target, &fk.referenced_columns);
            out.push_str(&format!("// fk: {}\n", fk.name.replace(['\r', '\n'], " ")));
            out.push_str(&format!("Ref: {left} > {right}\n\n"));
        }
    }
}

fn dbml_endpoint(ns: &Namespace, table: &str, columns: &[String]) -> String {
    dbml_endpoint_parts(ns.schema.as_deref(), table, columns)
}

fn dbml_endpoint_parts(schema: Option<&str>, table: &str, columns: &[String]) -> String {
    let table = match schema.filter(|schema| !schema.trim().is_empty()) {
        Some(schema) => format!("{}.{}", dbml_ident(schema.trim()), dbml_ident(table)),
        None => dbml_ident(table),
    };
    let columns = columns
        .iter()
        .map(|column| dbml_ident(column))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{table}.({columns})")
}

fn dbml_qualified(ns: &Namespace, table: &str) -> String {
    match ns
        .schema
        .as_deref()
        .filter(|schema| !schema.trim().is_empty())
    {
        Some(schema) => format!("{}.{}", dbml_ident(schema.trim()), dbml_ident(table)),
        None => dbml_ident(table),
    }
}

fn dbml_ident(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "'"))
}

fn dbml_type(column: &ColumnInfo) -> String {
    let native = column.meta.native_type.trim();
    let raw = if native.is_empty() {
        column.meta.logical_type.label()
    } else {
        native.to_string()
    };
    if raw.chars().any(char::is_whitespace) {
        format!("\"{}\"", raw.replace('"', "'"))
    } else {
        raw
    }
}

fn dbml_default(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("null") {
        return String::new();
    }
    let bare = value == "true"
        || value == "false"
        || value
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+'));
    if bare {
        format!("default: {value}")
    } else {
        format!("default: `{}`", value.replace('`', "'"))
    }
}

fn dbml_note(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace(['\r', '\n'], "\\n")
}

/// 笔记落盘路径（与 `ai_models.json` 同目录）。
pub fn notes_path(dir: &Path) -> PathBuf {
    dir.join("doc_notes.json")
}

/// 读取笔记集合；文件缺失或损坏时返回默认值。
pub fn load_notes(path: &Path) -> DocNoteStore {
    let Ok(text) = std::fs::read_to_string(path) else {
        return DocNoteStore::default();
    };
    match serde_json::from_str::<DocNoteStore>(&text) {
        Ok(store) => store,
        Err(error) => {
            log::warn!("文档笔记解析失败：{error}（{}）", path.display());
            DocNoteStore::default()
        }
    }
}

/// 原子写入笔记（同目录临时文件 + 替换）。
pub fn save_notes(path: &Path, store: &DocNoteStore) -> io::Result<()> {
    let text = serde_json::to_string_pretty(store)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomic(path, &text)
}

/// 原子写文本：先写同目录 `*.tmp`，再替换目标文件。
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "document.txt".into());
    let tmp = path.with_file_name(format!(".{file_name}.tmp"));
    std::fs::write(&tmp, text)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&tmp);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::{
        ColumnInfo, DdlSource, ForeignKeyInfo, IndexInfo, LogicalType, Namespace, ObjectKind,
        ObjectPath, ObjectSummary, TableDetail,
    };

    fn path(schema: &str, name: &str) -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("app"), Some(schema)),
            name,
            ObjectKind::Table,
        )
    }

    fn column(name: &str, logical: LogicalType, nullable: bool) -> ColumnInfo {
        let mut column = ColumnInfo::simple(name, logical, 0);
        column.meta.nullable = nullable;
        column
    }

    fn table(
        name: &str,
        comment: Option<&str>,
        mut columns: Vec<ColumnInfo>,
        indexes: Vec<IndexInfo>,
        foreign_keys: Vec<ForeignKeyInfo>,
    ) -> DocTable {
        for (index, column) in columns.iter_mut().enumerate() {
            column.ordinal = index as u32;
        }
        let path = path("public", name);
        DocTable {
            summary: ObjectSummary {
                path: path.clone(),
                row_estimate: Some(42),
                comment: comment.map(str::to_string),
                has_primary_key: Some(true),
            },
            detail: Some(TableDetail {
                summary: ObjectSummary {
                    path,
                    row_estimate: Some(42),
                    comment: comment.map(str::to_string),
                    has_primary_key: Some(true),
                },
                columns,
                indexes,
                foreign_keys,
                checks: Vec::new(),
                triggers: None,
                engine: Some("InnoDB".into()),
                collation: Some("utf8mb4".into()),
                comment: comment.map(str::to_string),
                ddl_source: DdlSource::Synthesized,
            }),
            error: None,
        }
    }

    fn fk(name: &str, columns: &[&str], target: &str, target_columns: &[&str]) -> ForeignKeyInfo {
        ForeignKeyInfo {
            name: name.into(),
            columns: columns.iter().map(|column| column.to_string()).collect(),
            referenced_table: target.into(),
            referenced_columns: target_columns
                .iter()
                .map(|column| column.to_string())
                .collect(),
            on_delete: Some("CASCADE".into()),
            on_update: None,
        }
    }

    fn input(tables: Vec<DocTable>) -> DocInput {
        DocInput {
            database: "app.public".into(),
            tables,
            skipped: 0,
        }
    }

    #[test]
    fn markdown_lists_columns_and_prefers_local_notes() {
        let orders = table(
            "orders",
            Some("订单"),
            vec![
                column(
                    "id",
                    LogicalType::Int {
                        bits: 64,
                        unsigned: false,
                    },
                    false,
                ),
                column(
                    "user_id",
                    LogicalType::Int {
                        bits: 64,
                        unsigned: false,
                    },
                    false,
                ),
                column("memo", LogicalType::Text { max_len: None }, true),
            ],
            vec![IndexInfo {
                name: "orders_pkey".into(),
                columns: vec!["id".into()],
                unique: true,
                primary: true,
                method: Some("btree".into()),
            }],
            vec![fk(
                "fk_orders_user",
                &["user_id"],
                "app.public.users",
                &["id"],
            )],
        );
        let users = table(
            "users",
            Some("用户"),
            vec![column(
                "id",
                LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
                false,
            )],
            Vec::new(),
            Vec::new(),
        );
        let mut notes = DocNotes::default();
        notes.set_project("项目备注");
        notes.set_table_note(&orders.summary.path, "订单本地笔记");
        notes.set_column_note(&orders.summary.path, "MEMO", "列笔记");
        let docs = generate(&input(vec![orders, users]), &notes);
        assert!(docs.markdown.contains("# 数据库文档：app.public"));
        assert!(docs.markdown.contains("## 项目说明"));
        assert!(docs.markdown.contains("项目备注"));
        assert!(docs.markdown.contains("**本地笔记**：订单本地笔记"));
        assert!(docs
            .markdown
            .contains("| 3 | `memo` | `text` | 是 |  |  | 列笔记 |"));
        assert!(docs
            .markdown
            .contains("- `orders_pkey`：`id`（主键，唯一，btree）"));
        assert!(docs.markdown.contains(
            "- `fk_orders_user`：`user_id` → `app.public.users.id`（ON DELETE CASCADE）"
        ));
        assert!(docs.markdown.contains("## 关系总览"));
        assert!(docs
            .markdown
            .contains("`orders.user_id` → `app.public.users.id`"));
        assert!(docs.warnings.is_empty(), "{:?}", docs.warnings);
    }

    #[test]
    fn markdown_escapes_pipes_and_newlines_in_comments() {
        let mut table = table(
            "audit",
            Some("含 | 竖线\n换行"),
            vec![column(
                "id",
                LogicalType::Int {
                    bits: 32,
                    unsigned: false,
                },
                false,
            )],
            Vec::new(),
            Vec::new(),
        );
        table.detail.as_mut().unwrap().columns[0].comment = Some("a | b".into());
        let docs = generate(&input(vec![table]), &DocNotes::default());
        assert!(docs.markdown.contains("含 | 竖线 换行"));
        assert!(docs.markdown.contains(r"| a \| b |"));
    }

    #[test]
    fn dbml_includes_tables_indexes_and_refs() {
        let mut orders = table(
            "orders",
            Some("订单 '主'"),
            vec![
                column(
                    "id",
                    LogicalType::Int {
                        bits: 64,
                        unsigned: false,
                    },
                    false,
                ),
                column(
                    "user_id",
                    LogicalType::Int {
                        bits: 64,
                        unsigned: false,
                    },
                    false,
                ),
            ],
            vec![IndexInfo {
                name: "orders_pkey".into(),
                columns: vec!["id".into()],
                unique: true,
                primary: true,
                method: None,
            }],
            vec![fk(
                "fk_orders_user",
                &["user_id"],
                "app.public.users",
                &["id"],
            )],
        );
        orders.detail.as_mut().unwrap().columns[0].is_primary_key = true;
        let mut table_note = DocNotes::default();
        table_note.set_table_note(&orders.summary.path, "订单\n备注");
        table_note.set_column_note(&orders.summary.path, "user_id", "下单人 '外键'");
        let docs = generate(&input(vec![orders]), &table_note);
        assert!(docs.dbml.contains("Table \"public\".\"orders\" {"));
        assert!(docs.dbml.contains("Note: '订单\\n备注'"));
        assert!(docs.dbml.contains("\"id\" int64 [pk, not null]"));
        assert!(docs
            .dbml
            .contains("\"user_id\" int64 [not null, note: '下单人 \\'外键\\'']"));
        assert!(docs.dbml.contains("(\"id\") [pk, name: 'orders_pkey']"));
        assert!(docs
            .dbml
            .contains("Ref: \"public\".\"orders\".(\"user_id\") > \"public\".\"users\".(\"id\")"));
        assert!(!docs.dbml.contains("LIMIT"));
    }

    #[test]
    fn dbml_quotes_multi_word_types_and_defaults() {
        let mut table = table(
            "events",
            None,
            vec![column("at", LogicalType::TimestampTz, false)],
            Vec::new(),
            Vec::new(),
        );
        {
            let column = &mut table.detail.as_mut().unwrap().columns[0];
            column.meta.native_type = "timestamp with time zone".into();
            column.default = Some("now()".into());
            column.meta.logical_type = LogicalType::TimestampTz;
        }
        let docs = generate(&input(vec![table]), &DocNotes::default());
        assert!(docs
            .dbml
            .contains("\"at\" \"timestamp with time zone\" [not null, default: `now()`]"));
    }

    #[test]
    fn missing_metadata_and_orphan_notes_produce_warnings() {
        let summary = ObjectSummary {
            path: path("public", "broken"),
            row_estimate: None,
            comment: None,
            has_primary_key: None,
        };
        let broken = DocTable {
            summary,
            detail: None,
            error: Some("权限不足".into()),
        };
        let empty = table("empty", None, Vec::new(), Vec::new(), Vec::new());
        let mut notes = DocNotes::default();
        notes.set_table_note(&path("public", "ghost"), "幽灵表");
        let mut input = input(vec![broken, empty]);
        input.skipped = 5;
        let docs = generate(&input, &notes);
        assert!(docs
            .warnings
            .iter()
            .any(|warning| warning.contains("broken") && warning.contains("权限不足")));
        assert!(docs
            .warnings
            .iter()
            .any(|warning| warning.contains("empty") && warning.contains("没有列信息")));
        assert!(docs
            .warnings
            .iter()
            .any(|warning| warning.contains("跳过 5 张")));
        assert!(docs
            .warnings
            .iter()
            .any(|warning| warning.contains("表笔记「app.public|ghost」")));
        assert!(docs.markdown.contains("## 元数据警告"));
        assert!(docs.dbml.contains("// 警告："));
    }

    #[test]
    fn empty_input_warns_and_renders_headers() {
        let docs = generate(&DocInput::default(), &DocNotes::default());
        assert_eq!(docs.warnings.len(), 1);
        assert!(docs.markdown.contains("# 数据库文档：默认库"));
        assert!(docs.dbml.contains("// 数据库文档：默认库"));
    }

    #[test]
    fn out_of_scope_foreign_key_warns() {
        let table = table(
            "orders",
            None,
            vec![column(
                "id",
                LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
                false,
            )],
            Vec::new(),
            vec![fk("fk_remote", &["id"], "other.public.users", &["id"])],
        );
        let docs = generate(&input(vec![table]), &DocNotes::default());
        assert!(docs
            .warnings
            .iter()
            .any(|warning| warning.contains("fk_remote") && warning.contains("不在本文档范围内")));
    }

    #[test]
    fn notes_round_trip_and_corrupt_file_falls_back() {
        let dir = std::env::temp_dir().join(format!("rustdb-doc-notes-{}", std::process::id()));
        let path = notes_path(&dir);
        let mut store = DocNoteStore::default();
        let mut notes = DocNotes::default();
        notes.set_project("项目");
        notes.set_table_note(&path_of("orders"), "表");
        notes.set_column_note(&path_of("orders"), "id", "列");
        store.set("p1", notes.clone());
        save_notes(&path, &store).expect("笔记应落盘");
        assert_eq!(load_notes(&path), store);
        std::fs::write(&path, "不是 json").expect("写入损坏内容");
        assert_eq!(load_notes(&path), DocNoteStore::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn path_of(name: &str) -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("app"), Some("public")),
            name,
            ObjectKind::Table,
        )
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let dir = std::env::temp_dir().join(format!("rustdb-doc-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.md");
        std::fs::write(&path, "旧内容").unwrap();
        write_atomic(&path, "新内容").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "新内容");
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0, "临时文件应被清理");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_ascii_names_survive() {
        let table = table(
            "用户",
            Some("用户表"),
            vec![column(
                "编号",
                LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
                false,
            )],
            Vec::new(),
            Vec::new(),
        );
        let docs = generate(&input(vec![table]), &DocNotes::default());
        assert!(docs.markdown.contains("## app.public.用户"));
        assert!(docs.dbml.contains("Table \"public\".\"用户\""));
        assert!(docs.dbml.contains("\"编号\" int64"));
    }

    #[test]
    fn table_key_is_case_insensitive() {
        let upper = ObjectPath::new(
            Namespace::new(Some("APP"), Some("PUBLIC")),
            "Orders",
            ObjectKind::Table,
        );
        let lower = ObjectPath::new(
            Namespace::new(Some("app"), Some("public")),
            "orders",
            ObjectKind::Table,
        );
        assert_eq!(table_key(&upper), table_key(&lower));
        let mut notes = DocNotes::default();
        notes.set_table_note(&upper, "备注");
        assert_eq!(notes.table_note(&lower), Some("备注"));
    }

    #[test]
    fn composite_foreign_key_renders_both_columns() {
        let table = table(
            "order_items",
            None,
            vec![column(
                "order_id",
                LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
                false,
            )],
            Vec::new(),
            vec![fk(
                "fk_items",
                &["order_id", "line"],
                "orders",
                &["id", "line"],
            )],
        );
        let docs = generate(&input(vec![table]), &DocNotes::default());
        assert!(docs.dbml.contains(
            ".\"order_items\".(\"order_id\", \"line\") > \"public\".\"orders\".(\"id\", \"line\")"
        ));
    }
}
