//! 迁移计划（F-39 / F-40）：表结构 diff → 有序 DDL + 破坏性标记。
//!
//! 纯逻辑、无 IO。UI 只负责收集「列草稿」，方言差异全部在这里处理
//! （仍然遵守「UI 零 `match DbKind`」）。

use rustdb_db_api::{ColumnInfo, DbKind, ObjectPath};
use serde::{Deserialize, Serialize};

use crate::core::object_from_clause;
use crate::edit::ident;

/// 表设计器里的一列草稿（用户可编辑）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDraft {
    pub name: String,
    /// 方言原生类型文本，如 `varchar(50)`、`numeric(12,2)`。
    pub type_text: String,
    pub nullable: bool,
    pub default: Option<String>,
}

impl ColumnDraft {
    pub fn new(name: impl Into<String>, type_text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_text: type_text.into(),
            nullable: true,
            default: None,
        }
    }

    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    pub fn default_value(mut self, value: impl Into<String>) -> Self {
        self.default = Some(value.into());
        self
    }
}

/// 表设计器页签。UI 与 core 共用同一套枚举，避免两边各写一份字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesignerTab {
    Columns,
    Indexes,
    ForeignKeys,
    Checks,
    Triggers,
    Options,
}

impl DesignerTab {
    pub const ALL: [DesignerTab; 6] = [
        DesignerTab::Columns,
        DesignerTab::Indexes,
        DesignerTab::ForeignKeys,
        DesignerTab::Checks,
        DesignerTab::Triggers,
        DesignerTab::Options,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Columns => "列",
            Self::Indexes => "索引",
            Self::ForeignKeys => "外键",
            Self::Checks => "检查约束",
            Self::Triggers => "触发器",
            Self::Options => "选项",
        }
    }
}

/// 索引草稿。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexDraft {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub method: Option<String>,
}

impl IndexDraft {
    pub fn new(name: impl Into<String>, columns: Vec<String>) -> Self {
        Self {
            name: name.into(),
            columns,
            unique: false,
            method: None,
        }
    }
}

/// 外键草稿。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForeignKeyDraft {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    pub on_delete: Option<String>,
    pub on_update: Option<String>,
}

impl ForeignKeyDraft {
    pub fn new(
        name: impl Into<String>,
        columns: Vec<String>,
        referenced_table: impl Into<String>,
        referenced_columns: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            columns,
            referenced_table: referenced_table.into(),
            referenced_columns,
            on_delete: None,
            on_update: None,
        }
    }
}

/// 检查约束草稿。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckDraft {
    pub name: String,
    pub expression: String,
}

impl CheckDraft {
    pub fn new(name: impl Into<String>, expression: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            expression: expression.into(),
        }
    }
}

/// 表选项草稿：存储引擎 / 排序规则 / 注释（各方言支持度不同）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableOptionsDraft {
    pub engine: Option<String>,
    pub collation: Option<String>,
    pub comment: Option<String>,
}

/// 设计器的全部草稿（列 + 索引 + 外键 + 检查约束 + 选项）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DesignerDraft {
    pub columns: Vec<ColumnDraft>,
    pub indexes: Vec<IndexDraft>,
    pub foreign_keys: Vec<ForeignKeyDraft>,
    pub checks: Vec<CheckDraft>,
    pub options: TableOptionsDraft,
}

/// 校验问题的严重程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueLevel {
    Error,
    Warning,
}

/// 一条校验结论：`scope` 是页签名，`message` 面向用户。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub scope: String,
    pub message: String,
    pub level: IssueLevel,
}

/// 一条迁移语句。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigrationStatement {
    pub sql: String,
    /// 破坏性：删列、改类型、或有数据丢失风险。
    pub destructive: bool,
    /// 破坏性语句的回滚建议 / 方言差异说明。
    pub note: Option<String>,
}

impl MigrationStatement {
    fn safe(sql: String) -> Self {
        Self {
            sql,
            destructive: false,
            note: None,
        }
    }

