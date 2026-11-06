//! 数据对比与同步（F-78）：纯逻辑规划、行级 diff 与同步 SQL，无 IO。
//!
//! 口径：
//! * 按**关键字段**匹配两张表：配置优先，否则取源表主键；关键字段缺失 / 为 NULL /
//!   重复都会显式告警，重复键只比较首行；
//! * 差异分 `INSERT` / `UPDATE` / `DELETE`，只有非关键列变化才算 `UPDATE`；
//! * 跨引擎按逻辑类型比较，类型不同时告警（可能产生假差异），不做隐式类型转换；
//! * 同步脚本复用 `transfer::insert_sql` 生成批量插入，更新 / 删除逐行生成；
//! * 对比配置保存为 JSON，可导出 / 导入，导入时校验版本与基本结构。

use crate::edit::{ident, literal};
use crate::transfer::{insert_sql, CaseStrategy, TransferMode};
use rustdb_db_api::{ColumnInfo, DbKind, LogicalType, Namespace, ObjectPath, SessionId, Value};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// 单次对比最多涉及的表数量。
pub const MAX_COMPARE_TABLES: usize = 100;
/// 默认每批读取 / 插入行数。
pub const DEFAULT_DIFF_BATCH_ROWS: usize = 1000;
/// 默认单语句超时。
pub const DEFAULT_SYNC_TIMEOUT_MS: u64 = 30_000;
/// 预览里最多展开的同步语句条数（其余只给计数）。
pub const MAX_PREVIEW_STATEMENTS: usize = 500;
/// 配置版本；导入时版本不符直接拒绝。
pub const DATA_COMPARE_CONFIG_VERSION: u32 = 1;

fn default_version() -> u32 {
    DATA_COMPARE_CONFIG_VERSION
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataCompareOptions {
    pub batch_rows: usize,
    pub timeout_ms: Option<u64>,
    pub case: CaseStrategy,
    pub insert: bool,
    pub update: bool,
    pub delete: bool,
}

impl Default for DataCompareOptions {
    fn default() -> Self {
        Self {
            batch_rows: DEFAULT_DIFF_BATCH_ROWS,
            timeout_ms: Some(DEFAULT_SYNC_TIMEOUT_MS),
            case: CaseStrategy::Preserve,
            insert: true,
            update: true,
            delete: true,
        }
    }
}

impl DataCompareOptions {
    /// 方言夹取后的批大小，与跨库传输保持同一口径。
    pub fn effective_batch(&self, kind: DbKind) -> usize {
        let configured = self.batch_rows.max(1);
        match kind {
            DbKind::Oracle => 1,
            DbKind::Mssql => configured.min(1000),
            _ => configured,
        }
    }

    pub fn generates_any(&self) -> bool {
        self.insert || self.update || self.delete
    }
}

/// 单张表的对比配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableCompareConfig {
    /// 源表名。
    pub source: String,
    /// 目标表名；为空时与源同名。
    #[serde(default)]
    pub target: Option<String>,
    /// 关键字段（源列名）；为空时使用源表主键。
    #[serde(default)]
    pub key_columns: Vec<String>,
    /// 参与比较的列（源列名）；为空时取全部同名可写列。
    #[serde(default)]
    pub columns: Vec<String>,
    /// 忽略的列（源列名），优先级高于 `columns`。
    #[serde(default)]
    pub ignore_columns: Vec<String>,
}

impl TableCompareConfig {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: None,
            key_columns: Vec::new(),
            columns: Vec::new(),
            ignore_columns: Vec::new(),
        }
    }
}

/// 对比配置：连接 / 命名空间 / 表清单 / 选项，可保存并导入导出。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DataCompareConfig {
    pub version: u32,
    pub source_profile: Option<String>,
    pub target_profile: Option<String>,
    pub source_ns: Namespace,
    pub target_ns: Namespace,
    pub tables: Vec<TableCompareConfig>,
    pub options: DataCompareOptions,
}

impl Default for DataCompareConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            source_profile: None,
            target_profile: None,
            source_ns: Namespace::new(None::<String>, None::<String>),
            target_ns: Namespace::new(None::<String>, None::<String>),
            tables: Vec::new(),
            options: DataCompareOptions::default(),
        }
    }
}

