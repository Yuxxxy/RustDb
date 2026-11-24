//! 跨库数据传输（F-75）：纯逻辑规划与 SQL 生成，无 IO。
//!
//! 口径：
//! * 先规划再执行：建表 SQL 与逐表列映射都在这里生成，UI 只负责预览与选择；
//! * 跨引擎只按**逻辑类型**映射（复用 `compare::native_type_for`），精度 / 默认值 /
//!   约束不保证等价，必须显式警告；
//! * `重新创建` 复用 `dump::drop_table_sql`，调用方负责先备份；
//! * Upsert 依赖目标表主键：MySQL `ON DUPLICATE KEY`、PG / SQLite `ON CONFLICT`、
//!   SQL Server / Oracle `MERGE`。

use crate::compare::native_type_for;
use crate::core::object_from_clause;
use crate::edit::{ident, literal};
use rustdb_db_api::{ColumnInfo, DbKind, LogicalType, Namespace, ObjectPath, SessionId, Value};

/// 单次传输最多涉及的源表数量（UI 侧同口径）。
pub const MAX_TRANSFER_TABLES: usize = 100;

/// 一次跨库传输的请求（源连接已建立，目标按配置临时建链）。
#[derive(Debug, Clone)]
pub struct TransferRequest {
    pub source_session: SessionId,
    pub target_profile: String,
    pub source_ns: Namespace,
    pub target_ns: Namespace,
    pub tables: Vec<String>,
    pub options: TransferOptions,
    /// `重建` 备份文件目录（通常复用结果导出目录）。
    pub backup_dir: std::path::PathBuf,
}

impl TransferRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.tables.is_empty() {
            return Err("至少选择一张表".into());
        }
        if self.tables.len() > MAX_TRANSFER_TABLES {
            return Err(format!(
                "单次最多传输 {MAX_TRANSFER_TABLES} 张表，当前 {} 张",
                self.tables.len()
            ));
        }
        if self.target_profile.trim().is_empty() {
            return Err("请选择目标连接".into());
        }
        Ok(())
    }
}
/// 默认每批行数（需求 4.9：批量 1000）。
pub const DEFAULT_BATCH_ROWS: usize = 1000;
/// 默认单语句超时。
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransferMode {
    /// 目标表必须不存在：按源结构建表后写入。
    #[default]
    Create,
    /// 目标表存在则先备份、再删除重建；不存在则直接创建。
    Recreate,
    /// 目标表必须存在：只写共同列。
    Append,
    /// 目标表必须存在且有主键：按主键插入或更新。
    Upsert,
}

impl TransferMode {
    pub const ALL: [TransferMode; 4] = [Self::Create, Self::Recreate, Self::Append, Self::Upsert];

    pub fn label(self) -> &'static str {
        match self {
            Self::Create => "建表",
            Self::Recreate => "重建",
            Self::Append => "追加",
            Self::Upsert => "Upsert",
        }
    }

    /// 需要先执行 DDL 的模式。
    pub fn needs_ddl(self) -> bool {
        matches!(self, Self::Create | Self::Recreate)
    }

    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|item| *item == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum CaseStrategy {
    /// 保持源名称。
    #[default]
    Preserve,
    Lower,
    Upper,
}

impl CaseStrategy {
    pub const ALL: [CaseStrategy; 3] = [Self::Preserve, Self::Lower, Self::Upper];

    pub fn label(self) -> &'static str {
        match self {
            Self::Preserve => "保持",
            Self::Lower => "转小写",
            Self::Upper => "转大写",
        }
    }

    pub fn apply(self, name: &str) -> String {
        match self {
            Self::Preserve => name.to_string(),
            Self::Lower => name.to_lowercase(),
            Self::Upper => name.to_uppercase(),
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|item| *item == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransferOptions {
    pub mode: TransferMode,
    pub batch_rows: usize,
    pub timeout_ms: Option<u64>,
    pub case: CaseStrategy,
    /// `重建` 前自动导出目标表到备份文件。
    pub backup: bool,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            mode: TransferMode::Create,
            batch_rows: DEFAULT_BATCH_ROWS,
            timeout_ms: Some(DEFAULT_TIMEOUT_MS),
            case: CaseStrategy::Preserve,
            backup: true,
        }
    }
}

