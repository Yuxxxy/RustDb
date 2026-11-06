//! 结构对比（F-86）：快照 → 差异 → 部署 / 回滚计划。
//!
//! 纯逻辑、无 IO（读写 JSON 除外）。方向固定：**源是参照（期望）**，
//! **目标是待修改的实际库**。所有部署语句复用 [`crate::migration`] 的
//! 方言规则，UI 仍然不需要 `match DbKind`。
//!
//! 约束（需求 19.4）：
//! * 重命名只做候选；用户确认后写进 [`CompareConfig::renames`] 才会生成 RENAME；
//! * 部署脚本默认不执行，先生成影响与回滚建议；
//! * 跨方言只按逻辑类型映射，映射不了的列显式进入 `unsupported`，不猜。

use rustdb_db_api::{
    ColumnInfo, DbKind, ForeignKeyInfo, IndexInfo, LogicalType, Namespace, ObjectKind, ObjectPath,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::core::object_from_clause;
use crate::edit::ident;
use crate::migration::{
    plan_add_foreign_key, plan_add_index, plan_changes, plan_drop_foreign_key, plan_drop_index,
    ColumnDraft, ForeignKeyDraft, IndexDraft, MigrationPlan, MigrationStatement,
};

pub const COMPARE_CONFIG_VERSION: u32 = 1;
/// 单次对比最多加载的表数；超出只计数，不静默隐藏。
pub const MAX_COMPARE_TABLES: usize = 500;
/// 重命名候选最多展示多少条，避免大库噪声。
pub const MAX_RENAME_CANDIDATES: usize = 50;

// --- 快照 ---

/// 一个命名空间的结构快照，可保存为 JSON 供离线 / 跨实例对比。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaSnapshot {
    pub driver: DbKind,
    pub namespace: Namespace,
    #[serde(default)]
    pub tables: Vec<TableSnapshot>,
    /// 因 [`MAX_COMPARE_TABLES`] 未加载的表数。
    #[serde(default)]
    pub skipped: usize,
    /// 逐表取详情失败的原因（权限 / 驱动不支持等），不静默丢。
    #[serde(default)]
    pub failed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableSnapshot {
    pub name: String,
    #[serde(default)]
    pub columns: Vec<ColumnInfo>,
    #[serde(default)]
    pub indexes: Vec<IndexInfo>,
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKeyInfo>,
}

impl SchemaSnapshot {
    pub fn table(&self, name: &str) -> Option<&TableSnapshot> {
        self.tables
            .iter()
            .find(|table| table.name.eq_ignore_ascii_case(name))
    }
}

// --- 差异 ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffStatus {
    Added,
    Removed,
    Changed,
    Same,
}

impl DiffStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "新增",
            Self::Removed => "删除",
            Self::Changed => "修改",
            Self::Same => "一致",
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Added => "+",
            Self::Removed => "-",
            Self::Changed => "~",
            Self::Same => "=",
        }
    }
}

/// 列的可比较形状；不携带驱动私有结构，方便 JSON 保存。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnShape {
    pub name: String,
    pub logical_type: LogicalType,
    pub native_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary_key: bool,
    pub auto_increment: bool,
}