    fn destructive(sql: String, note: impl Into<String>) -> Self {
        Self {
            sql,
            destructive: true,
            note: Some(note.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigrationPlan {
    pub statements: Vec<MigrationStatement>,
    /// 方言不支持、但用户提出过的改动（显式报告，不静默丢）。
    pub unsupported: Vec<String>,
    /// 该方言的 DDL 是否可放进事务。
    pub transactional: bool,
}

impl MigrationPlan {
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty() && self.unsupported.is_empty()
    }

    /// 预览文本：语句逐行 + 破坏性标注在语句前。
    pub fn preview_text(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        for statement in &self.statements {
            if statement.destructive {
                lines.push(format!(
                    "-- ⚠ 破坏性：{}",
                    statement.note.clone().unwrap_or_else(|| "不可回滚".into())
                ));
            } else if let Some(note) = &statement.note {
                lines.push(format!("-- {note}"));
            }
            lines.push(statement.sql.clone());
        }
        for item in &self.unsupported {
            lines.push(format!("-- 不支持：{item}"));
        }
        lines.join("\n")
    }

    pub fn executable_sql(&self) -> Vec<String> {
        self.statements
            .iter()
            .map(|statement| statement.sql.clone())
            .collect()
    }
}

fn quote(kind: DbKind, name: &str) -> String {
    ident(kind, name)
}

/// 表结构 diff：`before` 是数据库现状，`after` 是用户的草稿。
///
/// 顺序：删列 → 改类型/非空 → 加列（先腾位置再加，避免名字冲突）。
pub fn plan_changes(
    kind: DbKind,
    path: &ObjectPath,
    before: &[ColumnInfo],
    after: &[ColumnDraft],
) -> MigrationPlan {
    let table = object_from_clause(kind, path);
    let mut statements: Vec<MigrationStatement> = Vec::new();
    let mut unsupported: Vec<String> = Vec::new();

    let names_after: Vec<&str> = after.iter().map(|c| c.name.as_str()).collect();
    let names_before: Vec<&str> = before.iter().map(|c| c.meta.name.as_ref()).collect();

    // 1) 删除（原来有、现在没有）
    for old in before {
        let name = old.meta.name.to_string();
        if names_after.contains(&name.as_str()) {
            continue;
        }
        if old.is_primary_key {
            unsupported.push(format!("{name}：主键列不能通过设计器删除，请先改主键"));
            continue;
        }
        match kind {
            DbKind::Sqlite | DbKind::Postgres | DbKind::Mysql | DbKind::Mariadb | DbKind::Mssql => {
                statements.push(MigrationStatement::destructive(
                    format!("ALTER TABLE {table} DROP COLUMN {};", quote(kind, &name)),
                    "列删除后数据不可恢复",
                ));
            }
            _ => unsupported.push(format!("{name}：该方言不支持 DROP COLUMN")),
        }
    }

    // 2) 修改（同名比较类型与可空）
    for draft in after {
        let Some(old) = before
            .iter()
            .find(|column| column.meta.name.eq_ignore_ascii_case(&draft.name))
        else {
            continue;
        };
        let type_changed = !old.meta.native_type.eq_ignore_ascii_case(&draft.type_text)
            && !draft.type_text.is_empty();
        let null_changed = old.meta.nullable != draft.nullable;
        if !type_changed && !null_changed {
            continue;
        }
        let column = quote(kind, &draft.name);
        let def = format!(
            "{column} {}{}",
            draft.type_text,
            if draft.nullable { "" } else { " NOT NULL" }
        );
        match kind {
            DbKind::Postgres => {
                if type_changed {
                    statements.push(MigrationStatement::destructive(
                        format!(
                            "ALTER TABLE {table} ALTER COLUMN {column} TYPE {};",
                            draft.type_text
                        ),
                        "类型变更可能失败（需要 USING 表达式）或截断数据",
                    ));
                }
                if null_changed {
                    let action = if draft.nullable {
                        "DROP NOT NULL"
                    } else {
                        "SET NOT NULL"
                    };
                    statements.push(MigrationStatement::destructive(
                        format!("ALTER TABLE {table} ALTER COLUMN {column} {action};"),
                        "已有 NULL 行时加 NOT NULL 会失败",
                    ));
                }
            }
            DbKind::Mysql | DbKind::Mariadb => {
                statements.push(MigrationStatement::destructive(
                    format!("ALTER TABLE {table} MODIFY COLUMN {def};"),
                    "MODIFY 会重写列定义，可能截断数据",
                ));
            }
            DbKind::Mssql => {
                statements.push(MigrationStatement::destructive(
                    format!("ALTER TABLE {table} ALTER COLUMN {def};"),
                    "类型或可空变更可能失败或截断数据",
                ));
            }
            DbKind::Sqlite => {
                // R5：SQLite 改列需要重建表，明确拒绝而不是假装成功。
                unsupported.push(format!(
                    "{}：SQLite 不支持直接修改列定义，需要重建表",
                    draft.name
                ));
            }
            _ => unsupported.push(format!("{}：该方言不支持修改列", draft.name)),
        }
    }

    // 3) 新增（草稿里有、原来没有）
    for draft in after {
        if names_before
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&draft.name))
        {
            continue;
        }
        if draft.type_text.trim().is_empty() {
            unsupported.push(format!("{}：缺少类型", draft.name));
            continue;
        }
        let mut def = format!("{} {}", quote(kind, &draft.name), draft.type_text);
        if !draft.nullable {
            // 已有数据的表加 NOT NULL 列必须有默认值，否则方言会拒绝。
            match &draft.default {
                Some(default) => {
                    def.push_str(&format!(" NOT NULL DEFAULT {default}"));
                }
                None => {
                    unsupported.push(format!(
                        "{}：非空新列必须有默认值（表里已有数据）",
                        draft.name
                    ));
                    continue;
                }
            }
        } else if let Some(default) = &draft.default {
            def.push_str(&format!(" DEFAULT {default}"));
        }
        statements.push(MigrationStatement::safe(format!(
            "ALTER TABLE {table} ADD COLUMN {def};"
        )));
    }

    MigrationPlan {
        statements,
        unsupported,
        transactional: kind != DbKind::Mysql && kind != DbKind::Mariadb,
    }
}

/// 列草稿相对现状是否有实质改动（增删 / 改类型 / 改可空 / 改顺序）。
///
/// SQLite 判定「必须重建表」时用得上：SQLite 改不了列顺序，一样要重建。
pub fn columns_changed(before: &[ColumnInfo], after: &[ColumnDraft]) -> bool {
    if before.len() != after.len() {
        return true;
    }
    for (index, draft) in after.iter().enumerate() {
        // 顺序变了（草稿第 index 列不是现状第 index 列）也算改动。
        if !before[index].meta.name.eq_ignore_ascii_case(&draft.name) {
            return true;
        }
        let old = &before[index];
        if !old.meta.native_type.eq_ignore_ascii_case(&draft.type_text)
            && !draft.type_text.is_empty()
        {
            return true;
        }
        if old.meta.nullable != draft.nullable {
            return true;
        }
    }
    false
}

/// 各方言的标识符长度上限（Oracle 最严）。
fn max_ident_len(kind: DbKind) -> usize {
    match kind {
        DbKind::Oracle => 30,
        DbKind::Postgres => 63,
        DbKind::Mysql | DbKind::Mariadb => 64,
        DbKind::Mssql => 128,
        DbKind::Sqlite => 64,
    }
}

/// 名称合法性：只拦真正会破坏 SQL 拼接的字符，其余给提示而非硬失败。
fn ident_issue(kind: DbKind, name: &str) -> Option<(IssueLevel, String)> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some((IssueLevel::Error, "名称不能为空".into()));
    }
    if trimmed.chars().any(|ch| {
        ch.is_whitespace() || ch == ';' || ch == '\'' || ch == '"' || ch == '`' || ch == '\0'
    }) {
        return Some((
            IssueLevel::Error,
            format!("名称 {trimmed} 含空格或引号等非法字符"),
        ));
    }
    if trimmed.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        return Some((
            IssueLevel::Warning,
            format!("名称 {trimmed} 以数字开头，部分方言要求字母或下划线开头"),
        ));
    }
    let limit = max_ident_len(kind);
    if trimmed.chars().count() > limit {
        return Some((
            IssueLevel::Error,
            format!("名称 {trimmed} 超过 {} 字符上限", limit),
        ));
    }
    None
}

fn issue(scope: &str, level: IssueLevel, message: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        scope: scope.to_string(),
        message: message.into(),
        level,
    }
}