impl DataCompareConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != DATA_COMPARE_CONFIG_VERSION {
            return Err(format!(
                "对比配置版本 {} 不受支持（当前 {DATA_COMPARE_CONFIG_VERSION}）",
                self.version
            ));
        }
        if self.tables.len() > MAX_COMPARE_TABLES {
            return Err(format!(
                "对比配置最多 {MAX_COMPARE_TABLES} 张表，当前 {} 张",
                self.tables.len()
            ));
        }
        for table in &self.tables {
            if table.source.trim().is_empty() {
                return Err("对比配置里有空表名".into());
            }
            let mut seen = HashSet::new();
            for key in &table.key_columns {
                if !seen.insert(key.to_lowercase()) {
                    return Err(format!("表 {} 的关键字段 {} 重复", table.source, key));
                }
            }
        }
        Ok(())
    }
}

/// 一次数据对比请求（源连接已建立，目标按配置临时建链）。
#[derive(Debug, Clone)]
pub struct DataCompareRequest {
    pub source_session: SessionId,
    pub target_profile: String,
    pub source_ns: Namespace,
    pub target_ns: Namespace,
    pub tables: Vec<TableCompareConfig>,
    pub options: DataCompareOptions,
}

impl DataCompareRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.tables.is_empty() {
            return Err("至少选择一张表".into());
        }
        if self.tables.len() > MAX_COMPARE_TABLES {
            return Err(format!(
                "单次最多对比 {MAX_COMPARE_TABLES} 张表，当前 {} 张",
                self.tables.len()
            ));
        }
        if self.target_profile.trim().is_empty() {
            return Err("请选择目标连接".into());
        }
        if !self.options.generates_any() {
            return Err("至少选择一种同步动作（INSERT / UPDATE / DELETE）".into());
        }
        for table in &self.tables {
            if table.source.trim().is_empty() {
                return Err("表名不能为空".into());
            }
            let mut seen = HashSet::new();
            for key in &table.key_columns {
                if !seen.insert(key.to_lowercase()) {
                    return Err(format!("表 {} 的关键字段 {} 重复", table.source, key));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompareColumn {
    pub source: String,
    pub target: String,
    pub source_type: LogicalType,
    pub target_type: LogicalType,
    pub key: bool,
}

/// 单表对比计划：列映射、关键字段与告警；不读写任何数据。
#[derive(Debug, Clone, PartialEq)]
pub struct DataCompareTablePlan {
    pub source: ObjectPath,
    pub target: ObjectPath,
    /// 与 `source_columns` / `target_columns` 对齐的列映射。
    pub columns: Vec<CompareColumn>,
    pub key_indexes: Vec<usize>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

impl DataCompareTablePlan {
    pub fn source_columns(&self) -> Vec<String> {
        self.columns
            .iter()
            .map(|column| column.source.clone())
            .collect()
    }

    pub fn target_columns(&self) -> Vec<String> {
        self.columns
            .iter()
            .map(|column| column.target.clone())
            .collect()
    }

    pub fn key_columns(&self) -> Vec<String> {
        self.key_indexes
            .iter()
            .filter_map(|index| self.columns.get(*index))
            .map(|column| column.source.clone())
            .collect()
    }

    pub fn target_key_columns(&self) -> Vec<String> {
        self.key_indexes
            .iter()
            .filter_map(|index| self.columns.get(*index))
            .map(|column| column.target.clone())
            .collect()
    }
}

/// 根据配置与两侧列信息生成单表对比计划。
pub fn plan_compare_table(
    source: &ObjectPath,
    source_columns: &[ColumnInfo],
    target: &ObjectPath,
    target_columns: &[ColumnInfo],
    config: &TableCompareConfig,
    options: &DataCompareOptions,
) -> DataCompareTablePlan {
    let failure = |message: String| DataCompareTablePlan {
        source: source.clone(),
        target: target.clone(),
        columns: Vec::new(),
        key_indexes: Vec::new(),
        warnings: Vec::new(),
        error: Some(message),
    };
    if source_columns.is_empty() {
        return failure("源表没有可读取的列".into());
    }
    if target_columns.is_empty() {
        return failure("目标表没有可读取的列".into());
    }

    let ignored: HashSet<String> = config
        .ignore_columns
        .iter()
        .map(|column| column.to_lowercase())
        .collect();
    let explicit: HashSet<String> = config
        .columns
        .iter()
        .map(|column| column.to_lowercase())
        .collect();
    let mut warnings = Vec::new();
    let mut columns = Vec::new();
    for column in source_columns {
        let lower = column.meta.name.to_lowercase();
        if ignored.contains(&lower) {
            continue;
        }
        if !explicit.is_empty() && !explicit.contains(&lower) {
            continue;
        }
        let wanted = options.case.apply(&column.meta.name);
        let Some(found) = target_columns
            .iter()
            .find(|candidate| candidate.meta.name.eq_ignore_ascii_case(&wanted))
        else {
            warnings.push(format!("目标缺少列 {}，已跳过", column.meta.name));
            continue;
        };
        if found.meta.logical_type != column.meta.logical_type {
            warnings.push(format!(
                "列 {} 类型不同：源 {} → 目标 {}（按值精确比较，可能产生假差异）",
                column.meta.name,
                column.meta.logical_type.label(),
                found.meta.logical_type.label()
            ));
        }
        columns.push(CompareColumn {
            source: column.meta.name.to_string(),
            target: found.meta.name.to_string(),
            source_type: column.meta.logical_type.clone(),
            target_type: found.meta.logical_type.clone(),
            key: false,
        });
    }
    if columns.is_empty() {
        return failure("源表与目标表没有可对比的同名列".into());
    }

    let configured_keys: Vec<String> = config
        .key_columns
        .iter()
        .map(|column| column.trim().to_lowercase())
        .filter(|column| !column.is_empty())
        .collect();
    let key_sources: Vec<String> = if configured_keys.is_empty() {
        source_columns
            .iter()
            .filter(|column| column.is_primary_key)
            .map(|column| column.meta.name.to_string())
            .collect()
    } else {
        let mut keys = Vec::with_capacity(configured_keys.len());
        for key in &configured_keys {
            let Some(column) = source_columns
                .iter()
                .find(|column| column.meta.name.to_lowercase() == *key)
            else {
                return failure(format!("源表没有关键字段 {key}"));
            };
            keys.push(column.meta.name.to_string());
        }
        keys
    };
    if key_sources.is_empty() {
        return failure("源表没有主键；请在配置里显式指定关键字段".into());
    }

    let mut key_indexes = Vec::with_capacity(key_sources.len());
    for key in &key_sources {
        let Some(index) = columns
            .iter()
            .position(|column| column.source.eq_ignore_ascii_case(key))
        else {
            return failure(format!(
                "关键字段 {key} 不在参与比较的列里（检查忽略列 / 目标缺列）"
            ));
        };
        columns[index].key = true;
        key_indexes.push(index);
    }
    DataCompareTablePlan {
        source: source.clone(),
        target: target.clone(),
        columns,
        key_indexes,
        warnings,
        error: None,
    }
}

/// 单表差异与冲突统计。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TableDiff {
    pub inserts: Vec<Vec<Value>>,
    pub updates: Vec<Vec<Value>>,
    pub deletes: Vec<Vec<Value>>,
    pub same: u64,
    pub warnings: Vec<String>,
}

impl TableDiff {
    pub fn counts(&self) -> (u64, u64, u64, u64) {
        (
            self.inserts.len() as u64,
            self.updates.len() as u64,
            self.deletes.len() as u64,
            self.same,
        )
    }
}

/// 关键字段归一化：跨引擎同值（如 Int / UInt / 整数 Float）得到同一签名。
pub fn key_part(value: &Value) -> String {
    match value {
        Value::Null => "n".to_string(),
        Value::Bool(value) => format!("b{value}"),
        Value::Int(value) => format!("num:{value}"),
        Value::UInt(value) => format!("num:{value}"),
        Value::Float(value) => {
            if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
                format!("num:{}", *value as i64)
            } else {
                format!("f:{:016x}", value.to_bits())
            }
        }
        Value::Decimal(text) => format!("dec:{text}"),
        Value::Text(text) => format!("t:{text}"),
        Value::Bytes(bytes) => format!("x:{}", hex(bytes)),
        Value::Date(_)
        | Value::Time(_)
        | Value::Timestamp(_)
        | Value::TimestampTz(_)
        | Value::Json(_) => {
            format!("v:{value}")
        }
        Value::Uuid(bytes) => format!("uuid:{}", hex(bytes)),
        Value::Array(items) => {
            let parts = items
                .iter()
                .map(key_part)
                .collect::<Vec<_>>()
                .join("\u{1f}");
            format!("a:[{parts}]")
        }
        Value::Omitted {
            total_bytes,
            preview,
        } => format!("o:{total_bytes}:{}", hex(preview)),
        Value::Unknown { native_type, text } => format!("u:{native_type}:{text}"),
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn key_signature(values: &[Value]) -> String {
    values
        .iter()
        .map(key_part)
        .collect::<Vec<_>>()
        .join("\u{1e}")
}

fn has_null_key(row: &[Value], key_indexes: &[usize]) -> bool {
    key_indexes
        .iter()
        .any(|index| row.get(*index).map_or(true, Value::is_null))
}

fn same_row(left: &[Value], right: &[Value], key_indexes: &[usize]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    for index in 0..left.len() {
        if key_indexes.contains(&index) {
            continue;
        }
        if key_part(&left[index]) != key_part(&right[index]) {
            return false;
        }
    }
    true
}

/// 流式行级 diff：先灌入目标表全部行，再逐页喂源表，最后 `finish` 得到删除行。
pub struct RowDiffer {
    key_indexes: Vec<usize>,
    target: HashMap<String, Vec<Value>>,
    consumed: HashSet<String>,
    seen_source: HashSet<String>,
    inserts: Vec<Vec<Value>>,
    updates: Vec<Vec<Value>>,
    same: u64,
    duplicate_source: u64,
    duplicate_target: u64,
    null_source: u64,
    null_target: u64,
}

impl RowDiffer {
    pub fn new(key_indexes: Vec<usize>, target_rows: Vec<Vec<Value>>) -> Self {
        let mut target = HashMap::with_capacity(target_rows.len());
        let mut seen_target = HashSet::with_capacity(target_rows.len());
        let mut duplicate_target = 0u64;
        let mut null_target = 0u64;
        for row in target_rows {
            if has_null_key(&row, &key_indexes) {
                null_target += 1;
                continue;
            }
            let signature = key_signature(
                &key_indexes
                    .iter()
                    .filter_map(|index| row.get(*index).cloned())
                    .collect::<Vec<_>>(),
            );
            if !seen_target.insert(signature.clone()) {
                duplicate_target += 1;
                continue;
            }
            target.insert(signature, row);
        }
        Self {
            key_indexes,
            target,
            consumed: HashSet::new(),
            seen_source: HashSet::new(),
            inserts: Vec::new(),
            updates: Vec::new(),
            same: 0,
            duplicate_source: 0,
            duplicate_target,
            null_source: 0,
            null_target,
        }
    }

    /// 喂入一页源表行（已按计划列顺序投影）。
    pub fn push(&mut self, row: Vec<Value>) {
        if has_null_key(&row, &self.key_indexes) {
            self.null_source += 1;
            return;
        }
        let signature = key_signature(
            &self
                .key_indexes
                .iter()
                .filter_map(|index| row.get(*index).cloned())
                .collect::<Vec<_>>(),
        );
        if !self.seen_source.insert(signature.clone()) {
            self.duplicate_source += 1;
            return;
        }
        match self.target.get(&signature) {
            None => self.inserts.push(row),
            Some(existing) => {
                self.consumed.insert(signature);
                if same_row(&row, existing, &self.key_indexes) {
                    self.same += 1;
                } else {
                    self.updates.push(row);
                }
            }
        }
    }

    pub fn finish(self) -> TableDiff {
        let mut deletes: Vec<Vec<Value>> = self
            .target
            .into_iter()
            .filter(|(signature, _)| !self.consumed.contains(signature))
            .map(|(_, row)| row)
            .collect();
        deletes.sort_by(|left, right| {
            let left = self
                .key_indexes
                .iter()
                .filter_map(|index| left.get(*index))
                .map(key_part)
                .collect::<Vec<_>>()
                .join("\u{1e}");
            let right = self
                .key_indexes
                .iter()
                .filter_map(|index| right.get(*index))
                .map(key_part)
                .collect::<Vec<_>>()
                .join("\u{1e}");
            left.cmp(&right)
        });
        let mut warnings = Vec::new();
        if self.null_source > 0 {
            warnings.push(format!(
                "源表有 {} 行关键字段为 NULL，已跳过",
                self.null_source
            ));
        }
        if self.null_target > 0 {
            warnings.push(format!(
                "目标表有 {} 行关键字段为 NULL，已跳过",
                self.null_target
            ));
        }
        if self.duplicate_source > 0 {
            warnings.push(format!(
                "源表有 {} 个重复关键值，只比较首个",
                self.duplicate_source
            ));
        }
        if self.duplicate_target > 0 {
            warnings.push(format!(
                "目标表有 {} 个重复关键值，只比较首个",
                self.duplicate_target
            ));
        }
        TableDiff {
            inserts: self.inserts,
            updates: self.updates,
            deletes,
            same: self.same,
            warnings,
        }
    }
}

/// 单表对比结果（差异计数 + 告警）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TableCompareResult {
    pub table: String,
    pub insert: u64,
    pub update: u64,
    pub delete: u64,
    pub same: u64,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

impl TableCompareResult {
    pub fn summary(&self) -> String {
        if let Some(error) = &self.error {
            return format!("{}：{error}", self.table);
        }
        format!(
            "{}：新增 {} · 更新 {} · 删除 {} · 相同 {}",
            self.table, self.insert, self.update, self.delete, self.same
        )
    }
}

/// 生成更新语句（逐行）。
pub fn update_sql(
    kind: DbKind,
    path: &ObjectPath,
    columns: &[String],
    key_indexes: &[usize],
    row: &[Value],
) -> Result<String, String> {
    if row.len() != columns.len() {
        return Err(format!(
            "行宽不一致：期望 {} 列，实际 {} 列",
            columns.len(),
            row.len()
        ));
    }
    let sets = columns
        .iter()
        .enumerate()
        .filter(|(index, _)| !key_indexes.contains(index))
        .map(|(index, name)| {
            literal(kind, &row[index]).map(|value| format!("{} = {value}", ident(kind, name)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if sets.is_empty() {
        return Err("没有可更新的非关键列".into());
    }
    let conditions = key_indexes
        .iter()
        .filter_map(|index| {
            columns
                .get(*index)
                .zip(row.get(*index))
                .map(|(name, value)| (name, value))
        })
        .map(|(name, value)| {
            literal(kind, value).map(|value| format!("{} = {value}", ident(kind, name)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!(
        "UPDATE {} SET {} WHERE {};",
        crate::core::object_from_clause(kind, path),
        sets.join(", "),
        conditions.join(" AND ")
    ))
}

/// 生成删除语句（逐行）。
pub fn delete_sql(
    kind: DbKind,
    path: &ObjectPath,
    columns: &[String],
    key_indexes: &[usize],
    row: &[Value],
) -> Result<String, String> {
    if row.len() != columns.len() {
        return Err(format!(
            "行宽不一致：期望 {} 列，实际 {} 列",
            columns.len(),
            row.len()
        ));
    }
    let conditions = key_indexes
        .iter()
        .filter_map(|index| {
            columns
                .get(*index)
                .zip(row.get(*index))
                .map(|(name, value)| (name, value))
        })
        .map(|(name, value)| {
            literal(kind, value).map(|value| format!("{} = {value}", ident(kind, name)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if conditions.is_empty() {
        return Err("删除需要关键字段".into());
    }
    Ok(format!(
        "DELETE FROM {} WHERE {};",
        crate::core::object_from_clause(kind, path),
        conditions.join(" AND ")
    ))
}

/// 把差异转成同步语句：先批量 INSERT，再逐行 UPDATE，最后逐行 DELETE。
pub fn sync_statements(
    kind: DbKind,
    path: &ObjectPath,
    columns: &[String],
    key_indexes: &[usize],
    diff: &TableDiff,
    options: &DataCompareOptions,
) -> Result<Vec<String>, String> {
    let mut statements = Vec::new();
    let batch = options.effective_batch(kind);
    if options.insert {
        for chunk in diff.inserts.chunks(batch) {
            statements.push(insert_sql(
                kind,
                path,
                columns,
                chunk,
                TransferMode::Append,
                &[],
            )?);
        }
    }
    if options.update {
        for row in &diff.updates {
            statements.push(update_sql(kind, path, columns, key_indexes, row)?);
        }
    }
    if options.delete {
        for row in &diff.deletes {
            statements.push(delete_sql(kind, path, columns, key_indexes, row)?);
        }
    }
    Ok(statements)
}

/// 同步脚本预览：逐表计数 + 语句；语句过多时只展开前 `limit` 条。
pub fn preview_lines(
    results: &[TableCompareResult],
    scripts: &[Vec<String>],
    warnings: &[String],
    limit: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut shown = 0usize;
    let mut total = 0usize;
    for (index, statements) in scripts.iter().enumerate() {
        total += statements.len();
        let table = results
            .get(index)
            .map(|result| result.table.as_str())
            .unwrap_or("?");
        lines.push(format!("-- {table} · {} 条语句", statements.len()));
        for statement in statements {
            if shown >= limit {
                continue;
            }
            for line in statement.lines() {
                lines.push(format!("  {line}"));
            }
            shown += 1;
        }
    }
    if total > limit {
        lines.push(format!(
            "  … 其余 {} 条语句已省略（计数仍包含）",
            total - limit
        ));
    }
    for warning in warnings {
        lines.push(format!("警告：{warning}"));
    }
    lines
}

/// 对比配置默认保存位置。
pub fn data_compare_config_path(dir: &Path) -> PathBuf {
    dir.join("data-compare.json")
}

/// 读取配置；文件缺失 / 损坏 / 版本不符时回退默认值（内部保存用）。
pub fn load_data_compare_config(path: &Path) -> DataCompareConfig {
    let Ok(text) = std::fs::read_to_string(path) else {
        return DataCompareConfig::default();
    };
    match serde_json::from_str::<DataCompareConfig>(&text) {
        Ok(config) if config.validate().is_ok() => config,
        _ => DataCompareConfig::default(),
    }
}

pub fn save_data_compare_config(path: &Path, config: &DataCompareConfig) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(config)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(path, text)
}

/// 导出配置：先校验，再写 JSON。
pub fn export_data_compare_config(path: &Path, config: &DataCompareConfig) -> Result<(), String> {
    config.validate()?;
    save_data_compare_config(path, config).map_err(|error| format!("写入配置失败：{error}"))
}

/// 导入配置：解析 JSON 并校验版本与结构，失败时给出明确原因。
pub fn import_data_compare_config(path: &Path) -> Result<DataCompareConfig, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("读取配置失败：{error}"))?;
    let config: DataCompareConfig =
        serde_json::from_str(&text).map_err(|error| format!("配置不是有效 JSON：{error}"))?;
    config.validate()?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::{ColumnInfo, LogicalType, Namespace, ObjectKind};

    fn column(name: &str, logical: LogicalType, pk: bool) -> ColumnInfo {
        let mut info = ColumnInfo::simple(name, logical, 1);
        info.is_primary_key = pk;
        info.meta.nullable = !pk;
        info
    }

    fn int() -> LogicalType {
        LogicalType::Int {
            bits: 64,
            unsigned: false,
        }
    }

    fn text() -> LogicalType {
        LogicalType::Text { max_len: None }
    }

    fn path(name: &str) -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("db"), Some("public")),
            name,
            ObjectKind::Table,
        )
    }

    fn row(id: i64, value: &str) -> Vec<Value> {
        vec![Value::Int(id), Value::Text(value.into())]
    }

    #[test]
    fn defaults_are_safe() {
        let options = DataCompareOptions::default();
        assert_eq!(options.batch_rows, DEFAULT_DIFF_BATCH_ROWS);
        assert_eq!(options.timeout_ms, Some(DEFAULT_SYNC_TIMEOUT_MS));
        assert!(options.generates_any());
        assert_eq!(options.effective_batch(DbKind::Oracle), 1);
        assert_eq!(options.effective_batch(DbKind::Mssql), 1000);
    }

    #[test]
    fn plan_uses_primary_key_and_maps_columns() {
        let config = TableCompareConfig::new("Orders");
        let source_columns = vec![column("id", int(), true), column("note", text(), false)];
        let target_columns = vec![column("ID", int(), true), column("NOTE", text(), false)];
        let plan = plan_compare_table(
            &path("Orders"),
            &source_columns,
            &path("Orders"),
            &target_columns,
            &config,
            &DataCompareOptions::default(),
        );
        assert!(plan.error.is_none(), "{:?}", plan.error);
        assert_eq!(plan.key_columns(), vec!["id".to_string()]);
        assert_eq!(
            plan.target_columns(),
            vec!["ID".to_string(), "NOTE".to_string()]
        );
        assert!(plan.columns[0].key);
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn plan_reports_type_and_missing_column_warnings() {
        let config = TableCompareConfig::new("orders");
        let source_columns = vec![
            column("id", int(), true),
            column("note", text(), false),
            column("extra", text(), false),
        ];
        let target_columns = vec![column("id", int(), true), column("note", int(), false)];
        let plan = plan_compare_table(
            &path("orders"),
            &source_columns,
            &path("orders"),
            &target_columns,
            &config,
            &DataCompareOptions::default(),
        );
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.contains("extra")));
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.contains("类型不同")));
    }

    #[test]
    fn plan_requires_key() {
        let config = TableCompareConfig::new("orders");
        let source_columns = vec![column("id", int(), false)];
        let target_columns = vec![column("id", int(), false)];
        let plan = plan_compare_table(
            &path("orders"),
            &source_columns,
            &path("orders"),
            &target_columns,
            &config,
            &DataCompareOptions::default(),
        );
        assert!(plan.error.unwrap().contains("关键字段"));
    }

    #[test]
    fn plan_accepts_configured_keys_and_ignore_columns() {
        let config = TableCompareConfig {
            source: "orders".into(),
            target: Some("orders_v2".into()),
            key_columns: vec!["code".into()],
            columns: Vec::new(),
            ignore_columns: vec!["updated_at".into()],
        };
        let source_columns = vec![
            column("id", int(), true),
            column("code", text(), false),
            column("updated_at", text(), false),
        ];
        let target_columns = source_columns.clone();
        let plan = plan_compare_table(
            &path("orders"),
            &source_columns,
            &path("orders_v2"),
            &target_columns,
            &config,
            &DataCompareOptions::default(),
        );
        assert!(plan.error.is_none());
        assert_eq!(plan.key_columns(), vec!["code".to_string()]);
        assert_eq!(plan.columns.len(), 2);
    }

    #[test]
    fn diff_detects_insert_update_delete_and_same() {
        let target = vec![row(1, "a"), row(2, "old"), row(3, "gone")];
        let mut differ = RowDiffer::new(vec![0], target);
        differ.push(row(1, "a"));
        differ.push(row(2, "new"));
        differ.push(row(4, "fresh"));
        let diff = differ.finish();
        assert_eq!(diff.counts(), (1, 1, 1, 1));
        assert_eq!(diff.inserts[0], row(4, "fresh"));
        assert_eq!(diff.updates[0], row(2, "new"));
        assert_eq!(diff.deletes[0], row(3, "gone"));
    }

    #[test]
    fn diff_normalizes_integer_key_representations() {
        let target = vec![vec![Value::UInt(7), Value::Text("a".into())]];
        let mut differ = RowDiffer::new(vec![0], target);
        differ.push(vec![Value::Int(7), Value::Text("a".into())]);
        let diff = differ.finish();
        assert_eq!(diff.counts(), (0, 0, 0, 1));
    }

    #[test]
    fn diff_warns_on_null_and_duplicate_keys() {
        let target = vec![
            row(1, "a"),
            row(1, "b"),
            vec![Value::Null, Value::Text("x".into())],
        ];
        let mut differ = RowDiffer::new(vec![0], target);
        differ.push(row(1, "a"));
        differ.push(row(1, "c"));
        differ.push(vec![Value::Null, Value::Text("y".into())]);
        let diff = differ.finish();
        assert_eq!(diff.counts(), (0, 0, 0, 1));
        assert!(diff.warnings.iter().any(|warning| warning.contains("NULL")));
        assert!(diff.warnings.iter().any(|warning| warning.contains("重复")));
    }

    #[test]
    fn sync_statements_follow_flags_and_dialect() {
        let diff = TableDiff {
            inserts: vec![row(4, "new")],
            updates: vec![row(2, "new")],
            deletes: vec![row(3, "old")],
            same: 1,
            warnings: Vec::new(),
        };
        let columns = vec!["id".to_string(), "note".to_string()];
        let statements = sync_statements(
            DbKind::Postgres,
            &path("orders"),
            &columns,
            &[0],
            &diff,
            &DataCompareOptions::default(),
        )
        .unwrap();
        assert!(statements[0].starts_with("INSERT INTO"));
        assert!(statements[1].starts_with("UPDATE"));
        assert!(statements[1].contains("\"note\" = 'new'"));
        assert!(statements[2].starts_with("DELETE FROM"));
        assert!(statements[2].contains("\"id\" = 3"));

        let only_insert = DataCompareOptions {
            update: false,
            delete: false,
            ..DataCompareOptions::default()
        };
        let statements = sync_statements(
            DbKind::Sqlite,
            &path("orders"),
            &columns,
            &[0],
            &diff,
            &only_insert,
        )
        .unwrap();
        assert_eq!(statements.len(), 1);
        assert!(statements[0].contains("INSERT INTO"));
    }

    #[test]
    fn preview_lists_tables_and_truncates() {
        let results = vec![
            TableCompareResult {
                table: "orders".into(),
                insert: 1,
                update: 2,
                delete: 3,
                same: 4,
                warnings: Vec::new(),
                error: None,
            },
            TableCompareResult {
                table: "users".into(),
                error: Some("目标表不存在".into()),
                ..TableCompareResult::default()
            },
        ];
        let scripts = vec![vec!["INSERT INTO orders VALUES (1);".into()], Vec::new()];
        let lines = preview_lines(&results, &scripts, &["orders：冲突".into()], 1);
        assert!(lines.iter().any(|line| line.contains("orders")));
        assert!(lines.iter().any(|line| line.contains("INSERT INTO orders")));
        assert!(lines.iter().any(|line| line.contains("警告")));
        let summary = results[0].summary();
        assert!(summary.contains("新增 1"));
        assert!(results[1].summary().contains("目标表不存在"));
    }

    #[test]
    fn config_round_trips_and_rejects_bad_version() {
        let dir = std::env::temp_dir().join(format!(
            "rustdb-datacompare-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let path = data_compare_config_path(&dir);
        let mut config = DataCompareConfig::default();
        config.tables = vec![TableCompareConfig::new("orders")];
        export_data_compare_config(&path, &config).unwrap();
        assert_eq!(import_data_compare_config(&path).unwrap(), config);
        assert_eq!(load_data_compare_config(&path), config);

        let bad = dir.join("bad.json");
        std::fs::write(&bad, r#"{"version": 99, "tables": []}"#).unwrap();
        assert!(import_data_compare_config(&bad)
            .unwrap_err()
            .contains("版本"));
        assert_eq!(load_data_compare_config(&bad), DataCompareConfig::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn request_validation_rejects_empty_and_duplicate_keys() {
        let request = DataCompareRequest {
            source_session: SessionId("s".into()),
            target_profile: String::new(),
            source_ns: Namespace::new(Some("db"), None::<String>),
            target_ns: Namespace::new(Some("db"), None::<String>),
            tables: vec![TableCompareConfig::new("orders")],
            options: DataCompareOptions::default(),
        };
        assert!(request.validate().unwrap_err().contains("目标连接"));

        let mut request = request;
        request.target_profile = "t".into();
        request.tables[0].key_columns = vec!["id".into(), "ID".into()];
        assert!(request.validate().unwrap_err().contains("重复"));
    }
}