impl ColumnShape {
    fn from_info(info: &ColumnInfo) -> Self {
        Self {
            name: info.meta.name.to_string(),
            logical_type: info.meta.logical_type.clone(),
            native_type: info.meta.native_type.clone(),
            nullable: info.meta.nullable,
            default: info.default.clone(),
            primary_key: info.is_primary_key,
            auto_increment: info.is_auto_increment,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDiff {
    pub name: String,
    pub status: DiffStatus,
    /// 目标侧现状（新增列没有）。
    pub before: Option<ColumnShape>,
    /// 源侧期望（删除列没有）。
    pub after: Option<ColumnShape>,
    /// 面向用户的变更描述。
    #[serde(default)]
    pub changes: Vec<String>,
    /// 跨方言映射 / 不能自动部署的补充说明。
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexDiff {
    pub name: String,
    pub status: DiffStatus,
    pub before: Option<IndexInfo>,
    pub after: Option<IndexInfo>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForeignKeyDiff {
    pub name: String,
    pub status: DiffStatus,
    pub before: Option<ForeignKeyInfo>,
    pub after: Option<ForeignKeyInfo>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableDiff {
    /// 源（参照）侧表名；只在目标存在时为 `None`。
    pub source_name: Option<String>,
    /// 目标（待修改）侧表名；只在源存在时为 `None`。
    pub target_name: Option<String>,
    pub status: DiffStatus,
    #[serde(default)]
    pub columns: Vec<ColumnDiff>,
    #[serde(default)]
    pub indexes: Vec<IndexDiff>,
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKeyDiff>,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl TableDiff {
    pub fn name(&self) -> &str {
        self.source_name
            .as_deref()
            .or(self.target_name.as_deref())
            .unwrap_or("?")
    }

    pub fn is_renamed(&self) -> bool {
        match (&self.source_name, &self.target_name) {
            (Some(source), Some(target)) => !source.eq_ignore_ascii_case(target),
            _ => false,
        }
    }

    pub fn source(&self) -> Option<&str> {
        self.source_name.as_deref()
    }

    pub fn target(&self) -> Option<&str> {
        self.target_name.as_deref()
    }

    fn reversed(&self) -> Self {
        let mut reversed = TableDiff {
            source_name: self.target_name.clone(),
            target_name: self.source_name.clone(),
            status: match self.status {
                DiffStatus::Added => DiffStatus::Removed,
                DiffStatus::Removed => DiffStatus::Added,
                other => other,
            },
            columns: self
                .columns
                .iter()
                .map(|column| ColumnDiff {
                    name: column.name.clone(),
                    status: match column.status {
                        DiffStatus::Added => DiffStatus::Removed,
                        DiffStatus::Removed => DiffStatus::Added,
                        other => other,
                    },
                    before: column.after.clone(),
                    after: column.before.clone(),
                    changes: match column.status {
                        DiffStatus::Added => vec!["删除列".into()],
                        DiffStatus::Removed => vec!["新增列".into()],
                        DiffStatus::Changed => column
                            .after
                            .as_ref()
                            .zip(column.before.as_ref())
                            .map(|(old, new)| describe_column_changes(old, new, false))
                            .unwrap_or_default(),
                        DiffStatus::Same => Vec::new(),
                    },
                    notes: Vec::new(),
                })
                .collect(),
            indexes: self
                .indexes
                .iter()
                .map(|index| IndexDiff {
                    name: index.name.clone(),
                    status: reverse_status(index.status),
                    before: index.after.clone(),
                    after: index.before.clone(),
                    note: index.note.clone(),
                })
                .collect(),
            foreign_keys: self
                .foreign_keys
                .iter()
                .map(|fk| ForeignKeyDiff {
                    name: fk.name.clone(),
                    status: reverse_status(fk.status),
                    before: fk.after.clone(),
                    after: fk.before.clone(),
                    note: fk.note.clone(),
                })
                .collect(),
            notes: Vec::new(),
        };
        if reversed.is_renamed() {
            reversed.notes.push(format!(
                "回滚重命名：{} → {}",
                reversed.source().unwrap_or("?"),
                reversed.target().unwrap_or("?")
            ));
        }
        reversed
    }
}

fn reverse_status(status: DiffStatus) -> DiffStatus {
    match status {
        DiffStatus::Added => DiffStatus::Removed,
        DiffStatus::Removed => DiffStatus::Added,
        other => other,
    }
}

/// 列结构一致但表名不同：只做候选，不自动当重命名执行。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenameCandidate {
    /// 目标侧现有表名。
    pub from: String,
    /// 源侧期望表名。
    pub to: String,
    pub matched_columns: usize,
    pub total_columns: usize,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompareReport {
    pub source_driver: DbKind,
    pub source_namespace: Namespace,
    pub target_driver: DbKind,
    pub target_namespace: Namespace,
    #[serde(default)]
    pub tables: Vec<TableDiff>,
    #[serde(default)]
    pub rename_candidates: Vec<RenameCandidate>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffCounts {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
    pub same: usize,
    pub columns_changed: usize,
}

impl CompareReport {
    pub fn is_empty(&self) -> bool {
        self.tables
            .iter()
            .all(|table| table.status == DiffStatus::Same)
    }

    pub fn counts(&self) -> DiffCounts {
        let mut counts = DiffCounts::default();
        for table in &self.tables {
            match table.status {
                DiffStatus::Added => counts.added += 1,
                DiffStatus::Removed => counts.removed += 1,
                DiffStatus::Changed => counts.changed += 1,
                DiffStatus::Same => counts.same += 1,
            }
            counts.columns_changed += table
                .columns
                .iter()
                .filter(|column| column.status != DiffStatus::Same)
                .count();
        }
        counts
    }

    /// 人类可读的差异摘要（不含部署 SQL）。
    pub fn preview_text(&self) -> String {
        let counts = self.counts();
        let mut lines = vec![format!(
            "-- 结构对比：{}（{}）→ {}（{}）{}",
            self.source_namespace.display_label(),
            self.source_driver.display_name(),
            self.target_namespace.display_label(),
            self.target_driver.display_name(),
            if self.source_driver != self.target_driver {
                "（跨方言）"
            } else {
                ""
            }
        )];
        lines.push(format!(
            "-- 表：{} 新增 / {} 删除 / {} 修改 / {} 一致；列变更 {}",
            counts.added, counts.removed, counts.changed, counts.same, counts.columns_changed
        ));
        for table in &self.tables {
            if table.status == DiffStatus::Same {
                continue;
            }
            let mut header = format!(
                "{} {}（{}）",
                table.status.symbol(),
                table.name(),
                table.status.label()
            );
            if table.is_renamed() {
                header.push_str(&format!(
                    "：{} → {}",
                    table.target().unwrap_or("?"),
                    table.source().unwrap_or("?")
                ));
            }
            lines.push(header);
            for column in &table.columns {
                if column.status == DiffStatus::Same {
                    continue;
                }
                match column.status {
                    DiffStatus::Added => lines.push(format!("    + 列 {}", column.name)),
                    DiffStatus::Removed => lines.push(format!("    - 列 {}", column.name)),
                    DiffStatus::Changed => {
                        lines.push(format!(
                            "    ~ 列 {}：{}",
                            column.name,
                            column.changes.join("；")
                        ));
                    }
                    DiffStatus::Same => {}
                }
                for note in &column.notes {
                    lines.push(format!("      · {note}"));
                }
            }
            for index in &table.indexes {
                if index.status != DiffStatus::Same {
                    lines.push(format!(
                        "    {} 索引 {}（{}）",
                        index.status.symbol(),
                        index.name,
                        index.status.label()
                    ));
                }
            }
            for fk in &table.foreign_keys {
                if fk.status != DiffStatus::Same {
                    lines.push(format!(
                        "    {} 外键 {}（{}）",
                        fk.status.symbol(),
                        fk.name,
                        fk.status.label()
                    ));
                }
            }
            for note in &table.notes {
                lines.push(format!("    · {note}"));
            }
        }
        for candidate in &self.rename_candidates {
            lines.push(format!(
                "r 重命名候选：{} → {}（{}）",
                candidate.from, candidate.to, candidate.reason
            ));
        }
        for warning in &self.warnings {
            lines.push(format!("! {warning}"));
        }
        lines.join("\n")
    }

    /// 生成部署（源 → 目标）与回滚建议。纯计划，不执行。
    pub fn deploy_plan(&self, options: &DeployOptions) -> SchemaDeploy {
        let plan = self.build_plan(options);
        let rollback = self.reversed().build_plan(&DeployOptions::full());
        let mut impact = self.build_impact(options);
        if !rollback.statements.is_empty() {
            impact.push("已生成反向回滚脚本：只恢复结构，不恢复已删除的数据。".into());
        }
        SchemaDeploy {
            plan,
            rollback,
            impact,
        }
    }

    fn reversed(&self) -> CompareReport {
        CompareReport {
            source_driver: self.target_driver,
            source_namespace: self.target_namespace.clone(),
            target_driver: self.source_driver,
            target_namespace: self.source_namespace.clone(),
            tables: self.tables.iter().map(TableDiff::reversed).collect(),
            rename_candidates: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn build_impact(&self, options: &DeployOptions) -> Vec<String> {
        let mut impact = self.warnings.clone();
        for table in &self.tables {
            match table.status {
                DiffStatus::Added => impact.push(format!(
                    "新增表 {}：{} 列",
                    table.name(),
                    table.columns.iter().filter(|c| c.after.is_some()).count()
                )),
                DiffStatus::Removed => {
                    if options.drop_extra_tables {
                        impact.push(format!(
                            "删除目标表 {}：表数据与依赖它的对象会一并失效，请先备份",
                            table.name()
                        ));
                    } else {
                        impact.push(format!(
                            "目标表 {} 在源中不存在：默认保留（勾选「删除目标多余表」才会生成 DROP）",
                            table.name()
                        ));
                    }
                }
                DiffStatus::Changed => {
                    if table.is_renamed() {
                        impact.push(format!(
                            "重命名 {} → {}：依赖旧表名的视图 / 例程需要同步修改",
                            table.target().unwrap_or("?"),
                            table.source().unwrap_or("?")
                        ));
                    }
                    for column in &table.columns {
                        if column.status == DiffStatus::Removed && !options.drop_extra_columns {
                            impact.push(format!(
                                "目标表 {} 的列 {} 在源中不存在：默认保留（勾选「删除目标多余列」才会生成 DROP）",
                                table.name(),
                                column.name
                            ));
                        }
                    }
                    let destructive = table
                        .columns
                        .iter()
                        .filter(|c| {
                            c.status == DiffStatus::Changed
                                && c.before.is_some()
                                && c.after.is_some()
                        })
                        .count();
                    if destructive > 0 {
                        impact.push(format!(
                            "{} 有 {} 列需要改类型 / 可空：可能截断数据，执行前请备份",
                            table.name(),
                            destructive
                        ));
                    }
                }
                DiffStatus::Same => {}
            }
        }
        impact
    }

    fn build_plan(&self, options: &DeployOptions) -> MigrationPlan {
        let kind = self.target_driver;
        let ns = self.target_namespace.clone();
        let cross = self.source_driver != self.target_driver;

        let mut renames: Vec<MigrationStatement> = Vec::new();
        let mut drop_foreign_keys: Vec<MigrationStatement> = Vec::new();
        let mut drop_indexes: Vec<MigrationStatement> = Vec::new();
        let mut alters: Vec<MigrationStatement> = Vec::new();
        let mut creates: Vec<MigrationStatement> = Vec::new();
        let mut add_indexes: Vec<MigrationStatement> = Vec::new();
        let mut add_foreign_keys: Vec<MigrationStatement> = Vec::new();
        let mut drop_tables: Vec<MigrationStatement> = Vec::new();
        let mut unsupported: Vec<String> = Vec::new();

        for table in &self.tables {
            match table.status {
                DiffStatus::Same => {}
                DiffStatus::Added => {
                    let Some(name) = table.source() else {
                        continue;
                    };
                    let path = table_path(kind, &ns, name);
                    match create_table_statement(kind, &path, table, cross) {
                        Ok(statement) => creates.push(statement),
                        Err(error) => unsupported.push(error),
                    }
                    for index in &table.indexes {
                        let Some(info) = &index.after else { continue };
                        if info.primary {
                            continue;
                        }
                        let plan = plan_add_index(kind, &path, &index_draft(info));
                        add_indexes.extend(plan.statements);
                        unsupported.extend(plan.unsupported);
                    }
                    for fk in &table.foreign_keys {
                        let Some(info) = &fk.after else { continue };
                        let plan = plan_add_foreign_key(kind, &path, &foreign_key_draft(info));
                        add_foreign_keys.extend(plan.statements);
                        unsupported.extend(plan.unsupported);
                    }
                }
                DiffStatus::Removed => {
                    let Some(name) = table.target() else {
                        continue;
                    };
                    if options.drop_extra_tables {
                        let path = table_path(kind, &ns, name);
                        let from = object_from_clause(kind, &path);
                        drop_tables.push(MigrationStatement {
                            sql: format!("DROP TABLE {from};"),
                            destructive: true,
                            note: Some("表数据不可恢复；先备份或改用「保留目标多余表」".into()),
                        });
                    }
                }
                DiffStatus::Changed => {
                    let Some(source_name) = table.source() else {
                        continue;
                    };
                    let Some(target_name) = table.target() else {
                        continue;
                    };
                    if !source_name.eq_ignore_ascii_case(target_name) {
                        renames.push(rename_statement(kind, &ns, target_name, source_name));
                    }

                    // 依赖先拆：外键 → 索引 → 列。
                    for fk in &table.foreign_keys {
                        if !matches!(fk.status, DiffStatus::Removed | DiffStatus::Changed) {
                            continue;
                        }
                        let Some(info) = &fk.before else { continue };
                        let path = table_path(kind, &ns, target_name);
                        let plan = plan_drop_foreign_key(kind, &path, &info.name);
                        drop_foreign_keys.extend(plan.statements);
                        unsupported.extend(plan.unsupported);
                    }
                    for index in &table.indexes {
                        if !matches!(index.status, DiffStatus::Removed | DiffStatus::Changed) {
                            continue;
                        }
                        let Some(info) = &index.before else { continue };
                        if info.primary {
                            continue;
                        }
                        let path = table_path(kind, &ns, target_name);
                        let plan = plan_drop_index(kind, &path, &info.name);
                        drop_indexes.extend(plan.statements);
                        unsupported.extend(plan.unsupported);
                    }

                    let mut before: Vec<ColumnInfo> = Vec::new();
                    for (ordinal, column) in table.columns.iter().enumerate() {
                        let keep = match column.status {
                            DiffStatus::Same | DiffStatus::Changed => true,
                            DiffStatus::Removed => options.drop_extra_columns,
                            DiffStatus::Added => false,
                        };
                        if !keep {
                            continue;
                        }
                        if let Some(shape) = &column.before {
                            before.push(shape_to_info(shape, ordinal as u32));
                        }
                    }
                    let mut after: Vec<ColumnDraft> = Vec::new();
                    for column in &table.columns {
                        if !matches!(
                            column.status,
                            DiffStatus::Same | DiffStatus::Changed | DiffStatus::Added
                        ) {
                            continue;
                        }
                        let Some(shape) = &column.after else { continue };
                        match shape_to_draft(kind, shape, cross) {
                            Ok(draft) => after.push(draft),
                            Err(error) => unsupported.push(error),
                        }
                    }
                    let path = table_path(kind, &ns, source_name);
                    let plan = plan_changes(kind, &path, &before, &after);
                    alters.extend(plan.statements);
                    unsupported.extend(plan.unsupported);

                    for index in &table.indexes {
                        if !matches!(index.status, DiffStatus::Added | DiffStatus::Changed) {
                            continue;
                        }
                        let Some(info) = &index.after else { continue };
                        if info.primary {
                            continue;
                        }
                        let plan = plan_add_index(kind, &path, &index_draft(info));
                        add_indexes.extend(plan.statements);
                        unsupported.extend(plan.unsupported);
                    }
                    for fk in &table.foreign_keys {
                        if !matches!(fk.status, DiffStatus::Added | DiffStatus::Changed) {
                            continue;
                        }
                        let Some(info) = &fk.after else { continue };
                        let plan = plan_add_foreign_key(kind, &path, &foreign_key_draft(info));
                        add_foreign_keys.extend(plan.statements);
                        unsupported.extend(plan.unsupported);
                    }
                }
            }
        }

        let mut statements = Vec::new();
        statements.extend(renames);
        statements.extend(drop_foreign_keys);
        statements.extend(drop_indexes);
        statements.extend(alters);
        statements.extend(creates);
        statements.extend(add_indexes);
        statements.extend(add_foreign_keys);
        statements.extend(drop_tables);

        MigrationPlan {
            statements,
            unsupported,
            transactional: matches!(kind, DbKind::Postgres | DbKind::Sqlite | DbKind::Mssql),
        }
    }
}

/// 部署选项。默认全部为 `false`：只新增 / 修改，不删除目标里的东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployOptions {
    pub drop_extra_tables: bool,
    pub drop_extra_columns: bool,
}

impl Default for DeployOptions {
    fn default() -> Self {
        Self {
            drop_extra_tables: false,
            drop_extra_columns: false,
        }
    }
}

impl DeployOptions {
    /// 回滚建议用：目标里多出来的对象也要恢复 / 删除。
    pub fn full() -> Self {
        Self {
            drop_extra_tables: true,
            drop_extra_columns: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaDeploy {
    pub plan: MigrationPlan,
    pub rollback: MigrationPlan,
    /// 影响与风险提示（面向用户，不是 SQL）。
    #[serde(default)]
    pub impact: Vec<String>,
}

impl SchemaDeploy {
    /// 可直接执行的脚本；事务性方言自动包 `BEGIN` / `COMMIT`。
    pub fn script(&self) -> String {
        let statements = self.plan.executable_sql();
        if statements.is_empty() {
            return String::new();
        }
        if self.plan.transactional {
            format!("BEGIN;\n{}\nCOMMIT;", statements.join("\n"))
        } else {
            statements.join("\n")
        }
    }

    pub fn rollback_script(&self) -> String {
        let statements = self.rollback.executable_sql();
        if statements.is_empty() {
            return String::new();
        }
        if self.rollback.transactional {
            format!("BEGIN;\n{}\nCOMMIT;", statements.join("\n"))
        } else {
            statements.join("\n")
        }
    }
}

// --- 配置 ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenamePair {
    /// 目标侧现有表名。
    pub from: String,
    /// 源侧期望表名。
    pub to: String,
}

/// 对比配置：命名空间、忽略表、用户确认的重命名。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompareConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub source: Option<Namespace>,
    #[serde(default)]
    pub target: Option<Namespace>,
    #[serde(default)]
    pub ignored_tables: Vec<String>,
    #[serde(default)]
    pub renames: Vec<RenamePair>,
}

fn default_version() -> u32 {
    COMPARE_CONFIG_VERSION
}

impl Default for CompareConfig {
    fn default() -> Self {
        Self {
            version: COMPARE_CONFIG_VERSION,
            source: None,
            target: None,
            ignored_tables: Vec::new(),
            renames: Vec::new(),
        }
    }
}

impl CompareConfig {
    pub fn is_ignored(&self, table: &str) -> bool {
        self.ignored_tables
            .iter()
            .any(|name| name.trim().eq_ignore_ascii_case(table))
    }

    /// 解析逗号 / 换行分隔的忽略表清单。
    pub fn set_ignored_text(&mut self, text: &str) {
        self.ignored_tables = text
            .split([',', '，', '\n', '\r'])
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect();
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| format!("序列化对比配置失败：{error}"))
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|error| format!("解析对比配置失败：{error}"))
    }
}

pub fn compare_config_path(dir: &Path) -> PathBuf {
    dir.join("compare.json")
}

pub fn load_compare_config(path: &Path) -> CompareConfig {
    let Ok(text) = std::fs::read_to_string(path) else {
        return CompareConfig::default();
    };
    match CompareConfig::from_json(&text) {
        Ok(config) => config,
        Err(error) => {
            log::warn!("对比配置解析失败：{error}（{}）", path.display());
            CompareConfig::default()
        }
    }
}

pub fn save_compare_config(path: &Path, config: &CompareConfig) -> std::io::Result<()> {
    let text = config
        .to_json()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    crate::docs::write_atomic(path, &text)
}

pub fn save_snapshot(path: &Path, snapshot: &SchemaSnapshot) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(snapshot)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    crate::docs::write_atomic(path, &text)
}

pub fn load_snapshot(path: &Path) -> Result<SchemaSnapshot, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("读取快照失败：{error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("解析快照失败：{error}"))
}

// --- 对比 ---

/// 以 `source` 为参照，比较 `target`。
pub fn compare(
    source: &SchemaSnapshot,
    target: &SchemaSnapshot,
    config: &CompareConfig,
) -> CompareReport {
    let cross = source.driver != target.driver;
    let mut warnings: Vec<String> = Vec::new();
    if cross {
        warnings.push(format!(
            "跨方言对比（{} → {}）：类型按逻辑类型映射，部署前请逐列确认。",
            source.driver.display_name(),
            target.driver.display_name()
        ));
    }
    if source.skipped > 0 {
        warnings.push(format!("源快照有 {} 张表超过上限未加载。", source.skipped));
    }
    if target.skipped > 0 {
        warnings.push(format!(
            "目标快照有 {} 张表超过上限未加载。",
            target.skipped
        ));
    }
    push_failures(&mut warnings, "源快照", &source.failed);
    push_failures(&mut warnings, "目标快照", &target.failed);

    let mut source_tables: Vec<&TableSnapshot> = source
        .tables
        .iter()
        .filter(|table| !config.is_ignored(&table.name))
        .collect();
    let mut target_tables: Vec<&TableSnapshot> = target
        .tables
        .iter()
        .filter(|table| !config.is_ignored(&table.name))
        .collect();
    source_tables.sort_by_key(|table| table.name.to_lowercase());
    target_tables.sort_by_key(|table| table.name.to_lowercase());

    // 用户确认过的重命名：目标旧名 → 源新名。
    let mut source_to_target_name: HashMap<String, String> = HashMap::new();
    for pair in &config.renames {
        let from = pair.from.trim();
        let to = pair.to.trim();
        if from.is_empty() || to.is_empty() || from.eq_ignore_ascii_case(to) {
            continue;
        }
        let target_has = target_tables
            .iter()
            .any(|table| table.name.eq_ignore_ascii_case(from));
        let source_has = source_tables
            .iter()
            .any(|table| table.name.eq_ignore_ascii_case(to));
        if target_has && source_has {
            source_to_target_name.insert(to.to_lowercase(), from.to_string());
        } else {
            warnings.push(format!(
                "重命名配置 {from} → {to} 未命中：{}",
                if !target_has && !source_has {
                    "两侧都没有该表"
                } else if !target_has {
                    "目标侧没有旧表名"
                } else {
                    "源侧没有新表名"
                }
            ));
        }
    }

    let mut matched_targets: Vec<String> = Vec::new();
    let mut tables: Vec<TableDiff> = Vec::new();
    for source_table in &source_tables {
        let explicit = source_to_target_name
            .get(&source_table.name.to_lowercase())
            .cloned();
        let target_table = explicit
            .as_deref()
            .and_then(|name| {
                target_tables
                    .iter()
                    .find(|table| table.name.eq_ignore_ascii_case(name))
            })
            .or_else(|| {
                target_tables
                    .iter()
                    .find(|table| table.name.eq_ignore_ascii_case(&source_table.name))
            });
        match target_table {
            Some(target_table) => {
                matched_targets.push(target_table.name.to_lowercase());
                tables.push(diff_table(source_table, target_table, cross));
            }
            None => tables.push(added_table(source_table)),
        }
    }
    for target_table in &target_tables {
        if matched_targets.contains(&target_table.name.to_lowercase()) {
            continue;
        }
        tables.push(removed_table(target_table));
    }

    let rename_candidates = detect_rename_candidates(&tables);
    if !rename_candidates.is_empty() {
        warnings.push(format!(
            "发现 {} 组疑似重命名，确认后写入配置才会生成 RENAME。",
            rename_candidates.len()
        ));
    }

    CompareReport {
        source_driver: source.driver,
        source_namespace: source.namespace.clone(),
        target_driver: target.driver,
        target_namespace: target.namespace.clone(),
        tables,
        rename_candidates,
        warnings,
    }
}

fn push_failures(warnings: &mut Vec<String>, side: &str, failed: &[String]) {
    for message in failed.iter().take(10) {
        warnings.push(format!("{side}：{message}"));
    }
    if failed.len() > 10 {
        warnings.push(format!("{side}：另有 {} 条失败未展开。", failed.len() - 10));
    }
}

fn added_table(source: &TableSnapshot) -> TableDiff {
    TableDiff {
        source_name: Some(source.name.clone()),
        target_name: None,
        status: DiffStatus::Added,
        columns: source
            .columns
            .iter()
            .map(|column| ColumnDiff {
                name: column.meta.name.to_string(),
                status: DiffStatus::Added,
                before: None,
                after: Some(ColumnShape::from_info(column)),
                changes: vec!["新增列".into()],
                notes: Vec::new(),
            })
            .collect(),
        indexes: source
            .indexes
            .iter()
            .filter(|index| !index.primary)
            .map(|index| IndexDiff {
                name: index.name.clone(),
                status: DiffStatus::Added,
                before: None,
                after: Some(index.clone()),
                note: None,
            })
            .collect(),
        foreign_keys: source
            .foreign_keys
            .iter()
            .map(|fk| ForeignKeyDiff {
                name: fk.name.clone(),
                status: DiffStatus::Added,
                before: None,
                after: Some(fk.clone()),
                note: None,
            })
            .collect(),
        notes: auto_increment_note(source),
    }
}

fn removed_table(target: &TableSnapshot) -> TableDiff {
    TableDiff {
        source_name: None,
        target_name: Some(target.name.clone()),
        status: DiffStatus::Removed,
        columns: target
            .columns
            .iter()
            .map(|column| ColumnDiff {
                name: column.meta.name.to_string(),
                status: DiffStatus::Removed,
                before: Some(ColumnShape::from_info(column)),
                after: None,
                changes: vec!["删除列".into()],
                notes: Vec::new(),
            })
            .collect(),
        indexes: target
            .indexes
            .iter()
            .filter(|index| !index.primary)
            .map(|index| IndexDiff {
                name: index.name.clone(),
                status: DiffStatus::Removed,
                before: Some(index.clone()),
                after: None,
                note: None,
            })
            .collect(),
        foreign_keys: target
            .foreign_keys
            .iter()
            .map(|fk| ForeignKeyDiff {
                name: fk.name.clone(),
                status: DiffStatus::Removed,
                before: Some(fk.clone()),
                after: None,
                note: None,
            })
            .collect(),
        notes: Vec::new(),
    }
}

fn diff_table(source: &TableSnapshot, target: &TableSnapshot, cross: bool) -> TableDiff {
    let mut notes = auto_increment_note(source);
    if !source.name.eq_ignore_ascii_case(&target.name) {
        notes.push(format!("重命名：{} → {}", target.name, source.name));
    }
    let columns = diff_columns(&source.columns, &target.columns, cross);
    let indexes = diff_indexes(&source.indexes, &target.indexes);
    let foreign_keys = diff_foreign_keys(&source.foreign_keys, &target.foreign_keys);
    let changed = !source.name.eq_ignore_ascii_case(&target.name)
        || columns
            .iter()
            .any(|column| column.status != DiffStatus::Same)
        || indexes.iter().any(|index| index.status != DiffStatus::Same)
        || foreign_keys.iter().any(|fk| fk.status != DiffStatus::Same);
    TableDiff {
        source_name: Some(source.name.clone()),
        target_name: Some(target.name.clone()),
        status: if changed {
            DiffStatus::Changed
        } else {
            DiffStatus::Same
        },
        columns,
        indexes,
        foreign_keys,
        notes,
    }
}

// 真正的类型映射只发生在 deploy（那里有明确的目标方言）。
fn diff_columns(source: &[ColumnInfo], target: &[ColumnInfo], cross: bool) -> Vec<ColumnDiff> {
    let mut used: Vec<bool> = vec![false; target.len()];
    let mut diffs: Vec<ColumnDiff> = Vec::new();
    for column in source {
        let shape = ColumnShape::from_info(column);
        let found = target.iter().enumerate().find(|(index, item)| {
            !used[*index] && item.meta.name.eq_ignore_ascii_case(&shape.name)
        });
        match found {
            Some((index, before_info)) => {
                used[index] = true;
                let before = ColumnShape::from_info(before_info);
                let changes = describe_column_changes(&before, &shape, cross);
                let status = if changes.is_empty() {
                    DiffStatus::Same
                } else {
                    DiffStatus::Changed
                };
                diffs.push(ColumnDiff {
                    name: shape.name.clone(),
                    status,
                    before: Some(before),
                    after: Some(shape),
                    changes,
                    notes: Vec::new(),
                });
            }
            None => diffs.push(ColumnDiff {
                name: shape.name.clone(),
                status: DiffStatus::Added,
                before: None,
                after: Some(shape),
                changes: vec!["新增列".into()],
                notes: Vec::new(),
            }),
        }
    }
    for (index, column) in target.iter().enumerate() {
        if used[index] {
            continue;
        }
        let shape = ColumnShape::from_info(column);
        diffs.push(ColumnDiff {
            name: shape.name.clone(),
            status: DiffStatus::Removed,
            before: Some(shape),
            after: None,
            changes: vec!["删除列".into()],
            notes: Vec::new(),
        });
    }
    diffs
}

fn describe_column_changes(before: &ColumnShape, after: &ColumnShape, cross: bool) -> Vec<String> {
    let mut changes = Vec::new();
    if !before.name.eq_ignore_ascii_case(&after.name) {
        changes.push(format!("列名 {} → {}", before.name, after.name));
    }
    let type_changed = if cross {
        before.logical_type != after.logical_type
    } else {
        !before.native_type.eq_ignore_ascii_case(&after.native_type)
    };
    if type_changed {
        changes.push(format!(
            "类型 {} → {}",
            before.native_type, after.native_type
        ));
    }
    if before.nullable != after.nullable {
        changes.push(if after.nullable {
            "非空 → 可空".to_string()
        } else {
            "可空 → 非空".to_string()
        });
    }
    if normalize_default(before.default.as_deref()) != normalize_default(after.default.as_deref()) {
        changes.push(format!(
            "默认值 {} → {}",
            before.default.as_deref().unwrap_or("-"),
            after.default.as_deref().unwrap_or("-")
        ));
    }
    if before.primary_key != after.primary_key {
        changes.push(if after.primary_key {
            "非主键 → 主键".to_string()
        } else {
            "主键 → 非主键".to_string()
        });
    }
    if before.auto_increment != after.auto_increment {
        changes.push(if after.auto_increment {
            "非自增 → 自增（脚本不包含自增属性，需手工处理）".to_string()
        } else {
            "自增 → 非自增（脚本不包含自增属性，需手工处理）".to_string()
        });
    }
    changes
}

fn normalize_default(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.to_string())
}

fn auto_increment_note(table: &TableSnapshot) -> Vec<String> {
    if table.columns.iter().any(|column| column.is_auto_increment) {
        vec!["自增属性不包含在部署脚本中，请在建表后手工补充。".into()]
    } else {
        Vec::new()
    }
}

fn diff_indexes(source: &[IndexInfo], target: &[IndexInfo]) -> Vec<IndexDiff> {
    let source: Vec<&IndexInfo> = source.iter().filter(|index| !index.primary).collect();
    let target: Vec<&IndexInfo> = target.iter().filter(|index| !index.primary).collect();
    let mut used: Vec<bool> = vec![false; target.len()];
    let mut diffs = Vec::new();
    for index in &source {
        let named = target.iter().enumerate().find(|(position, item)| {
            !used[*position] && item.name.eq_ignore_ascii_case(&index.name)
        });
        let found = named.or_else(|| {
            target
                .iter()
                .enumerate()
                .find(|(position, item)| !used[*position] && index_equal(item, index))
        });
        match found {
            Some((position, before)) => {
                used[position] = true;
                let same_shape = index_equal(before, index);
                diffs.push(IndexDiff {
                    name: index.name.clone(),
                    status: if same_shape {
                        DiffStatus::Same
                    } else {
                        DiffStatus::Changed
                    },
                    before: Some((*before).clone()),
                    after: Some((*index).clone()),
                    note: (!before.name.eq_ignore_ascii_case(&index.name))
                        .then(|| format!("名称不同但结构一致：{} → {}", before.name, index.name)),
                });
            }
            None => diffs.push(IndexDiff {
                name: index.name.clone(),
                status: DiffStatus::Added,
                before: None,
                after: Some((*index).clone()),
                note: None,
            }),
        }
    }
    for (position, index) in target.iter().enumerate() {
        if used[position] {
            continue;
        }
        diffs.push(IndexDiff {
            name: index.name.clone(),
            status: DiffStatus::Removed,
            before: Some((*index).clone()),
            after: None,
            note: None,
        });
    }
    diffs
}

fn index_equal(a: &IndexInfo, b: &IndexInfo) -> bool {
    a.unique == b.unique
        && a.method
            .as_deref()
            .unwrap_or("")
            .eq_ignore_ascii_case(b.method.as_deref().unwrap_or(""))
        && string_list_equal(&a.columns, &b.columns)
}

fn diff_foreign_keys(source: &[ForeignKeyInfo], target: &[ForeignKeyInfo]) -> Vec<ForeignKeyDiff> {
    let mut used: Vec<bool> = vec![false; target.len()];
    let mut diffs = Vec::new();
    for fk in source {
        let named = target
            .iter()
            .enumerate()
            .find(|(position, item)| !used[*position] && item.name.eq_ignore_ascii_case(&fk.name));
        let found = named.or_else(|| {
            target
                .iter()
                .enumerate()
                .find(|(position, item)| !used[*position] && foreign_key_equal(item, fk))
        });
        match found {
            Some((position, before)) => {
                used[position] = true;
                let same_shape = foreign_key_equal(before, fk);
                diffs.push(ForeignKeyDiff {
                    name: fk.name.clone(),
                    status: if same_shape {
                        DiffStatus::Same
                    } else {
                        DiffStatus::Changed
                    },
                    before: Some((*before).clone()),
                    after: Some(fk.clone()),
                    note: (!before.name.eq_ignore_ascii_case(&fk.name))
                        .then(|| format!("名称不同但结构一致：{} → {}", before.name, fk.name)),
                });
            }
            None => diffs.push(ForeignKeyDiff {
                name: fk.name.clone(),
                status: DiffStatus::Added,
                before: None,
                after: Some(fk.clone()),
                note: None,
            }),
        }
    }
    for (position, fk) in target.iter().enumerate() {
        if used[position] {
            continue;
        }
        diffs.push(ForeignKeyDiff {
            name: fk.name.clone(),
            status: DiffStatus::Removed,
            before: Some(fk.clone()),
            after: None,
            note: None,
        });
    }
    diffs
}

fn foreign_key_equal(a: &ForeignKeyInfo, b: &ForeignKeyInfo) -> bool {
    a.referenced_table.eq_ignore_ascii_case(&b.referenced_table)
        && string_list_equal(&a.columns, &b.columns)
        && string_list_equal(&a.referenced_columns, &b.referenced_columns)
        && option_eq_ignore_case(&a.on_delete, &b.on_delete)
        && option_eq_ignore_case(&a.on_update, &b.on_update)
}

fn string_list_equal(a: &[String], b: &[String]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn option_eq_ignore_case(a: &Option<String>, b: &Option<String>) -> bool {
    match (a.as_deref(), b.as_deref()) {
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        (None, None) => true,
        _ => false,
    }
}

fn detect_rename_candidates(tables: &[TableDiff]) -> Vec<RenameCandidate> {
    let added: Vec<&TableDiff> = tables
        .iter()
        .filter(|table| table.status == DiffStatus::Added)
        .collect();
    let removed: Vec<&TableDiff> = tables
        .iter()
        .filter(|table| table.status == DiffStatus::Removed)
        .collect();
    if added.is_empty() || removed.is_empty() {
        return Vec::new();
    }
    let mut pairs: Vec<RenameCandidate> = Vec::new();
    for source in &added {
        let Some(to) = source.source() else { continue };
        let source_signature = shape_signature(source, true);
        if source_signature.is_empty() {
            continue;
        }
        for target in &removed {
            let Some(from) = target.target() else {
                continue;
            };
            let target_signature = shape_signature(target, false);
            if target_signature.is_empty() {
                continue;
            }
            let matched = multiset_matches(&source_signature, &target_signature);
            let total = source_signature.len().max(target_signature.len());
            let ratio = matched as f32 / total as f32;
            if matched == 0 || (matched < total && ratio < 0.7) {
                continue;
            }
            let reason = if matched == source_signature.len() && matched == target_signature.len() {
                format!("{matched} 列完全一致")
            } else {
                format!("{matched}/{total} 列一致")
            };
            pairs.push(RenameCandidate {
                from: from.to_string(),
                to: to.to_string(),
                matched_columns: matched,
                total_columns: total,
                reason,
            });
        }
    }
    pairs.sort_by(|a, b| {
        b.matched_columns
            .cmp(&a.matched_columns)
            .then_with(|| a.to.cmp(&b.to))
            .then_with(|| a.from.cmp(&b.from))
    });
    let mut used_from: Vec<String> = Vec::new();
    let mut used_to: Vec<String> = Vec::new();
    let mut picked = Vec::new();
    for pair in pairs {
        if picked.len() >= MAX_RENAME_CANDIDATES {
            break;
        }
        if used_from
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&pair.from))
            || used_to
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&pair.to))
        {
            continue;
        }
        used_from.push(pair.from.clone());
        used_to.push(pair.to.clone());
        picked.push(pair);
    }
    picked
}

fn shape_signature(table: &TableDiff, source: bool) -> Vec<String> {
    table
        .columns
        .iter()
        .filter_map(|column| {
            let shape = if source {
                column.after.as_ref()
            } else {
                column.before.as_ref()
            }?;
            Some(format!(
                "{}:{}",
                shape.name.to_lowercase(),
                shape.logical_type.label()
            ))
        })
        .collect()
}

fn multiset_matches(left: &[String], right: &[String]) -> usize {
    let mut remaining = right.to_vec();
    let mut matched = 0;
    for item in left {
        if let Some(position) = remaining.iter().position(|candidate| candidate == item) {
            remaining.remove(position);
            matched += 1;
        }
    }
    matched
}

// --- SQL 生成辅助 ---

fn table_path(_kind: DbKind, ns: &Namespace, name: &str) -> ObjectPath {
    ObjectPath::new(ns.clone(), name, ObjectKind::Table)
}

fn shape_to_info(shape: &ColumnShape, ordinal: u32) -> ColumnInfo {
    let mut info = ColumnInfo::simple(shape.name.clone(), shape.logical_type.clone(), ordinal);
    info.meta.native_type = shape.native_type.clone();
    info.meta.nullable = shape.nullable;
    info.default = shape.default.clone();
    info.is_primary_key = shape.primary_key;
    info.is_auto_increment = shape.auto_increment;
    info
}

fn shape_to_draft(kind: DbKind, shape: &ColumnShape, cross: bool) -> Result<ColumnDraft, String> {
    let type_text = if cross {
        native_type_for(kind, &shape.logical_type).ok_or_else(|| {
            format!(
                "列 {}：源类型 {}（{}）无法自动映射到 {}，请手工指定",
                shape.name,
                shape.native_type,
                shape.logical_type.label(),
                kind.display_name()
            )
        })?
    } else {
        shape.native_type.clone()
    };
    Ok(ColumnDraft {
        name: shape.name.clone(),
        type_text,
        nullable: shape.nullable,
        default: shape.default.clone(),
    })
}

fn index_draft(info: &IndexInfo) -> IndexDraft {
    IndexDraft {
        name: info.name.clone(),
        columns: info.columns.clone(),
        unique: info.unique,
        method: info.method.clone(),
    }
}

fn foreign_key_draft(info: &ForeignKeyInfo) -> ForeignKeyDraft {
    ForeignKeyDraft {
        name: info.name.clone(),
        columns: info.columns.clone(),
        referenced_table: info.referenced_table.clone(),
        referenced_columns: info.referenced_columns.clone(),
        on_delete: info.on_delete.clone(),
        on_update: info.on_update.clone(),
    }
}

fn create_table_statement(
    kind: DbKind,
    path: &ObjectPath,
    table: &TableDiff,
    cross: bool,
) -> Result<MigrationStatement, String> {
    let mut definitions: Vec<String> = Vec::new();
    let mut primary: Vec<String> = Vec::new();
    for column in &table.columns {
        let Some(shape) = &column.after else { continue };
        let draft = shape_to_draft(kind, shape, cross)?;
        let mut definition = format!("{} {}", ident(kind, &draft.name), draft.type_text);
        if !draft.nullable {
            definition.push_str(" NOT NULL");
        }
        if let Some(default) = &draft.default {
            definition.push_str(&format!(" DEFAULT {default}"));
        }
        definitions.push(definition);
        if shape.primary_key {
            primary.push(ident(kind, &shape.name));
        }
    }
    if !primary.is_empty() {
        definitions.push(format!("PRIMARY KEY ({})", primary.join(", ")));
    }
    let name = object_from_clause(kind, path);
    Ok(MigrationStatement {
        sql: format!(
            "CREATE TABLE {name} (\n    {}\n);",
            definitions.join(",\n    ")
        ),
        destructive: false,
        note: table.notes.first().cloned(),
    })
}

fn rename_statement(kind: DbKind, ns: &Namespace, from: &str, to: &str) -> MigrationStatement {
    let from_path = table_path(kind, ns, from);
    let sql = match kind {
        DbKind::Mssql => {
            let old = match ns.schema.as_deref() {
                Some(schema) => format!("{schema}.{from}"),
                None => from.to_string(),
            };
            format!(
                "EXEC sp_rename {}, {};",
                rustdb_db_api::dialect::quote_string_literal(&old),
                rustdb_db_api::dialect::quote_string_literal(to)
            )
        }
        _ => format!(
            "ALTER TABLE {} RENAME TO {};",
            object_from_clause(kind, &from_path),
            ident(kind, to)
        ),
    };
    MigrationStatement {
        sql,
        destructive: false,
        note: Some("重命名会改变依赖该表的视图 / 例程，执行前确认影响".into()),
    }
}

/// 跨方言的类型映射。映射不了返回 `None`，由调用方显式报不支持。
pub fn native_type_for(kind: DbKind, logical: &LogicalType) -> Option<String> {
    let text = match logical {
        LogicalType::Bool => match kind {
            DbKind::Postgres => "boolean".to_string(),
            DbKind::Mysql | DbKind::Mariadb => "tinyint(1)".to_string(),
            DbKind::Mssql => "bit".to_string(),
            DbKind::Oracle => "number(1)".to_string(),
            DbKind::Sqlite => "integer".to_string(),
        },
        LogicalType::Int { bits, unsigned } => match kind {
            DbKind::Postgres => sized_int(*bits, "smallint", "integer", "bigint"),
            DbKind::Mysql | DbKind::Mariadb => {
                let base = if *bits <= 8 {
                    "tinyint"
                } else if *bits <= 16 {
                    "smallint"
                } else if *bits <= 32 {
                    "int"
                } else {
                    "bigint"
                };
                if *unsigned {
                    format!("{base} unsigned")
                } else {
                    base.to_string()
                }
            }
            DbKind::Mssql => match bits {
                0..=8 => "tinyint".to_string(),
                9..=16 => "smallint".to_string(),
                17..=32 => "int".to_string(),
                _ => "bigint".to_string(),
            },
            DbKind::Oracle => {
                let precision = if *bits <= 8 {
                    3
                } else if *bits <= 16 {
                    5
                } else if *bits <= 32 {
                    10
                } else {
                    19
                };
                format!("number({precision})")
            }
            DbKind::Sqlite => "integer".to_string(),
        },
        LogicalType::Float { bits } => {
            if *bits <= 32 {
                match kind {
                    DbKind::Postgres => "real",
                    DbKind::Mysql | DbKind::Mariadb => "float",
                    DbKind::Mssql => "real",
                    DbKind::Oracle => "binary_float",
                    DbKind::Sqlite => "real",
                }
                .to_string()
            } else {
                match kind {
                    DbKind::Postgres => "double precision",
                    DbKind::Mysql | DbKind::Mariadb => "double",
                    DbKind::Mssql => "float",
                    DbKind::Oracle => "binary_double",
                    DbKind::Sqlite => "real",
                }
                .to_string()
            }
        }
        LogicalType::Decimal { precision, scale } => match kind {
            DbKind::Mysql | DbKind::Mariadb => match (precision, scale) {
                (Some(p), Some(s)) => format!("decimal({p},{s})"),
                (Some(p), None) => format!("decimal({p})"),
                _ => "decimal".to_string(),
            },
            DbKind::Oracle => match (precision, scale) {
                (Some(p), Some(s)) => format!("number({p},{s})"),
                (Some(p), None) => format!("number({p})"),
                _ => "number".to_string(),
            },
            DbKind::Sqlite => "numeric".to_string(),
            _ => match (precision, scale) {
                (Some(p), Some(s)) => format!("numeric({p},{s})"),
                (Some(p), None) => format!("numeric({p})"),
                _ => "numeric".to_string(),
            },
        },
        LogicalType::Text { max_len } => match (kind, max_len) {
            (DbKind::Mysql | DbKind::Mariadb, Some(len)) => format!("varchar({len})"),
            (DbKind::Mysql | DbKind::Mariadb, None) => "text".to_string(),
            (DbKind::Mssql, Some(len)) => format!("nvarchar({len})"),
            (DbKind::Mssql, None) => "nvarchar(max)".to_string(),
            (DbKind::Oracle, Some(len)) => format!("varchar2({len})"),
            (DbKind::Oracle, None) => "clob".to_string(),
            (DbKind::Sqlite, _) => "text".to_string(),
            (_, Some(len)) => format!("varchar({len})"),
            (_, None) => "text".to_string(),
        },
        LogicalType::Bytes => match kind {
            DbKind::Postgres => "bytea".to_string(),
            DbKind::Mysql | DbKind::Mariadb => "blob".to_string(),
            DbKind::Mssql => "varbinary(max)".to_string(),
            DbKind::Oracle => "blob".to_string(),
            DbKind::Sqlite => "blob".to_string(),
        },
        LogicalType::Date => "date".to_string(),
        LogicalType::Time => {
            if kind == DbKind::Oracle {
                "timestamp".to_string()
            } else {
                "time".to_string()
            }
        }
        LogicalType::Timestamp => "timestamp".to_string(),
        LogicalType::TimestampTz => match kind {
            DbKind::Postgres => "timestamptz".to_string(),
            DbKind::Mysql | DbKind::Mariadb => "timestamp".to_string(),
            DbKind::Mssql => "datetimeoffset".to_string(),
            DbKind::Oracle => "timestamp with time zone".to_string(),
            DbKind::Sqlite => "text".to_string(),
        },
        LogicalType::Uuid => match kind {
            DbKind::Postgres => "uuid".to_string(),
            DbKind::Mysql | DbKind::Mariadb => "char(36)".to_string(),
            DbKind::Mssql => "uniqueidentifier".to_string(),
            DbKind::Oracle => "raw(16)".to_string(),
            DbKind::Sqlite => "text".to_string(),
        },
        LogicalType::Json => match kind {
            DbKind::Postgres => "jsonb".to_string(),
            DbKind::Mysql | DbKind::Mariadb => "json".to_string(),
            DbKind::Mssql => "nvarchar(max)".to_string(),
            DbKind::Oracle => "clob".to_string(),
            DbKind::Sqlite => "text".to_string(),
        },
        LogicalType::Array(inner) => {
            if kind == DbKind::Postgres {
                format!("{}[]", native_type_for(kind, inner)?)
            } else {
                return None;
            }
        }
        LogicalType::Unknown(_) => return None,
    };
    Some(text)
}

fn sized_int(bits: u8, small: &str, medium: &str, large: &str) -> String {
    if bits <= 16 {
        small.to_string()
    } else if bits <= 32 {
        medium.to_string()
    } else {
        large.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::{ColumnInfo, LogicalType};

    fn ns(name: &str) -> Namespace {
        Namespace::new(Some(name), Some("public"))
    }

    fn column(name: &str, logical: LogicalType, native: &str, nullable: bool) -> ColumnInfo {
        let mut info = ColumnInfo::simple(name, logical, 1);
        info.meta.native_type = native.to_string();
        info.meta.nullable = nullable;
        info
    }

    fn table(name: &str) -> TableSnapshot {
        TableSnapshot {
            name: name.into(),
            columns: Vec::new(),
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
        }
    }

    fn snapshot(driver: DbKind, name: &str, tables: Vec<TableSnapshot>) -> SchemaSnapshot {
        SchemaSnapshot {
            driver,
            namespace: ns(name),
            tables,
            skipped: 0,
            failed: Vec::new(),
        }
    }

    fn base_target() -> SchemaSnapshot {
        let mut users = table("users");
        users.columns = vec![
            column(
                "id",
                LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
                "bigint",
                false,
            ),
            column(
                "name",
                LogicalType::Text { max_len: Some(50) },
                "varchar(50)",
                true,
            ),
            column("legacy", LogicalType::Text { max_len: None }, "text", true),
        ];
        let mut users_index = IndexInfo {
            name: "idx_name".into(),
            columns: vec!["name".into()],
            unique: false,
            primary: false,
            method: None,
        };
        users_index.unique = true;
        users.indexes = vec![users_index];
        users.foreign_keys = vec![ForeignKeyInfo {
            name: "fk_team".into(),
            columns: vec!["id".into()],
            referenced_table: "teams".into(),
            referenced_columns: vec!["id".into()],
            on_delete: Some("CASCADE".into()),
            on_update: None,
        }];

        let mut teams = table("teams");
        teams.columns = vec![column(
            "id",
            LogicalType::Int {
                bits: 64,
                unsigned: false,
            },
            "bigint",
            false,
        )];

        let mut old_name = table("orders_old");
        old_name.columns = vec![
            column(
                "id",
                LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
                "bigint",
                false,
            ),
            column(
                "total",
                LogicalType::Decimal {
                    precision: Some(12),
                    scale: Some(2),
                },
                "numeric(12,2)",
                false,
            ),
        ];

        snapshot(DbKind::Postgres, "app", vec![users, teams, old_name])
    }

    fn base_source() -> SchemaSnapshot {
        let mut users = table("users");
        users.columns = vec![
            column(
                "id",
                LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
                "bigint",
                false,
            ),
            column(
                "name",
                LogicalType::Text { max_len: Some(80) },
                "varchar(80)",
                false,
            ),
            column(
                "email",
                LogicalType::Text { max_len: Some(120) },
                "varchar(120)",
                true,
            ),
        ];
        users.indexes = vec![
            IndexInfo {
                name: "idx_name".into(),
                columns: vec!["name".into()],
                unique: true,
                primary: false,
                method: None,
            },
            IndexInfo {
                name: "idx_users_email".into(),
                columns: vec!["email".into()],
                unique: false,
                primary: false,
                method: None,
            },
        ];
        users.foreign_keys = vec![ForeignKeyInfo {
            name: "fk_team".into(),
            columns: vec!["id".into()],
            referenced_table: "teams".into(),
            referenced_columns: vec!["id".into()],
            on_delete: Some("CASCADE".into()),
            on_update: None,
        }];

        let mut teams = table("teams");
        teams.columns = vec![column(
            "id",
            LogicalType::Int {
                bits: 64,
                unsigned: false,
            },
            "bigint",
            false,
        )];

        let mut orders = table("orders");
        orders.columns = vec![
            column(
                "id",
                LogicalType::Int {
                    bits: 64,
                    unsigned: false,
                },
                "bigint",
                false,
            ),
            column(
                "total",
                LogicalType::Decimal {
                    precision: Some(12),
                    scale: Some(2),
                },
                "numeric(12,2)",
                false,
            ),
        ];
        orders.columns[0].is_primary_key = true;

        snapshot(DbKind::Postgres, "app", vec![users, teams, orders])
    }

    #[test]
    fn compare_reports_added_removed_changed_and_same() {
        let report = compare(&base_source(), &base_target(), &CompareConfig::default());
        let counts = report.counts();
        assert_eq!(counts.added, 1, "orders");
        assert_eq!(counts.removed, 1, "orders_old");
        assert_eq!(counts.changed, 1, "users");
        assert_eq!(counts.same, 1, "teams");

        let users = report
            .tables
            .iter()
            .find(|table| table.name() == "users")
            .unwrap();
        assert_eq!(users.status, DiffStatus::Changed);
        assert!(users
            .columns
            .iter()
            .any(|column| column.name == "email" && column.status == DiffStatus::Added));
        assert!(users
            .columns
            .iter()
            .any(|column| column.name == "legacy" && column.status == DiffStatus::Removed));
        let name = users
            .columns
            .iter()
            .find(|column| column.name == "name")
            .unwrap();
        assert!(name.changes.iter().any(|change| change.contains("类型")));
        assert!(report.preview_text().contains("~ users"));
    }

    #[test]
    fn rename_candidate_detected_and_confirmed_by_config() {
        let report = compare(&base_source(), &base_target(), &CompareConfig::default());
        let candidate = report
            .rename_candidates
            .iter()
            .find(|candidate| candidate.from == "orders_old" && candidate.to == "orders")
            .expect("orders 与 orders_old 列结构一致，应给出候选");
        assert_eq!(candidate.matched_columns, 2);
        assert!(candidate.reason.contains("完全一致"));

        let config = CompareConfig {
            renames: vec![RenamePair {
                from: "orders_old".into(),
                to: "orders".into(),
            }],
            ..CompareConfig::default()
        };
        let report = compare(&base_source(), &base_target(), &config);
        assert!(
            !report
                .tables
                .iter()
                .any(|table| table.status == DiffStatus::Added && table.name() == "orders"),
            "确认重命名后 orders 不应再算新增"
        );
        let orders = report
            .tables
            .iter()
            .find(|table| table.name() == "orders")
            .unwrap();
        assert!(orders.is_renamed());
        let deploy = report.deploy_plan(&DeployOptions::default());
        assert!(
            deploy
                .plan
                .executable_sql()
                .iter()
                .any(|sql| sql.contains("RENAME TO") && sql.contains("orders")),
            "{:?}",
            deploy.plan.executable_sql()
        );
    }

    #[test]
    fn ignored_tables_are_excluded_from_report() {
        let config = CompareConfig {
            ignored_tables: vec!["users".into(), "ORDERS_OLD".into()],
            ..CompareConfig::default()
        };
        let report = compare(&base_source(), &base_target(), &config);
        assert!(!report.tables.iter().any(|table| table.name() == "users"));
        assert!(!report
            .tables
            .iter()
            .any(|table| table.name() == "orders_old"));
        assert!(report.tables.iter().any(|table| table.name() == "orders"));
    }

    #[test]
    fn deploy_keeps_target_extras_by_default_and_full_mode_drops_them() {
        let report = compare(&base_source(), &base_target(), &CompareConfig::default());
        let safe = report.deploy_plan(&DeployOptions::default());
        let sql = safe.plan.executable_sql().join("\n");
        assert!(sql.contains("CREATE TABLE"), "{sql}");
        assert!(sql.contains("ADD COLUMN \"email\""), "{sql}");
        assert!(!sql.contains("DROP TABLE"), "默认不删目标表：{sql}");
        assert!(
            !sql.contains("DROP COLUMN \"legacy\""),
            "默认不删目标列：{sql}"
        );
        assert!(safe.plan.transactional);
        assert!(safe.script().starts_with("BEGIN;"));
        assert!(safe.script().ends_with("COMMIT;"));
        assert!(
            safe.impact.iter().any(|line| line.contains("默认保留")),
            "{:?}",
            safe.impact
        );

        let full = report.deploy_plan(&DeployOptions::full());
        let sql = full.plan.executable_sql().join("\n");
        assert!(sql.contains("DROP TABLE"), "{sql}");
        assert!(sql.contains("DROP COLUMN \"legacy\""), "{sql}");
        let drop = full
            .plan
            .statements
            .iter()
            .find(|statement| statement.sql.contains("DROP TABLE"))
            .unwrap();
        assert!(drop.destructive);
    }

    #[test]
    fn deploy_orders_renames_drops_columns_creates_indexes_and_foreign_keys() {
        let config = CompareConfig {
            renames: vec![RenamePair {
                from: "orders_old".into(),
                to: "orders".into(),
            }],
            ..CompareConfig::default()
        };
        let report = compare(&base_source(), &base_target(), &config);
        let deploy = report.deploy_plan(&DeployOptions::full());
        let sql = deploy.plan.executable_sql();
        let position = |needle: &str| {
            sql.iter()
                .position(|statement| statement.contains(needle))
                .unwrap_or_else(|| panic!("缺少 {needle}：{sql:?}"))
        };
        assert!(position("RENAME TO") < position("DROP COLUMN \"legacy\""));
        assert!(position("ADD COLUMN \"email\"") < position("CREATE INDEX"));
    }

    #[test]
    fn rollback_reverses_added_and_removed_tables() {
        let report = compare(&base_source(), &base_target(), &CompareConfig::default());
        let deploy = report.deploy_plan(&DeployOptions::full());
        let rollback = deploy.rollback_script();
        assert!(
            rollback.contains("DROP TABLE") && rollback.contains("\"orders\""),
            "{rollback}"
        );
        assert!(
            rollback.contains("CREATE TABLE") && rollback.contains("\"orders_old\""),
            "{rollback}"
        );
        assert!(
            deploy.impact.iter().any(|line| line.contains("回滚")),
            "{:?}",
            deploy.impact
        );
    }

    #[test]
    fn cross_dialect_maps_types_and_reports_mapping_notes() {
        let mut source = base_source();
        source.driver = DbKind::Postgres;
        let mut target = snapshot(DbKind::Mysql, "app", Vec::new());
        let mut users = table("users");
        users.columns = vec![
            column("id", LogicalType::Uuid, "uuid", false),
            column("payload", LogicalType::Json, "jsonb", true),
            column("created", LogicalType::TimestampTz, "timestamptz", false),
        ];
        target.tables.push(users);
        let mut source_users = table("users");
        source_users.columns = vec![
            column("id", LogicalType::Uuid, "uuid", false),
            column("payload", LogicalType::Json, "jsonb", true),
            column("created", LogicalType::TimestampTz, "timestamptz", true),
        ];
        source.tables = vec![source_users];

        let report = compare(&source, &target, &CompareConfig::default());
        assert!(report.warnings.iter().any(|line| line.contains("跨方言")));
        let users = report.tables.first().unwrap();
        assert!(
            users
                .columns
                .iter()
                .any(|column| column.name == "created" && column.status == DiffStatus::Changed),
            "可空差异应被识别：{users:?}"
        );

        let deploy = report.deploy_plan(&DeployOptions::default());
        let sql = deploy.plan.executable_sql().join("\n");
        assert!(sql.contains("char(36)"), "uuid → char(36)：{sql}");
        assert!(sql.contains("json"), "jsonb → json：{sql}");
        assert!(sql.contains("timestamp"), "timestamptz → timestamp：{sql}");
        assert!(!deploy.plan.transactional, "MySQL DDL 会隐式提交");
    }

    #[test]
    fn cross_dialect_unknown_type_is_explicitly_unsupported() {
        let mut source = snapshot(DbKind::Oracle, "app", Vec::new());
        let mut target = snapshot(DbKind::Postgres, "app", Vec::new());
        let mut source_table = table("t");
        source_table.columns = vec![column(
            "weird",
            LogicalType::Unknown("SDO_GEOMETRY".into()),
            "SDO_GEOMETRY",
            true,
        )];
        source.tables.push(source_table);
        let mut target_table = table("t");
        target_table.columns = vec![column(
            "weird",
            LogicalType::Unknown("SDO_GEOMETRY".into()),
            "SDO_GEOMETRY",
            true,
        )];
        target.tables.push(target_table);
        let report = compare(&source, &target, &CompareConfig::default());
        // 逻辑类型一致，不算差异；改成新增列才会走映射。
        assert!(report.is_empty());
        source.tables[0].columns[0].meta.nullable = true;
        target.tables[0].columns.clear();
        let report = compare(&source, &target, &CompareConfig::default());
        let deploy = report.deploy_plan(&DeployOptions::default());
        assert!(
            deploy
                .plan
                .unsupported
                .iter()
                .any(|line| line.contains("无法自动映射")),
            "{:?}",
            deploy.plan.unsupported
        );
    }

    #[test]
    fn index_and_foreign_key_shape_match_avoids_rename_noise() {
        let mut source = snapshot(DbKind::Postgres, "app", Vec::new());
        let mut target = snapshot(DbKind::Postgres, "app", Vec::new());
        let mut source_table = table("t");
        source_table.columns = vec![column(
            "id",
            LogicalType::Int {
                bits: 64,
                unsigned: false,
            },
            "bigint",
            false,
        )];
        source_table.indexes = vec![IndexInfo {
            name: "t_id_idx".into(),
            columns: vec!["id".into()],
            unique: true,
            primary: false,
            method: None,
        }];
        source_table.foreign_keys = vec![ForeignKeyInfo {
            name: "fk_new".into(),
            columns: vec!["id".into()],
            referenced_table: "other".into(),
            referenced_columns: vec!["id".into()],
            on_delete: None,
            on_update: None,
        }];
        source.tables.push(source_table.clone());
        let mut target_table = source_table;
        target_table.indexes[0].name = "t_id_idx1".into();
        target_table.foreign_keys[0].name = "fk_old".into();
        target.tables.push(target_table);

        let report = compare(&source, &target, &CompareConfig::default());
        let table = report.tables.first().unwrap();
        assert_eq!(table.status, DiffStatus::Same, "{table:?}");
        assert!(table.indexes[0]
            .note
            .as_deref()
            .unwrap()
            .contains("名称不同"));
        assert!(table.foreign_keys[0]
            .note
            .as_deref()
            .unwrap()
            .contains("名称不同"));
        assert!(report
            .deploy_plan(&DeployOptions::default())
            .plan
            .is_empty());
    }

    #[test]
    fn config_roundtrip_and_import_export() {
        let config = CompareConfig {
            source: Some(ns("app")),
            target: Some(ns("app2")),
            ignored_tables: vec!["audit".into()],
            renames: vec![RenamePair {
                from: "old".into(),
                to: "new".into(),
            }],
            ..CompareConfig::default()
        };
        let json = config.to_json().unwrap();
        assert_eq!(CompareConfig::from_json(&json).unwrap(), config);

        let path = temp_path("config");
        save_compare_config(&path, &config).unwrap();
        assert_eq!(load_compare_config(&path), config);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ignored_text_accepts_commas_and_newlines() {
        let mut config = CompareConfig::default();
        config.set_ignored_text(" a, b，c\n d \n");
        assert_eq!(config.ignored_tables, vec!["a", "b", "c", "d"]);
        assert!(config.is_ignored("B"));
    }

    #[test]
    fn corrupt_config_falls_back_to_default() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load_compare_config(&path), CompareConfig::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn snapshot_roundtrip() {
        let path = temp_path("snapshot");
        let source = base_source();
        save_snapshot(&path, &source).unwrap();
        assert_eq!(load_snapshot(&path).unwrap(), source);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn failed_snapshots_surface_warnings() {
        let mut source = base_source();
        source.failed.push("orders：permission denied".into());
        let report = compare(&source, &base_target(), &CompareConfig::default());
        assert!(report
            .warnings
            .iter()
            .any(|line| line.contains("permission denied")));
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rustdb-compare-{name}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }
}