/// 保存前校验：命名、类型、依赖三类问题一次报全，不逐个弹窗。
pub fn validate_draft(kind: DbKind, draft: &DesignerDraft) -> Vec<ValidationIssue> {
    let mut issues: Vec<ValidationIssue> = Vec::new();
    let column_scope = DesignerTab::Columns.label();

    let mut column_names: Vec<String> = Vec::new();
    for column in &draft.columns {
        let name = column.name.trim().to_string();
        if let Some((level, message)) = ident_issue(kind, &name) {
            issues.push(issue(&column_scope, level, message));
        }
        if !name.is_empty()
            && column_names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&name))
        {
            issues.push(issue(
                &column_scope,
                IssueLevel::Error,
                format!("列名 {name} 重复"),
            ));
        }
        column_names.push(name.clone());

        let type_text = column.type_text.trim();
        if type_text.is_empty() {
            issues.push(issue(
                &column_scope,
                IssueLevel::Error,
                format!("列 {name} 缺少类型"),
            ));
        } else if type_text.contains(';') {
            issues.push(issue(
                &column_scope,
                IssueLevel::Error,
                format!("列 {name} 的类型里含分号"),
            ));
        }
    }

    let index_scope = DesignerTab::Indexes.label();
    let mut index_names: Vec<String> = Vec::new();
    for index in &draft.indexes {
        let name = index.name.trim().to_string();
        if let Some((level, message)) = ident_issue(kind, &name) {
            issues.push(issue(&index_scope, level, message));
        }
        if !name.is_empty()
            && index_names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&name))
        {
            issues.push(issue(
                &index_scope,
                IssueLevel::Error,
                format!("索引名 {name} 重复"),
            ));
        }
        index_names.push(name.clone());
        if index.columns.is_empty() {
            issues.push(issue(
                &index_scope,
                IssueLevel::Error,
                format!("索引 {name} 没有列"),
            ));
        }
        for column in &index.columns {
            if !column_names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(column.trim()))
            {
                issues.push(issue(
                    &index_scope,
                    IssueLevel::Error,
                    format!("索引 {name} 引用了不存在的列 {column}"),
                ));
            }
        }
    }

    let fk_scope = DesignerTab::ForeignKeys.label();
    let mut fk_names: Vec<String> = Vec::new();
    for fk in &draft.foreign_keys {
        let name = fk.name.trim().to_string();
        if let Some((level, message)) = ident_issue(kind, &name) {
            issues.push(issue(&fk_scope, level, message));
        }
        if !name.is_empty()
            && fk_names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&name))
        {
            issues.push(issue(
                &fk_scope,
                IssueLevel::Error,
                format!("外键名 {name} 重复"),
            ));
        }
        fk_names.push(name.clone());
        if fk.referenced_table.trim().is_empty() {
            issues.push(issue(
                &fk_scope,
                IssueLevel::Error,
                format!("外键 {name} 没有引用表"),
            ));
        }
        if fk.columns.is_empty() {
            issues.push(issue(
                &fk_scope,
                IssueLevel::Error,
                format!("外键 {name} 没有本地列"),
            ));
        }
        for column in &fk.columns {
            if !column_names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(column.trim()))
            {
                issues.push(issue(
                    &fk_scope,
                    IssueLevel::Error,
                    format!("外键 {name} 引用了不存在的列 {column}"),
                ));
            }
        }
        if !fk.referenced_columns.is_empty() && fk.columns.len() != fk.referenced_columns.len() {
            issues.push(issue(
                &fk_scope,
                IssueLevel::Error,
                format!(
                    "外键 {name} 的本地列（{} 个）与引用列（{} 个）数量不一致",
                    fk.columns.len(),
                    fk.referenced_columns.len()
                ),
            ));
        }
    }

    let check_scope = DesignerTab::Checks.label();
    let mut check_names: Vec<String> = Vec::new();
    for check in &draft.checks {
        let name = check.name.trim().to_string();
        if let Some((level, message)) = ident_issue(kind, &name) {
            issues.push(issue(&check_scope, level, message));
        }
        if !name.is_empty()
            && check_names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&name))
        {
            issues.push(issue(
                &check_scope,
                IssueLevel::Error,
                format!("检查约束名 {name} 重复"),
            ));
        }
        check_names.push(name.clone());
        let expression = check.expression.trim();
        if expression.is_empty() {
            issues.push(issue(
                &check_scope,
                IssueLevel::Error,
                format!("检查约束 {name} 没有表达式"),
            ));
        } else if expression.contains(';') {
            issues.push(issue(
                &check_scope,
                IssueLevel::Error,
                format!("检查约束 {name} 的表达式里含分号"),
            ));
        }
    }

    // 依赖：被外键引用的列被删掉时给出明确提示（执行阶段还会再拦一次）。
    for fk in &draft.foreign_keys {
        for column in &fk.columns {
            if !column_names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(column.trim()))
            {
                issues.push(issue(
                    &fk_scope,
                    IssueLevel::Warning,
                    format!(
                        "外键 {} 依赖的列 {column} 已被移除，提交前请先删除该外键",
                        fk.name.trim()
                    ),
                ));
            }
        }
    }

    issues
}

/// 校验结论里有没有阻断保存的错误。
pub fn has_blocking_issue(issues: &[ValidationIssue]) -> bool {
    issues.iter().any(|issue| issue.level == IssueLevel::Error)
}

fn empty_plan(transactional: bool) -> MigrationPlan {
    MigrationPlan {
        statements: Vec::new(),
        unsupported: Vec::new(),
        transactional,
    }
}

/// 新建索引。
pub fn plan_add_index(kind: DbKind, path: &ObjectPath, index: &IndexDraft) -> MigrationPlan {
    let table = object_from_clause(kind, path);
    let columns = index
        .columns
        .iter()
        .map(|column| quote(kind, column.trim()))
        .collect::<Vec<_>>()
        .join(", ");
    let unique = if index.unique { "UNIQUE " } else { "" };
    let method = match (kind, index.method.as_deref()) {
        (DbKind::Postgres, Some(method)) if !method.trim().is_empty() => {
            format!(" USING {}", method.trim())
        }
        _ => String::new(),
    };
    let mut plan = empty_plan(kind != DbKind::Mysql && kind != DbKind::Mariadb);
    plan.statements.push(MigrationStatement::safe(format!(
        "CREATE {unique}INDEX {} ON {table} ({columns}){method};",
        quote(kind, index.name.trim())
    )));
    plan
}