impl TransferOptions {
    /// 方言夹取后的每批行数：Oracle 只支持单行 `VALUES` / 单行合并源，
    /// SQL Server 单条语句最多 1000 行；其余沿用设定值。
    pub fn effective_batch(&self, kind: DbKind) -> usize {
        let configured = self.batch_rows.max(1);
        match kind {
            DbKind::Oracle => 1,
            DbKind::Mssql => configured.min(1000),
            _ => configured,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnPlan {
    pub source: String,
    pub target: String,
    pub logical_type: LogicalType,
    pub native_type: String,
    pub nullable: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransferTablePlan {
    pub source: ObjectPath,
    pub target: ObjectPath,
    pub mode: TransferMode,
    pub target_exists: bool,
    pub columns: Vec<ColumnPlan>,
    /// 目标表主键列（Upsert 的匹配键）。
    pub key_columns: Vec<String>,
    pub drop_sql: Option<String>,
    pub create_sql: Option<String>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
}

impl TransferTablePlan {
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

    pub fn structure_sql(&self) -> Vec<String> {
        let mut statements = Vec::new();
        if let Some(sql) = &self.drop_sql {
            statements.push(sql.clone());
        }
        if let Some(sql) = &self.create_sql {
            statements.push(sql.clone());
        }
        statements
    }
}

/// 执行前预览：逐表结构 SQL、列映射与警告（纯文本，供 `TransferEvent::Preview`）。
pub fn preview_lines(plans: &[TransferTablePlan]) -> (Vec<String>, Vec<String>) {
    let mut lines = Vec::new();
    let mut warnings = Vec::new();
    for plan in plans {
        lines.push(format!(
            "表 {} → {}（{}{}）",
            plan.source.name,
            plan.target.name,
            plan.mode.label(),
            if plan.target_exists {
                "，目标已存在"
            } else {
                "，目标新建"
            }
        ));
        if let Some(error) = &plan.error {
            warnings.push(format!("{}：{error}", plan.source.name));
            continue;
        }
        for sql in plan.structure_sql() {
            for line in sql.lines() {
                lines.push(format!("  {line}"));
            }
        }
        if !plan.columns.is_empty() {
            let columns = plan
                .columns
                .iter()
                .map(|column| {
                    format!(
                        "{} → {} {}",
                        column.source, column.target, column.native_type
                    )
                })
                .collect::<Vec<_>>()
                .join("，");
            lines.push(format!("  列：{columns}"));
        }
        if !plan.key_columns.is_empty() {
            lines.push(format!("  主键：{}", plan.key_columns.join("，")));
        }
        for warning in &plan.warnings {
            warnings.push(format!("{}：{warning}", plan.source.name));
        }
    }
    (lines, warnings)
}

pub(crate) fn plan_failure(
    source: &ObjectPath,
    target: &ObjectPath,
    mode: TransferMode,
    target_exists: bool,
    message: impl Into<String>,
) -> TransferTablePlan {
    TransferTablePlan {
        source: source.clone(),
        target: target.clone(),
        mode,
        target_exists,
        columns: Vec::new(),
        key_columns: Vec::new(),
        drop_sql: None,
        create_sql: None,
        warnings: Vec::new(),
        error: Some(message.into()),
    }
}

/// 目标表名：按大小写策略转换。
pub fn target_table_name(source: &str, options: &TransferOptions) -> String {
    options.case.apply(source)
}

/// 在目标对象列表里按名称查找表（不区分大小写）；返回实际名称。
pub fn find_target_table<'a>(
    source: &str,
    options: &TransferOptions,
    target_names: &'a [String],
) -> Option<&'a String> {
    let wanted = target_table_name(source, options);
    target_names
        .iter()
        .find(|name| name.eq_ignore_ascii_case(&wanted))
}

/// 生成逐表计划。`target_columns` 为 `None` 表示目标表不存在。
#[allow(clippy::too_many_arguments)]
pub fn plan_table(
    source: &ObjectPath,
    source_columns: &[ColumnInfo],
    target: &ObjectPath,
    target_columns: Option<&[ColumnInfo]>,
    source_kind: DbKind,
    target_kind: DbKind,
    options: &TransferOptions,
) -> TransferTablePlan {
    let mode = options.mode;
    let target_exists = target_columns.is_some();
    if source_columns.is_empty() {
        return plan_failure(source, target, mode, target_exists, "源表没有可读取的列");
    }

    match mode {
        TransferMode::Create | TransferMode::Recreate => {
            if mode == TransferMode::Create && target_exists {
                return plan_failure(
                    source,
                    target,
                    mode,
                    true,
                    "目标表已存在；请选择「重建」或「追加」",
                );
            }
            let mut warnings = Vec::new();
            if source_kind != target_kind {
                warnings.push(format!(
                    "跨引擎传输（{} → {}）：类型按逻辑类型映射，精度 / 默认值 / 约束可能不一致",
                    source_kind.display_name(),
                    target_kind.display_name()
                ));
            }
            let mut columns = Vec::with_capacity(source_columns.len());
            for column in source_columns {
                let target_name = options.case.apply(&column.meta.name);
                let native = match native_type_for(target_kind, &column.meta.logical_type) {
                    Some(native) => native,
                    None => {
                        warnings.push(format!(
                            "列 {}：逻辑类型 {} 无法映射到 {}，沿用源原生类型 {}",
                            column.meta.name,
                            column.meta.logical_type.label(),
                            target_kind.display_name(),
                            column.meta.native_type
                        ));
                        column.meta.native_type.clone()
                    }
                };
                if source_kind != target_kind && native != column.meta.native_type {
                    warnings.push(format!(
                        "列 {}：源类型 {} → 目标 {}",
                        column.meta.name, column.meta.native_type, native
                    ));
                }
                columns.push(ColumnPlan {
                    source: column.meta.name.to_string(),
                    target: target_name,
                    logical_type: column.meta.logical_type.clone(),
                    native_type: native,
                    nullable: column.meta.nullable,
                    primary_key: column.is_primary_key,
                });
            }
            let key_columns = columns
                .iter()
                .filter(|column| column.primary_key)
                .map(|column| column.target.clone())
                .collect::<Vec<_>>();
            let create_sql = create_table_sql(target_kind, target, &columns);
            let drop_sql = (mode == TransferMode::Recreate && target_exists)
                .then(|| crate::dump::drop_table_sql(target_kind, target));
            TransferTablePlan {
                source: source.clone(),
                target: target.clone(),
                mode,
                target_exists,
                columns,
                key_columns,
                drop_sql,
                create_sql: Some(create_sql),
                warnings,
                error: None,
            }
        }
        TransferMode::Append | TransferMode::Upsert => {
            let Some(target_columns) = target_columns else {
                return plan_failure(
                    source,
                    target,
                    mode,
                    false,
                    "目标表不存在；请选择「建表」或「重建」",
                );
            };
            if target_columns.is_empty() {
                return plan_failure(source, target, mode, true, "目标表没有可写入的列");
            }
            let mut warnings = Vec::new();
            let mut columns = Vec::new();
            for column in source_columns {
                let Some(found) = target_columns
                    .iter()
                    .find(|candidate| candidate.meta.name.eq_ignore_ascii_case(&column.meta.name))
                else {
                    warnings.push(format!("目标缺少列 {}，已跳过", column.meta.name));
                    continue;
                };
                if found.meta.logical_type != column.meta.logical_type {
                    warnings.push(format!(
                        "列 {} 类型不同：源 {} → 目标 {}（按目标类型写入）",
                        column.meta.name,
                        column.meta.logical_type.label(),
                        found.meta.logical_type.label()
                    ));
                }
                columns.push(ColumnPlan {
                    source: column.meta.name.to_string(),
                    target: found.meta.name.to_string(),
                    logical_type: found.meta.logical_type.clone(),
                    native_type: found.meta.native_type.clone(),
                    nullable: found.meta.nullable,
                    primary_key: found.is_primary_key,
                });
            }
            if columns.is_empty() {
                return plan_failure(source, target, mode, true, "源表与目标表没有同名可写列");
            }
            let mapped = |name: &str| {
                columns
                    .iter()
                    .any(|column| column.target.eq_ignore_ascii_case(name))
            };
            let key_columns = target_columns
                .iter()
                .filter(|column| column.is_primary_key)
                .map(|column| column.meta.name.to_string())
                .filter(|name| mapped(name))
                .collect::<Vec<_>>();
            if mode == TransferMode::Upsert {
                if key_columns.is_empty() {
                    return plan_failure(
                        source,
                        target,
                        mode,
                        true,
                        "目标表没有可用的主键列，无法 Upsert",
                    );
                }
                let missing = target_columns
                    .iter()
                    .filter(|column| column.is_primary_key)
                    .map(|column| column.meta.name.to_string())
                    .find(|name| !mapped(name));
                if let Some(name) = missing {
                    return plan_failure(
                        source,
                        target,
                        mode,
                        true,
                        format!("目标主键列 {name} 不在源表中，无法 Upsert"),
                    );
                }
            }
            TransferTablePlan {
                source: source.clone(),
                target: target.clone(),
                mode,
                target_exists: true,
                columns,
                key_columns,
                drop_sql: None,
                create_sql: None,
                warnings,
                error: None,
            }
        }
    }
}

fn create_table_sql(kind: DbKind, path: &ObjectPath, columns: &[ColumnPlan]) -> String {
    let mut definitions = Vec::with_capacity(columns.len() + 1);
    for column in columns {
        let mut definition = format!("{} {}", ident(kind, &column.target), column.native_type);
        if !column.nullable {
            definition.push_str(" NOT NULL");
        }
        definitions.push(definition);
    }
    let keys = columns
        .iter()
        .filter(|column| column.primary_key)
        .map(|column| ident(kind, &column.target))
        .collect::<Vec<_>>();
    if !keys.is_empty() {
        definitions.push(format!("PRIMARY KEY ({})", keys.join(", ")));
    }
    format!(
        "CREATE TABLE {} (\n  {}\n);",
        object_from_clause(kind, path),
        definitions.join(",\n  ")
    )
}

/// 生成一批写入语句（每批一条）。
pub fn insert_sql(
    kind: DbKind,
    path: &ObjectPath,
    columns: &[String],
    rows: &[Vec<Value>],
    mode: TransferMode,
    keys: &[String],
) -> Result<String, String> {
    if rows.is_empty() {
        return Err("没有可写入的行".into());
    }
    if columns.is_empty() {
        return Err("没有可写入的列".into());
    }
    let table = object_from_clause(kind, path);
    let names = columns
        .iter()
        .map(|name| ident(kind, name))
        .collect::<Vec<_>>();
    let mut literal_rows: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    for row in rows {
        if row.len() != columns.len() {
            return Err(format!(
                "行宽不一致：期望 {} 列，实际 {} 列",
                columns.len(),
                row.len()
            ));
        }
        let mut literals = Vec::with_capacity(row.len());
        for value in row {
            literals.push(literal(kind, value)?);
        }
        literal_rows.push(literals);
    }
    let values = literal_rows
        .iter()
        .map(|row| format!("({})", row.join(", ")))
        .collect::<Vec<_>>();
    let base = format!(
        "INSERT INTO {table} ({}) VALUES\n{};",
        names.join(", "),
        values.join(",\n")
    );
    if mode != TransferMode::Upsert {
        return Ok(base);
    }
    if keys.is_empty() {
        return Err("Upsert 需要主键列".into());
    }

    let non_keys = columns
        .iter()
        .filter(|name| !keys.iter().any(|key| key.eq_ignore_ascii_case(name)))
        .cloned()
        .collect::<Vec<_>>();
    let key_names = keys
        .iter()
        .map(|name| ident(kind, name))
        .collect::<Vec<_>>();

    Ok(match kind {
        DbKind::Mysql | DbKind::Mariadb => {
            let update = if non_keys.is_empty() {
                format!("{} = {}", key_names[0], key_names[0])
            } else {
                non_keys
                    .iter()
                    .map(|name| {
                        let quoted = ident(kind, name);
                        format!("{quoted} = VALUES({quoted})")
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!("{base}\nON DUPLICATE KEY UPDATE {update};")
        }
        DbKind::Postgres | DbKind::Sqlite => {
            let action = if non_keys.is_empty() {
                "DO NOTHING".to_string()
            } else {
                format!(
                    "DO UPDATE SET {}",
                    non_keys
                        .iter()
                        .map(|name| {
                            let quoted = ident(kind, name);
                            format!("{quoted} = excluded.{quoted}")
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            format!("{base}\nON CONFLICT ({}) {action};", key_names.join(", "))
        }
        DbKind::Mssql => {
            let source_columns = names.join(", ");
            let on = keys
                .iter()
                .zip(key_names.iter())
                .map(|(name, quoted)| format!("tgt.{quoted} = src.{}", ident(kind, name)))
                .collect::<Vec<_>>()
                .join(" AND ");
            let update = non_keys
                .iter()
                .map(|name| {
                    let quoted = ident(kind, name);
                    format!("tgt.{quoted} = src.{quoted}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            let source_values = literal_rows
                .iter()
                .map(|row| format!("({})", row.join(", ")))
                .collect::<Vec<_>>()
                .join(",\n  ");
            let mut sql = format!(
                "MERGE INTO {table} AS tgt USING (VALUES\n  {source_values}\n) AS src ({source_columns}) ON {on}"
            );
            if !update.is_empty() {
                sql.push_str(&format!("\nWHEN MATCHED THEN UPDATE SET {update}"));
            }
            let insert_values = columns
                .iter()
                .map(|name| format!("src.{}", ident(kind, name)))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(&format!(
                "\nWHEN NOT MATCHED THEN INSERT ({source_columns}) VALUES ({insert_values});"
            ));
            sql
        }
        DbKind::Oracle => {
            let select_rows = literal_rows
                .iter()
                .map(|row| {
                    let cells = row
                        .iter()
                        .zip(columns.iter())
                        .map(|(cell, name)| format!("{cell} AS {}", ident(kind, name)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("SELECT {cells} FROM dual")
                })
                .collect::<Vec<_>>()
                .join(" UNION ALL ");
            let on = keys
                .iter()
                .map(|name| {
                    let quoted = ident(kind, name);
                    format!("tgt.{quoted} = src.{quoted}")
                })
                .collect::<Vec<_>>()
                .join(" AND ");
            let update = non_keys
                .iter()
                .map(|name| {
                    let quoted = ident(kind, name);
                    format!("tgt.{quoted} = src.{quoted}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            let insert_values = columns
                .iter()
                .map(|name| format!("src.{}", ident(kind, name)))
                .collect::<Vec<_>>()
                .join(", ");
            let mut sql = format!("MERGE INTO {table} tgt USING ({select_rows}) src ON ({on})");
            if !update.is_empty() {
                sql.push_str(&format!("\nWHEN MATCHED THEN UPDATE SET {update}"));
            }
            sql.push_str(&format!(
                "\nWHEN NOT MATCHED THEN INSERT ({}) VALUES ({insert_values})",
                names.join(", ")
            ));
            sql
        }
    })
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

    fn path(name: &str) -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("db"), Some("public")),
            name,
            ObjectKind::Table,
        )
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

    #[test]
    fn defaults_are_safe() {
        let options = TransferOptions::default();
        assert_eq!(options.mode, TransferMode::Create);
        assert_eq!(options.batch_rows, DEFAULT_BATCH_ROWS);
        assert!(options.backup);
        assert_eq!(options.case, CaseStrategy::Preserve);
        assert_eq!(options.effective_batch(DbKind::Postgres), 1000);
        assert_eq!(options.effective_batch(DbKind::Oracle), 1);
        assert_eq!(options.effective_batch(DbKind::Mssql), 1000);
    }

    #[test]
    fn case_strategy_changes_target_names() {
        let source = path("Orders");
        let columns = vec![column("Id", int(), true), column("Note", text(), false)];
        let options = TransferOptions {
            case: CaseStrategy::Lower,
            ..TransferOptions::default()
        };
        let target = path("orders");
        let plan = plan_table(
            &source,
            &columns,
            &target,
            None,
            DbKind::Postgres,
            DbKind::Postgres,
            &options,
        );
        assert!(plan.error.is_none(), "{:?}", plan.error);
        assert_eq!(plan.columns[1].target, "note");
        assert_eq!(plan.key_columns, vec!["id".to_string()]);
    }

    #[test]
    fn create_plan_maps_types_and_primary_key() {
        let source = path("orders");
        let columns = vec![column("id", int(), true), column("note", text(), false)];
        let target = path("orders");
        let plan = plan_table(
            &source,
            &columns,
            &target,
            None,
            DbKind::Postgres,
            DbKind::Mssql,
            &TransferOptions::default(),
        );
        assert!(plan.error.is_none());
        assert_eq!(plan.columns[0].native_type, "bigint");
        assert!(plan.create_sql.as_deref().unwrap().contains("PRIMARY KEY"));
        assert!(plan
            .create_sql
            .as_deref()
            .unwrap()
            .contains("[id] bigint NOT NULL"));
        assert!(plan
            .warnings
            .iter()
            .any(|warning| warning.contains("跨引擎")));
        assert!(plan.drop_sql.is_none());
    }

    #[test]
    fn create_rejects_existing_target() {
        let columns = vec![column("id", int(), true)];
        let target_columns = vec![column("id", int(), true)];
        let plan = plan_table(
            &path("orders"),
            &columns,
            &path("orders"),
            Some(&target_columns),
            DbKind::Postgres,
            DbKind::Postgres,
            &TransferOptions::default(),
        );
        assert!(plan.error.unwrap().contains("已存在"));
    }

    #[test]
    fn recreate_drops_then_creates() {
        let columns = vec![column("id", int(), true)];
        let target_columns = vec![column("id", int(), true)];
        let options = TransferOptions {
            mode: TransferMode::Recreate,
            ..TransferOptions::default()
        };
        let plan = plan_table(
            &path("orders"),
            &columns,
            &path("orders"),
            Some(&target_columns),
            DbKind::Postgres,
            DbKind::Oracle,
            &options,
        );
        assert!(plan.error.is_none());
        assert!(plan.drop_sql.unwrap().contains("CASCADE CONSTRAINTS"));
        assert!(plan.create_sql.unwrap().contains("CREATE TABLE"));
    }

    #[test]
    fn append_maps_common_columns_and_warns_missing() {
        let source_columns = vec![
            column("id", int(), true),
            column("note", text(), false),
            column("extra", text(), false),
        ];
        let target_columns = vec![
            column("ID", int(), true),
            column("NOTE", LogicalType::Text { max_len: Some(64) }, false),
        ];
        let options = TransferOptions {
            mode: TransferMode::Append,
            ..TransferOptions::default()
        };
        let plan = plan_table(
            &path("orders"),
            &source_columns,
            &path("orders"),
            Some(&target_columns),
            DbKind::Postgres,
            DbKind::Mysql,
            &options,
        );
        assert!(plan.error.is_none());
        assert_eq!(plan.columns.len(), 2);
        assert_eq!(plan.columns[0].target, "ID");
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
    fn append_requires_existing_target() {
        let options = TransferOptions {
            mode: TransferMode::Append,
            ..TransferOptions::default()
        };
        let plan = plan_table(
            &path("orders"),
            &[column("id", int(), true)],
            &path("orders"),
            None,
            DbKind::Postgres,
            DbKind::Postgres,
            &options,
        );
        assert!(plan.error.unwrap().contains("不存在"));
    }

    #[test]
    fn upsert_requires_primary_key() {
        let source_columns = vec![column("id", int(), false)];
        let target_columns = vec![column("id", int(), false)];
        let options = TransferOptions {
            mode: TransferMode::Upsert,
            ..TransferOptions::default()
        };
        let plan = plan_table(
            &path("orders"),
            &source_columns,
            &path("orders"),
            Some(&target_columns),
            DbKind::Postgres,
            DbKind::Postgres,
            &options,
        );
        assert!(plan.error.unwrap().contains("主键"));
    }

    #[test]
    fn upsert_sql_follows_dialect() {
        let rows = vec![
            vec![Value::Int(1), Value::Text("a".into())],
            vec![Value::Int(2), Value::Text("b".into())],
        ];
        let mysql = insert_sql(
            DbKind::Mysql,
            &path("orders"),
            &["id".into(), "note".into()],
            &rows,
            TransferMode::Upsert,
            &["id".into()],
        )
        .unwrap();
        assert!(mysql.contains("ON DUPLICATE KEY UPDATE"));
        assert!(mysql.contains("`note` = VALUES(`note`)"));

        let pg = insert_sql(
            DbKind::Postgres,
            &path("orders"),
            &["id".into(), "note".into()],
            &rows,
            TransferMode::Upsert,
            &["id".into()],
        )
        .unwrap();
        assert!(pg.contains("ON CONFLICT (\"id\") DO UPDATE SET"));
        assert!(pg.contains("excluded.\"note\""));

        let mssql = insert_sql(
            DbKind::Mssql,
            &path("orders"),
            &["id".into(), "note".into()],
            &rows,
            TransferMode::Upsert,
            &["id".into()],
        )
        .unwrap();
        assert!(mssql.starts_with("MERGE INTO"));
        assert!(mssql.contains("WHEN MATCHED THEN UPDATE SET tgt.[note] = src.[note]"));
        assert!(mssql
            .contains("WHEN NOT MATCHED THEN INSERT ([id], [note]) VALUES (src.[id], src.[note])"));

        let oracle = insert_sql(
            DbKind::Oracle,
            &path("orders"),
            &["id".into(), "note".into()],
            &rows,
            TransferMode::Upsert,
            &["id".into()],
        )
        .unwrap();
        assert!(oracle.contains("USING (SELECT 1 AS \"id\", 'a' AS \"note\" FROM dual"));
        assert!(oracle.contains("WHEN NOT MATCHED THEN INSERT"));
    }

    #[test]
    fn plain_insert_batches_rows() {
        let rows = vec![
            vec![Value::Int(1), Value::Text("a".into())],
            vec![Value::Int(2), Value::Null],
        ];
        let sql = insert_sql(
            DbKind::Sqlite,
            &path("orders"),
            &["id".into(), "note".into()],
            &rows,
            TransferMode::Append,
            &[],
        )
        .unwrap();
        assert!(sql.contains("(1, 'a')"));
        assert!(sql.contains("(2, NULL)"));
        assert!(!sql.contains("ON CONFLICT"));
    }

    #[test]
    fn insert_rejects_width_mismatch() {
        let err = insert_sql(
            DbKind::Postgres,
            &path("orders"),
            &["id".into(), "note".into()],
            &[vec![Value::Int(1)]],
            TransferMode::Append,
            &[],
        )
        .unwrap_err();
        assert!(err.contains("行宽"));
    }

    #[test]
    fn target_lookup_is_case_insensitive() {
        let names = vec!["Orders".to_string(), "users".to_string()];
        let options = TransferOptions::default();
        assert_eq!(
            find_target_table("orders", &options, &names),
            Some(&"Orders".to_string())
        );
        assert_eq!(find_target_table("missing", &options, &names), None);
    }

    #[test]
    fn preview_lists_structure_columns_and_warnings() {
        let columns = vec![column("id", int(), true), column("note", text(), false)];
        let plan = plan_table(
            &path("orders"),
            &columns,
            &path("orders"),
            None,
            DbKind::Postgres,
            DbKind::Mssql,
            &TransferOptions::default(),
        );
        let (lines, warnings) = preview_lines(&[plan]);
        assert!(lines[0].contains("orders → orders"));
        assert!(lines.iter().any(|line| line.contains("CREATE TABLE")));
        assert!(lines.iter().any(|line| line.contains("id → id bigint")));
        assert!(lines.iter().any(|line| line.contains("主键：id")));
        assert!(warnings.iter().any(|warning| warning.contains("跨引擎")));
    }

    #[test]
    fn preview_reports_plan_errors_as_warnings() {
        let columns = vec![column("id", int(), true)];
        let target_columns = vec![column("id", int(), true)];
        let plan = plan_table(
            &path("orders"),
            &columns,
            &path("orders"),
            Some(&target_columns),
            DbKind::Postgres,
            DbKind::Postgres,
            &TransferOptions::default(),
        );
        let (lines, warnings) = preview_lines(&[plan]);
        assert_eq!(lines.len(), 1);
        assert!(warnings[0].contains("已存在"));
    }
}