/// 删除索引。
pub fn plan_drop_index(kind: DbKind, path: &ObjectPath, name: &str) -> MigrationPlan {
    let table = object_from_clause(kind, path);
    let name = name.trim();
    let mut plan = empty_plan(kind != DbKind::Mysql && kind != DbKind::Mariadb);
    let sql = match kind {
        DbKind::Mysql | DbKind::Mariadb => {
            format!("DROP INDEX {} ON {table};", quote(kind, name))
        }
        DbKind::Mssql => format!("DROP INDEX {} ON {table};", quote(kind, name)),
        DbKind::Postgres | DbKind::Oracle => {
            // PG / Oracle 的索引名按「schema.name」解析。
            let qualified = match path.ns.schema.as_deref() {
                Some(schema) => format!("{}.{}", quote(kind, schema), quote(kind, name)),
                None => quote(kind, name),
            };
            format!("DROP INDEX {qualified};")
        }
        DbKind::Sqlite => format!("DROP INDEX {};", quote(kind, name)),
    };
    plan.statements.push(MigrationStatement::safe(sql));
    plan
}

fn foreign_key_clause(kind: DbKind, fk: &ForeignKeyDraft) -> String {
    let columns = fk
        .columns
        .iter()
        .map(|column| quote(kind, column.trim()))
        .collect::<Vec<_>>()
        .join(", ");
    let referenced = if fk.referenced_columns.is_empty() {
        String::new()
    } else {
        let names = fk
            .referenced_columns
            .iter()
            .map(|column| quote(kind, column.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" ({names})")
    };
    let mut clause = format!(
        "FOREIGN KEY ({columns}) REFERENCES {}{referenced}",
        quote(kind, fk.referenced_table.trim())
    );
    if let Some(action) = fk
        .on_delete
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clause.push_str(&format!(" ON DELETE {}", action.to_uppercase()));
    }
    if let Some(action) = fk
        .on_update
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clause.push_str(&format!(" ON UPDATE {}", action.to_uppercase()));
    }
    clause
}

/// 新增外键（逐条保存用）。
pub fn plan_add_foreign_key(
    kind: DbKind,
    path: &ObjectPath,
    fk: &ForeignKeyDraft,
) -> MigrationPlan {
    let table = object_from_clause(kind, path);
    let mut plan = empty_plan(kind != DbKind::Mysql && kind != DbKind::Mariadb);
    if kind == DbKind::Sqlite {
        plan.unsupported
            .push("SQLite 不支持直接新增外键，需要重建表".into());
        return plan;
    }
    plan.statements.push(MigrationStatement::safe(format!(
        "ALTER TABLE {table} ADD CONSTRAINT {} {};",
        quote(kind, fk.name.trim()),
        foreign_key_clause(kind, fk)
    )));
    plan
}

/// 删除外键。
pub fn plan_drop_foreign_key(kind: DbKind, path: &ObjectPath, name: &str) -> MigrationPlan {
    let table = object_from_clause(kind, path);
    let mut plan = empty_plan(kind != DbKind::Mysql && kind != DbKind::Mariadb);
    if kind == DbKind::Sqlite {
        plan.unsupported
            .push("SQLite 不支持直接删除外键，需要重建表".into());
        return plan;
    }
    let sql = match kind {
        DbKind::Mysql | DbKind::Mariadb => format!(
            "ALTER TABLE {table} DROP FOREIGN KEY {};",
            quote(kind, name.trim())
        ),
        _ => format!(
            "ALTER TABLE {table} DROP CONSTRAINT {};",
            quote(kind, name.trim())
        ),
    };
    plan.statements.push(MigrationStatement::safe(sql));
    plan
}

/// 新增检查约束。
pub fn plan_add_check(kind: DbKind, path: &ObjectPath, check: &CheckDraft) -> MigrationPlan {
    let table = object_from_clause(kind, path);
    let mut plan = empty_plan(kind != DbKind::Mysql && kind != DbKind::Mariadb);
    if kind == DbKind::Sqlite {
        plan.unsupported
            .push("SQLite 不支持直接新增检查约束，需要重建表".into());
        return plan;
    }
    plan.statements.push(MigrationStatement::safe(format!(
        "ALTER TABLE {table} ADD CONSTRAINT {} CHECK ({});",
        quote(kind, check.name.trim()),
        check.expression.trim()
    )));
    plan
}

/// 删除检查约束。
pub fn plan_drop_check(kind: DbKind, path: &ObjectPath, name: &str) -> MigrationPlan {
    let table = object_from_clause(kind, path);
    let mut plan = empty_plan(kind != DbKind::Mysql && kind != DbKind::Mariadb);
    if kind == DbKind::Sqlite {
        plan.unsupported
            .push("SQLite 不支持直接删除检查约束，需要重建表".into());
        return plan;
    }
    let sql = match kind {
        DbKind::Mysql | DbKind::Mariadb => {
            format!(
                "ALTER TABLE {table} DROP CHECK {};",
                quote(kind, name.trim())
            )
        }
        _ => format!(
            "ALTER TABLE {table} DROP CONSTRAINT {};",
            quote(kind, name.trim())
        ),
    };
    plan.statements.push(MigrationStatement::safe(sql));
    plan
}

/// 表选项（引擎 / 排序规则 / 注释）。
pub fn plan_table_options(
    kind: DbKind,
    path: &ObjectPath,
    options: &TableOptionsDraft,
) -> MigrationPlan {
    let table = object_from_clause(kind, path);
    let mut plan = empty_plan(kind != DbKind::Mysql && kind != DbKind::Mariadb);
    match kind {
        DbKind::Mysql | DbKind::Mariadb => {
            if let Some(engine) = options
                .engine
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                plan.statements.push(MigrationStatement::safe(format!(
                    "ALTER TABLE {table} ENGINE = {engine};"
                )));
            }
            if let Some(collation) = options
                .collation
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                plan.statements.push(MigrationStatement::safe(format!(
                    "ALTER TABLE {table} COLLATE = {collation};"
                )));
            }
            if let Some(comment) = options
                .comment
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                plan.statements.push(MigrationStatement::safe(format!(
                    "ALTER TABLE {table} COMMENT = {};",
                    rustdb_db_api::dialect::quote_string_literal(comment)
                )));
            }
        }
        DbKind::Postgres | DbKind::Oracle => {
            if let Some(comment) = options
                .comment
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                plan.statements.push(MigrationStatement::safe(format!(
                    "COMMENT ON TABLE {table} IS {};",
                    rustdb_db_api::dialect::quote_string_literal(comment)
                )));
            }
            if options
                .engine
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty())
            {
                plan.unsupported.push("该方言没有表级存储引擎设置".into());
            }
            if options
                .collation
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty())
            {
                plan.unsupported
                    .push("该方言的排序规则是列级属性，请在列页签设置".into());
            }
        }
        DbKind::Mssql => {
            if options
                .comment
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty())
            {
                plan.unsupported
                    .push("SQL Server 的表注释通过扩展属性维护，设计器暂不支持".into());
            }
            if options
                .engine
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty())
            {
                plan.unsupported
                    .push("SQL Server 没有表级存储引擎设置".into());
            }
        }
        DbKind::Sqlite => {
            if options
                .comment
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty())
            {
                plan.unsupported.push("SQLite 不支持表注释".into());
            }
            if options
                .engine
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty())
            {
                plan.unsupported.push("SQLite 没有表级存储引擎设置".into());
            }
            if options
                .collation
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty())
            {
                plan.unsupported
                    .push("SQLite 的排序规则是列级属性，请在列页签设置".into());
            }
        }
    }
    plan
}

/// 草稿里的列顺序变化（拖拽排序的结果）。
pub fn reorder_columns(columns: &mut Vec<ColumnDraft>, from: usize, to: usize) {
    if from >= columns.len() || to >= columns.len() || from == to {
        return;
    }
    let item = columns.remove(from);
    columns.insert(to, item);
}

/// SQLite 改列 / 加删外键与检查约束的唯一可行路径是「建新表 → 拷数据 → 换名」。
///
/// 生成的计划整段标记为破坏性，并在预览里显式提示先备份。
pub fn plan_sqlite_rebuild(
    path: &ObjectPath,
    before: &[ColumnInfo],
    after: &[ColumnDraft],
    indexes: &[IndexDraft],
    foreign_keys: &[ForeignKeyDraft],
    checks: &[CheckDraft],
) -> MigrationPlan {
    let kind = DbKind::Sqlite;
    let table = object_from_clause(kind, path);
    let new_name = format!("{}__rustdb_new", path.name);
    let new_table = object_from_clause(
        kind,
        &ObjectPath::new(path.ns.clone(), new_name.clone(), path.kind),
    );

    let mut statements: Vec<MigrationStatement> = Vec::new();
    let mut unsupported: Vec<String> = Vec::new();

    let mut defs: Vec<String> = Vec::new();
    for column in after {
        if column.type_text.trim().is_empty() {
            unsupported.push(format!("{}：缺少类型", column.name));
            continue;
        }
        let mut def = format!(
            "{} {}",
            quote(kind, column.name.trim()),
            column.type_text.trim()
        );
        if !column.nullable {
            def.push_str(" NOT NULL");
        }
        if let Some(default) = &column.default {
            def.push_str(&format!(" DEFAULT {default}"));
        }
        defs.push(def);
    }
    let primary: Vec<String> = before
        .iter()
        .filter(|column| column.is_primary_key)
        .map(|column| column.meta.name.to_string())
        .filter(|name| {
            after
                .iter()
                .any(|draft| draft.name.eq_ignore_ascii_case(name))
        })
        .collect();
    if !primary.is_empty() {
        defs.push(format!(
            "PRIMARY KEY ({})",
            primary
                .iter()
                .map(|name| quote(kind, name))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for check in checks {
        if check.expression.trim().is_empty() {
            continue;
        }
        defs.push(format!(
            "CONSTRAINT {} CHECK ({})",
            quote(kind, check.name.trim()),
            check.expression.trim()
        ));
    }
    for fk in foreign_keys {
        if fk.columns.is_empty() || fk.referenced_table.trim().is_empty() {
            continue;
        }
        defs.push(format!(
            "CONSTRAINT {} {}",
            quote(kind, fk.name.trim()),
            foreign_key_clause(kind, fk)
        ));
    }

    if defs.is_empty() {
        return MigrationPlan {
            statements,
            unsupported,
            transactional: true,
        };
    }

    statements.push(MigrationStatement::destructive(
        format!(
            "CREATE TABLE {new_table} (\n    {}\n);",
            defs.join(",\n    ")
        ),
        "重建表：先建影子表，失败时原表不受影响",
    ));

    // 只搬两边都有的列，避免把改名列当成同名搬数据。
    let kept: Vec<String> = after
        .iter()
        .filter(|draft| {
            before
                .iter()
                .any(|column| column.meta.name.eq_ignore_ascii_case(&draft.name))
        })
        .map(|draft| draft.name.trim().to_string())
        .collect();
    if !kept.is_empty() {
        let names = kept
            .iter()
            .map(|name| quote(kind, name))
            .collect::<Vec<_>>()
            .join(", ");
        statements.push(MigrationStatement::destructive(
            format!("INSERT INTO {new_table} ({names}) SELECT {names} FROM {table};"),
            "重建表：按同名列拷贝数据，新列留空",
        ));
    }

    statements.push(MigrationStatement::destructive(
        format!("DROP TABLE {table};"),
        "重建表：原表在此刻被删除，只保留新建的影子表",
    ));
    statements.push(MigrationStatement::destructive(
        format!(
            "ALTER TABLE {new_table} RENAME TO {};",
            quote(kind, &path.name)
        ),
        "重建表：影子表改回原名",
    ));
    for index in indexes {
        if index.columns.is_empty() {
            continue;
        }
        let columns = index
            .columns
            .iter()
            .map(|column| quote(kind, column.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let unique = if index.unique { "UNIQUE " } else { "" };
        statements.push(MigrationStatement::safe(format!(
            "CREATE {unique}INDEX {} ON {table} ({columns});",
            quote(kind, index.name.trim())
        )));
    }

    MigrationPlan {
        statements,
        unsupported,
        transactional: true,
    }
}

/// 迁移失败时给用户的恢复建议（按错误文本分类，不给一句「出错了」）。
pub fn recovery_hint(message: &str) -> String {
    let lower = message.to_lowercase();
    if lower.contains("already exists")
        || lower.contains("duplicate")
        || lower.contains("duplicate key")
    {
        "同名对象已存在：换个名字，或先删除已存在的索引 / 约束。".into()
    } else if lower.contains("foreign key")
        || lower.contains("constraint")
        || lower.contains("dependency")
    {
        "存在依赖：先删掉依赖该列的索引或外键，再重试。".into()
    } else if lower.contains("no such table")
        || lower.contains("doesn't exist")
        || lower.contains("does not exist")
        || lower.contains("unknown table")
        || lower.contains("not found")
    {
        "对象已改名或被删除：刷新表结构后重新提交。".into()
    } else if lower.contains("syntax") || lower.contains("parse") {
        "DDL 语法不被该方言接受：在「预览 DDL」里逐条核对后再执行。".into()
    } else if lower.contains("permission")
        || lower.contains("denied")
        || lower.contains("privilege")
    {
        "当前账号没有该对象的 DDL 权限：换用有权限的账号执行。".into()
    } else if lower.contains("timeout") || lower.contains("cancel") || lower.contains("interrupt") {
        "执行被中断：DDL 可能只完成了一部分，先核对表结构再继续。".into()
    } else {
        "先备份表数据，再在「预览 DDL」里逐条执行以定位问题语句。".into()
    }
}

/// 新建表：从列草稿直接产出 `CREATE TABLE`。
pub fn plan_create_table(
    kind: DbKind,
    path: &ObjectPath,
    columns: &[ColumnDraft],
) -> MigrationPlan {
    let table = object_from_clause(kind, path);
    let mut unsupported = Vec::new();
    let mut defs = Vec::new();
    for column in columns {
        if column.type_text.trim().is_empty() {
            unsupported.push(format!("{}：缺少类型", column.name));
            continue;
        }
        let mut def = format!("{} {}", quote(kind, &column.name), column.type_text);
        if !column.nullable {
            def.push_str(" NOT NULL");
        }
        if let Some(default) = &column.default {
            def.push_str(&format!(" DEFAULT {default}"));
        }
        defs.push(def);
    }
    if defs.is_empty() {
        return MigrationPlan {
            statements: Vec::new(),
            unsupported,
            transactional: true,
        };
    }
    MigrationPlan {
        statements: vec![MigrationStatement::safe(format!(
            "CREATE TABLE {table} (\n    {}\n);",
            defs.join(",\n    ")
        ))],
        unsupported,
        transactional: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::{LogicalType, Namespace, ObjectKind};

    fn path() -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("shop"), Some("public")),
            "orders",
            ObjectKind::Table,
        )
    }

    fn column(name: &str, native: &str, nullable: bool) -> ColumnInfo {
        let mut info = ColumnInfo::simple(name, LogicalType::Text { max_len: None }, 1);
        info.meta.native_type = native.to_string();
        info.meta.nullable = nullable;
        info
    }

    #[test]
    fn add_column_is_safe_and_quoted_per_dialect() {
        let before = vec![column("id", "bigint", false)];
        let after = vec![
            ColumnDraft::new("id", "bigint").not_null(),
            ColumnDraft::new("note", "text"),
        ];
        let plan = plan_changes(DbKind::Postgres, &path(), &before, &after);
        assert_eq!(plan.statements.len(), 1);
        assert!(!plan.statements[0].destructive);
        assert_eq!(
            plan.statements[0].sql,
            "ALTER TABLE \"shop\".\"public\".\"orders\" ADD COLUMN \"note\" text;"
        );

        let mysql = plan_changes(DbKind::Mysql, &path(), &before, &after);
        assert_eq!(
            mysql.statements[0].sql,
            "ALTER TABLE `shop`.`public`.`orders` ADD COLUMN `note` text;"
        );
    }

    #[test]
    fn drop_column_is_destructive_with_rollback_note() {
        let before = vec![column("id", "bigint", false), column("tmp", "text", true)];
        let after = vec![ColumnDraft::new("id", "bigint").not_null()];
        let plan = plan_changes(DbKind::Postgres, &path(), &before, &after);
        assert_eq!(plan.statements.len(), 1);
        assert!(plan.statements[0].destructive);
        assert!(plan.statements[0]
            .note
            .as_deref()
            .unwrap()
            .contains("不可恢复"));
        assert!(plan.preview_text().contains("-- ⚠ 破坏性"));
    }

    #[test]
    fn sqlite_reports_unsupported_instead_of_guessing() {
        let before = vec![column("id", "INTEGER", false)];
        let after = vec![ColumnDraft::new("id", "TEXT").not_null()];
        let plan = plan_changes(DbKind::Sqlite, &path(), &before, &after);
        assert!(plan.statements.is_empty(), "不应生成猜测的 SQL");
        assert_eq!(plan.unsupported.len(), 1);
        assert!(
            plan.unsupported[0].contains("重建表"),
            "{:?}",
            plan.unsupported
        );
        assert!(plan.preview_text().contains("-- 不支持"));
    }

    #[test]
    fn new_not_null_column_requires_default() {
        let before = vec![column("id", "bigint", false)];
        let after = vec![
            ColumnDraft::new("id", "bigint").not_null(),
            ColumnDraft::new("state", "text").not_null(),
        ];
        let plan = plan_changes(DbKind::Postgres, &path(), &before, &after);
        assert!(plan.statements.is_empty());
        assert!(plan.unsupported[0].contains("默认值"));

        let after = vec![
            ColumnDraft::new("id", "bigint").not_null(),
            ColumnDraft::new("state", "text")
                .not_null()
                .default_value("'new'"),
        ];
        let plan = plan_changes(DbKind::Postgres, &path(), &before, &after);
        assert_eq!(
            plan.statements[0].sql,
            "ALTER TABLE \"shop\".\"public\".\"orders\" ADD COLUMN \"state\" text NOT NULL DEFAULT 'new';"
        );
    }

    #[test]
    fn primary_key_cannot_be_dropped() {
        let mut id = column("id", "bigint", false);
        id.is_primary_key = true;
        let plan = plan_changes(DbKind::Postgres, &path(), &[id], &[]);
        assert!(plan.statements.is_empty());
        assert!(plan.unsupported[0].contains("主键"));
    }

    #[test]
    fn create_table_plan_lists_columns_in_order() {
        let plan = plan_create_table(
            DbKind::Postgres,
            &path(),
            &[
                ColumnDraft::new("id", "bigint").not_null(),
                ColumnDraft::new("name", "text"),
            ],
        );
        assert_eq!(plan.statements.len(), 1);
        assert!(plan.statements[0].sql.starts_with("CREATE TABLE"));
        assert!(plan.statements[0].sql.contains("\"id\" bigint NOT NULL"));
        assert!(plan.statements[0].sql.contains("\"name\" text"));
        assert!(plan.transactional);
    }

    #[test]
    fn mysql_alter_is_not_transactional() {
        let plan = plan_changes(
            DbKind::Mysql,
            &path(),
            &[column("id", "bigint", false)],
            &[
                ColumnDraft::new("id", "bigint").not_null(),
                ColumnDraft::new("x", "int"),
            ],
        );
        assert!(!plan.transactional, "MySQL DDL 会隐式提交，必须如实标注");
    }

    #[test]
    fn type_change_follows_dialect_syntax() {
        let before = vec![column("total", "numeric(12,2)", false)];
        let after = vec![ColumnDraft::new("total", "numeric(10,2)").not_null()];
        let pg = plan_changes(DbKind::Postgres, &path(), &before, &after);
        assert!(pg.statements[0]
            .sql
            .contains("ALTER COLUMN \"total\" TYPE numeric(10,2)"));
        let mssql = plan_changes(DbKind::Mssql, &path(), &before, &after);
        assert!(
            mssql.statements[0]
                .sql
                .contains("ALTER COLUMN [total] numeric(10,2) NOT NULL"),
            "{}",
            mssql.statements[0].sql
        );
    }

    fn draft(columns: Vec<ColumnDraft>) -> DesignerDraft {
        DesignerDraft {
            columns,
            ..Default::default()
        }
    }

    /// SQLite 没有 schema，命名空间只有库名 `main`。
    fn sqlite_path() -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("main"), None::<String>),
            "orders",
            ObjectKind::Table,
        )
    }

    #[test]
    fn validation_reports_duplicate_and_missing_type() {
        let draft = draft(vec![
            ColumnDraft::new("id", "bigint").not_null(),
            ColumnDraft::new("id", "text"),
            ColumnDraft::new("note", ""),
        ]);
        let issues = validate_draft(DbKind::Postgres, &draft);
        assert!(
            issues
                .iter()
                .any(|i| i.level == IssueLevel::Error && i.message.contains("重复")),
            "{issues:?}"
        );
        assert!(
            issues
                .iter()
                .any(|i| i.level == IssueLevel::Error && i.message.contains("缺少类型")),
            "{issues:?}"
        );
        assert!(has_blocking_issue(&issues));
    }

    #[test]
    fn validation_checks_identifier_rules_and_length() {
        let long = "x".repeat(40);
        let draft = draft(vec![
            ColumnDraft::new("bad name", "text"),
            ColumnDraft::new(&long, "text"),
        ]);
        let issues = validate_draft(DbKind::Oracle, &draft);
        assert!(
            issues.iter().any(|i| i.message.contains("非法字符")),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|i| i.message.contains("30 字符上限")),
            "{issues:?}"
        );
    }

    #[test]
    fn validation_checks_dependency_of_indexes_and_foreign_keys() {
        let mut d = draft(vec![ColumnDraft::new("id", "bigint").not_null()]);
        d.indexes
            .push(IndexDraft::new("idx_missing", vec!["ghost".into()]));
        d.foreign_keys.push(ForeignKeyDraft::new(
            "fk_missing",
            vec!["ghost".into()],
            "orders",
            vec!["id".into()],
        ));
        let issues = validate_draft(DbKind::Postgres, &d);
        assert!(
            issues
                .iter()
                .any(|i| i.scope == "索引" && i.message.contains("不存在的列")),
            "{issues:?}"
        );
        assert!(
            issues
                .iter()
                .any(|i| i.scope == "外键" && i.message.contains("不存在的列")),
            "{issues:?}"
        );
    }

    #[test]
    fn validation_flags_column_count_mismatch() {
        let mut d = draft(vec![
            ColumnDraft::new("a", "int"),
            ColumnDraft::new("b", "int"),
        ]);
        d.foreign_keys.push(ForeignKeyDraft::new(
            "fk_x",
            vec!["a".into()],
            "orders",
            vec!["id".into(), "id2".into()],
        ));
        let issues = validate_draft(DbKind::Postgres, &d);
        assert!(
            issues.iter().any(|i| i.message.contains("数量不一致")),
            "{issues:?}"
        );
    }

    #[test]
    fn add_index_uses_dialect_quoting() {
        let index = IndexDraft::new("idx_name", vec!["name".into()]);
        let pg = plan_add_index(DbKind::Postgres, &path(), &index);
        assert_eq!(
            pg.statements[0].sql,
            "CREATE INDEX \"idx_name\" ON \"shop\".\"public\".\"orders\" (\"name\");"
        );
        let mysql = plan_add_index(DbKind::Mysql, &path(), &index);
        assert!(mysql.statements[0].sql.contains("`idx_name`"));
        let unique = IndexDraft {
            unique: true,
            ..index
        };
        assert!(
            plan_add_index(DbKind::Postgres, &path(), &unique).statements[0]
                .sql
                .starts_with("CREATE UNIQUE INDEX")
        );
    }

    #[test]
    fn drop_index_differs_between_dialects() {
        let pg = plan_drop_index(DbKind::Postgres, &path(), "idx_name");
        assert_eq!(pg.statements[0].sql, "DROP INDEX \"public\".\"idx_name\";");
        let mysql = plan_drop_index(DbKind::Mysql, &path(), "idx_name");
        assert_eq!(
            mysql.statements[0].sql,
            "DROP INDEX `idx_name` ON `shop`.`public`.`orders`;"
        );
        let sqlite = plan_drop_index(DbKind::Sqlite, &path(), "idx_name");
        assert_eq!(sqlite.statements[0].sql, "DROP INDEX \"idx_name\";");
    }

    #[test]
    fn add_foreign_key_builds_clause_with_actions() {
        let mut fk = ForeignKeyDraft::new(
            "fk_customer",
            vec!["customer_id".into()],
            "customers",
            vec!["id".into()],
        );
        fk.on_delete = Some("cascade".into());
        let plan = plan_add_foreign_key(DbKind::Postgres, &path(), &fk);
        assert_eq!(
            plan.statements[0].sql,
            "ALTER TABLE \"shop\".\"public\".\"orders\" ADD CONSTRAINT \"fk_customer\" \
             FOREIGN KEY (\"customer_id\") REFERENCES \"customers\" (\"id\") ON DELETE CASCADE;"
        );
        let mysql = plan_add_foreign_key(DbKind::Mysql, &path(), &fk);
        assert!(mysql.statements[0].sql.contains("`fk_customer`"));
    }

    #[test]
    fn sqlite_foreign_key_requires_rebuild() {
        let fk = ForeignKeyDraft::new("fk_x", vec!["a".into()], "orders", vec!["id".into()]);
        let plan = plan_add_foreign_key(DbKind::Sqlite, &path(), &fk);
        assert!(plan.statements.is_empty());
        assert!(
            plan.unsupported[0].contains("重建表"),
            "{:?}",
            plan.unsupported
        );
    }

    #[test]
    fn drop_foreign_key_uses_dialect_specific_syntax() {
        let mysql = plan_drop_foreign_key(DbKind::Mysql, &path(), "fk_x");
        assert!(mysql.statements[0].sql.contains("DROP FOREIGN KEY"));
        let pg = plan_drop_foreign_key(DbKind::Postgres, &path(), "fk_x");
        assert!(pg.statements[0].sql.contains("DROP CONSTRAINT"));
        let mssql = plan_drop_foreign_key(DbKind::Mssql, &path(), "fk_x");
        assert!(mssql.statements[0].sql.contains("DROP CONSTRAINT [fk_x]"));
    }

    #[test]
    fn table_options_follow_dialect_support() {
        let options = TableOptionsDraft {
            engine: Some("InnoDB".into()),
            collation: Some("utf8mb4_general_ci".into()),
            comment: Some("订单表".into()),
        };
        let mysql = plan_table_options(DbKind::Mysql, &path(), &options);
        assert_eq!(mysql.statements.len(), 3);
        assert!(mysql.statements[0].sql.contains("ENGINE = InnoDB"));
        assert!(mysql.statements[2].sql.contains("COMMENT = '订单表'"));

        let pg = plan_table_options(DbKind::Postgres, &path(), &options);
        assert_eq!(pg.statements.len(), 1);
        assert!(pg.statements[0].sql.starts_with("COMMENT ON TABLE"));
        assert_eq!(pg.unsupported.len(), 2, "{:?}", pg.unsupported);

        let sqlite = plan_table_options(DbKind::Sqlite, &path(), &options);
        assert!(sqlite.statements.is_empty());
        assert_eq!(sqlite.unsupported.len(), 3);
    }

    #[test]
    fn sqlite_rebuild_plan_creates_shadow_table_and_copies_data() {
        let mut before = vec![column("id", "INTEGER", false)];
        before[0].is_primary_key = true;
        let after = vec![
            ColumnDraft::new("id", "INTEGER").not_null(),
            ColumnDraft::new("note", "TEXT"),
        ];
        let index = IndexDraft::new("idx_note", vec!["note".into()]);
        let fk = ForeignKeyDraft::new("fk_x", vec!["id".into()], "orders", vec!["id".into()]);
        let check = CheckDraft::new("ck_id", "id > 0");
        let plan = plan_sqlite_rebuild(&sqlite_path(), &before, &after, &[index], &[fk], &[check]);
        let sql = plan.executable_sql().join("\n");
        assert!(sql.contains("CREATE TABLE \"orders__rustdb_new\""), "{sql}");
        assert!(
            sql.contains("INSERT INTO \"orders__rustdb_new\" (\"id\") SELECT \"id\""),
            "{sql}"
        );
        assert!(sql.contains("DROP TABLE \"orders\""), "{sql}");
        assert!(sql.contains("RENAME TO \"orders\""), "{sql}");
        assert!(sql.contains("CONSTRAINT \"fk_x\" FOREIGN KEY"), "{sql}");
        assert!(sql.contains("CHECK (id > 0)"), "{sql}");
        assert!(sql.contains("CREATE INDEX \"idx_note\""), "{sql}");
        assert!(
            plan.statements[..4].iter().all(|s| s.destructive),
            "重建表的前四步（建表/拷数据/删表/改名）都是破坏性操作"
        );
        assert!(plan.preview_text().contains("-- ⚠ 破坏性"));
    }

    #[test]
    fn sqlite_rebuild_skips_columns_absent_in_both_sides() {
        let before = vec![column("id", "INTEGER", false)];
        let after = vec![
            ColumnDraft::new("id", "INTEGER").not_null(),
            ColumnDraft::new("brand_new", "TEXT"),
        ];
        let plan = plan_sqlite_rebuild(&sqlite_path(), &before, &after, &[], &[], &[]);
        let sql = plan.executable_sql().join("\n");
        assert!(!sql.contains("\"brand_new\") SELECT"), "{sql}");
    }

    #[test]
    fn reorder_columns_moves_item() {
        let mut columns = vec![
            ColumnDraft::new("a", "int"),
            ColumnDraft::new("b", "int"),
            ColumnDraft::new("c", "int"),
        ];
        reorder_columns(&mut columns, 0, 2);
        let names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["b", "c", "a"]);
        reorder_columns(&mut columns, 5, 0);
        assert_eq!(columns.len(), 3, "越界下标必须被忽略");
    }

    #[test]
    fn recovery_hint_classifies_common_failures() {
        assert!(recovery_hint("relation already exists").contains("同名"));
        assert!(recovery_hint("FOREIGN KEY constraint failed").contains("依赖"));
        assert!(recovery_hint("no such table: orders").contains("刷新"));
        assert!(recovery_hint("syntax error at or near").contains("逐条核对"));
        assert!(recovery_hint("permission denied").contains("权限"));
        assert!(recovery_hint("something odd").contains("备份"));
    }

    #[test]
    fn columns_changed_detects_type_null_count_and_order() {
        let before = vec![column("id", "bigint", false), column("note", "text", true)];
        let same = vec![
            ColumnDraft::new("id", "bigint").not_null(),
            ColumnDraft::new("note", "text"),
        ];
        assert!(!columns_changed(&before, &same));

        let retyped = vec![
            ColumnDraft::new("id", "bigint").not_null(),
            ColumnDraft::new("note", "varchar(50)"),
        ];
        assert!(columns_changed(&before, &retyped));

        let not_null = vec![
            ColumnDraft::new("id", "bigint").not_null(),
            ColumnDraft::new("note", "text").not_null(),
        ];
        assert!(columns_changed(&before, &not_null));

        let added = vec![
            ColumnDraft::new("id", "bigint").not_null(),
            ColumnDraft::new("note", "text"),
            ColumnDraft::new("extra", "int"),
        ];
        assert!(columns_changed(&before, &added));

        let reordered = vec![
            ColumnDraft::new("note", "text"),
            ColumnDraft::new("id", "bigint").not_null(),
        ];
        assert!(columns_changed(&before, &reordered));
    }
}
