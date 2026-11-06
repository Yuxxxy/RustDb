use crate::error::DriverError;
use crate::session::ColumnMeta;
use crate::value::LogicalType;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// 两级命名空间：屏蔽 MySQL / PG / Oracle / SQLite / SQL Server 的层级差异。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Namespace {
    /// 物理库：MySQL database / PG database / SQLite main|attached / MSSQL database
    pub catalog: Option<Arc<str>>,
    /// 逻辑模式：PG schema / Oracle user / MSSQL schema；MySQL 与 SQLite 为 None
    pub schema: Option<Arc<str>>,
}

impl Namespace {
    pub fn new(catalog: Option<impl Into<Arc<str>>>, schema: Option<impl Into<Arc<str>>>) -> Self {
        Self {
            catalog: catalog.map(Into::into),
            schema: schema.map(Into::into),
        }
    }

    /// UI 展示用（不参与 SQL 拼接）。
    pub fn display_label(&self) -> String {
        match (&self.catalog, &self.schema) {
            (Some(c), Some(s)) => format!("{c}.{s}"),
            (Some(c), None) => c.to_string(),
            (None, Some(s)) => s.to_string(),
            (None, None) => "default".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    Sequence,
    Function,
    Procedure,
    Trigger,
    Index,
    Synonym,
    Type,
    Package,
}

impl ObjectKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::View => "view",
            Self::MaterializedView => "materialized view",
            Self::Sequence => "sequence",
            Self::Function => "function",
            Self::Procedure => "procedure",
            Self::Trigger => "trigger",
            Self::Index => "index",
            Self::Synonym => "synonym",
            Self::Type => "type",
            Self::Package => "package",
        }
    }

    /// 树形展示时的分组顺序权重（与枚举判别值解耦，允许后续插入新种类）。
    pub fn sort_rank(self) -> u8 {
        match self {
            Self::Table => 0,
            Self::View => 1,
            Self::MaterializedView => 2,
            Self::Procedure => 3,
            Self::Function => 4,
            Self::Trigger => 5,
            Self::Sequence => 6,
            Self::Type => 7,
            Self::Index => 8,
            Self::Package => 9,
            Self::Synonym => 10,
        }
    }
}

/// 对象种类掩码（避免为一次查询构造 HashSet）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectKindMask(u32);

impl ObjectKindMask {
    pub const ALL: Self = Self(u32::MAX);

    pub fn only(kind: ObjectKind) -> Self {
        Self(1 << (kind as u32))
    }

    pub fn contains(self, kind: ObjectKind) -> bool {
        self.0 & (1 << (kind as u32)) != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectPath {
    pub ns: Namespace,
    pub name: Arc<str>,
    pub kind: ObjectKind,
}

impl ObjectPath {
    pub fn new(ns: Namespace, name: impl Into<Arc<str>>, kind: ObjectKind) -> Self {
        Self {
            ns,
            name: name.into(),
            kind,
        }
    }

    pub fn display_label(&self) -> String {
        format!("{}.{}", self.ns.display_label(), self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectSummary {
    pub path: ObjectPath,
    pub row_estimate: Option<u64>,
    pub comment: Option<String>,
    pub has_primary_key: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DdlSource {
    /// 服务端原生导出（SHOW CREATE / pg_get_*def / DBMS_METADATA）
    Native,
    /// 由 DdlGenerator 合成
    Synthesized,
    Unavailable,
}

impl DdlSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Native => "原生",
            Self::Synthesized => "合成",
            Self::Unavailable => "不可用",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub meta: ColumnMeta,
    pub ordinal: u32,
    pub default: Option<String>,
    pub is_primary_key: bool,
    pub is_auto_increment: bool,
    pub is_generated: bool,
    pub comment: Option<String>,
    pub charset: Option<String>,
    pub collation: Option<String>,
}

impl ColumnInfo {
    pub fn simple(name: impl Into<Arc<str>>, logical_type: LogicalType, ordinal: u32) -> Self {
        Self {
            meta: ColumnMeta::new(name, logical_type),
            ordinal,
            default: None,
            is_primary_key: false,
            is_auto_increment: false,
            is_generated: false,
            comment: None,
            charset: None,
            collation: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    pub method: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    pub on_delete: Option<String>,
    pub on_update: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckConstraint {
    pub name: String,
    pub expression: String,
}

/// 表上的触发器。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerInfo {
    pub name: String,
    /// 触发时机（BEFORE / AFTER / INSTEAD OF），驱动拿不到时为 `None`。
    pub timing: Option<String>,
    /// 触发事件（INSERT / UPDATE / DELETE）。
    pub event: Option<String>,
    /// 触发体 SQL。
    pub statement: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableDetail {
    pub summary: ObjectSummary,
    pub columns: Vec<ColumnInfo>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
    pub checks: Vec<CheckConstraint>,
    /// 表上的触发器：`None` 表示驱动未提供该元数据，`Some(vec![])` 表示确实没有。
    pub triggers: Option<Vec<TriggerInfo>>,
    pub engine: Option<String>,
    pub collation: Option<String>,
    pub comment: Option<String>,
    pub ddl_source: DdlSource,
}

/// 一个对象的 DDL 文本 + 来源标记（对齐需求 F-13）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectDdl {
    pub text: String,
    pub source: DdlSource,
}

/// 依赖列表只取前 N 条（F-83：只直接依赖，不递归）。
pub const MAX_DEPENDENCIES: usize = 200;

/// 一条直接依赖：`path` 是被引用 / 引用的对象。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyInfo {
    pub path: ObjectPath,
    /// 方言原文的依赖类型（例如 PG 的 `n` / MySQL 的 `VIEW` / Oracle 的 `REF`）。
    pub dependency_type: String,
    /// 跨库 / 跨 schema 的远端标识；与当前对象同一库时为空。
    pub remote: Option<String>,
}

/// 对象依赖（F-83）：只含**直接**依赖，不递归展开。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyList {
    /// 本对象依赖的对象。
    pub depends_on: Vec<DependencyInfo>,
    /// 依赖本对象的对象。
    pub depended_by: Vec<DependencyInfo>,
    /// 达到 [`MAX_DEPENDENCIES`] 被截断。
    pub truncated: bool,
    /// 驱动的补充说明（例如某些方言只能看到视图→表的依赖）。
    pub note: Option<String>,
}

impl DependencyList {
    /// 追加「本对象依赖的对象」：同 kind + 同命名空间 + 同名去重，超上限置截断。
    pub fn push_depends_on(&mut self, info: DependencyInfo) {
        push_unique(&mut self.depends_on, info, &mut self.truncated);
    }

    /// 追加「依赖本对象的对象」。
    pub fn push_depended_by(&mut self, info: DependencyInfo) {
        push_unique(&mut self.depended_by, info, &mut self.truncated);
    }

    pub fn is_empty(&self) -> bool {
        self.depends_on.is_empty() && self.depended_by.is_empty()
    }
}

fn push_unique(list: &mut Vec<DependencyInfo>, info: DependencyInfo, truncated: &mut bool) {
    let duplicate = list.iter().any(|existing| {
        existing.path.ns == info.path.ns
            && existing.path.name == info.path.name
            && existing.path.kind == info.path.kind
    });
    if duplicate {
        return;
    }
    if list.len() >= MAX_DEPENDENCIES {
        *truncated = true;
        return;
    }
    list.push(info);
}

/// 分区列表最多返回的条目数（F-85）。
pub const MAX_PARTITIONS: usize = 500;

/// 一个一级分区（F-85）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionInfo {
    /// 分区名（不含 schema）。
    pub name: String,
    /// 边界原文（例如 `FOR VALUES FROM ('2026-01-01') TO ('2026-02-01')`）。
    pub bounds: String,
    /// 行估算；驱动拿不到时为 `None`。
    pub row_estimate: Option<u64>,
    /// 是否为 DEFAULT 分区。
    pub is_default: bool,
}

/// 分区概况：策略与分区键来自父表，`partitions` 只含**一级**分区。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionList {
    /// `range` / `list` / `hash`；`None` 表示该表不是分区表。
    pub strategy: Option<String>,
    /// 分区键表达式（方言原文）。
    pub key: Option<String>,
    pub partitions: Vec<PartitionInfo>,
    /// 达到 [`MAX_PARTITIONS`] 被截断。
    pub truncated: bool,
    /// 驱动的补充说明（例如「该表不是分区表」）。
    pub note: Option<String>,
}

impl PartitionList {
    /// 追加一个分区：同名去重，超上限置截断。
    pub fn push(&mut self, info: PartitionInfo) {
        if self
            .partitions
            .iter()
            .any(|existing| existing.name == info.name)
        {
            return;
        }
        if self.partitions.len() >= MAX_PARTITIONS {
            self.truncated = true;
            return;
        }
        self.partitions.push(info);
    }

    pub fn is_partitioned(&self) -> bool {
        self.strategy.is_some()
    }
}

#[async_trait::async_trait]
pub trait MetadataProvider: Send + Sync {
    async fn namespaces(&self) -> Result<Vec<Namespace>, DriverError>;

    async fn objects(
        &self,
        ns: &Namespace,
        kinds: ObjectKindMask,
    ) -> Result<Vec<ObjectSummary>, DriverError>;

    async fn columns(&self, path: &ObjectPath) -> Result<Vec<ColumnInfo>, DriverError>;

    async fn table(&self, path: &ObjectPath) -> Result<TableDetail, DriverError> {
        let columns = self.columns(path).await?;
        Ok(TableDetail {
            summary: ObjectSummary {
                path: path.clone(),
                row_estimate: None,
                comment: None,
                has_primary_key: None,
            },
            columns,
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            checks: Vec::new(),
            triggers: None,
            engine: None,
            collation: None,
            comment: None,
            ddl_source: DdlSource::Unavailable,
        })
    }

    /// 表上的触发器。默认不支持（返回 `None`），驱动按能力覆盖。
    async fn triggers(&self, _path: &ObjectPath) -> Result<Option<Vec<TriggerInfo>>, DriverError> {
        Ok(None)
    }

    /// 对象依赖（F-83）。默认不支持（返回 `None`），驱动按能力覆盖。
    ///
    /// 只返回直接依赖；实现方负责去重与截断到 [`MAX_DEPENDENCIES`]。
    async fn dependencies(
        &self,
        _path: &ObjectPath,
    ) -> Result<Option<DependencyList>, DriverError> {
        Ok(None)
    }

    /// 列出分区（F-85）。默认不支持（返回 `None`）。
    ///
    /// 支持但目标不是分区表时返回 `Some(PartitionList { strategy: None, note: … })`，
    /// 让 UI 能区分「驱动不支持」与「这张表没分区」。
    async fn partitions(&self, _path: &ObjectPath) -> Result<Option<PartitionList>, DriverError> {
        Ok(None)
    }

    async fn indexes(&self, _path: &ObjectPath) -> Result<Vec<IndexInfo>, DriverError> {
        Ok(Vec::new())
    }

    async fn foreign_keys(&self, _path: &ObjectPath) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        Ok(Vec::new())
    }

    async fn row_count_estimate(&self, _path: &ObjectPath) -> Result<Option<u64>, DriverError> {
        Ok(None)
    }

    async fn ddl(&self, _path: &ObjectPath) -> Result<DdlSource, DriverError> {
        Ok(DdlSource::Unavailable)
    }

    /// 取对象的 DDL 文本。服务端能原生导出就用原生（`DdlSource::Native`），
    /// 否则驱动可自行合成；两者都做不到时返回 `None`。
    async fn object_ddl(&self, path: &ObjectPath) -> Result<Option<ObjectDdl>, DriverError> {
        match self.ddl(path).await? {
            DdlSource::Unavailable => Ok(None),
            source => Ok(Some(ObjectDdl {
                text: format!(
                    "-- {} 的 DDL 暂不可用（{}）",
                    path.display_label(),
                    source.label()
                ),
                source,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(schema: &str, name: &str, kind: ObjectKind) -> DependencyInfo {
        DependencyInfo {
            path: ObjectPath::new(Namespace::new(Some("db"), Some(schema)), name, kind),
            dependency_type: "n".into(),
            remote: None,
        }
    }

    #[test]
    fn dependency_list_dedups_by_object_identity() {
        let mut list = DependencyList::default();
        list.push_depends_on(dep("public", "users", ObjectKind::Table));
        list.push_depends_on(dep("public", "users", ObjectKind::Table));
        list.push_depends_on(dep("public", "users", ObjectKind::View));
        list.push_depended_by(dep("public", "user_view", ObjectKind::View));
        assert_eq!(list.depends_on.len(), 2);
        assert_eq!(list.depended_by.len(), 1);
        assert!(!list.truncated);
        assert!(!list.is_empty());
    }

    #[test]
    fn dependency_list_truncates_at_limit() {
        let mut list = DependencyList::default();
        for index in 0..=MAX_DEPENDENCIES {
            list.push_depends_on(dep("public", &format!("t{index}"), ObjectKind::Table));
        }
        assert_eq!(list.depends_on.len(), MAX_DEPENDENCIES);
        assert!(list.truncated, "超上限应标记截断");
        // 重复项不占用截断名额。
        let mut exact = DependencyList::default();
        for index in 0..MAX_DEPENDENCIES {
            exact.push_depends_on(dep("public", &format!("t{index}"), ObjectKind::Table));
        }
        assert!(!exact.truncated);
    }

    fn part(name: &str) -> PartitionInfo {
        PartitionInfo {
            name: name.into(),
            bounds: "DEFAULT".into(),
            row_estimate: None,
            is_default: true,
        }
    }

    #[test]
    fn partition_list_dedups_by_name_and_truncates() {
        let mut list = PartitionList::default();
        list.push(part("p0"));
        list.push(part("p0"));
        assert_eq!(list.partitions.len(), 1);
        assert!(!list.truncated);
        assert!(!list.is_partitioned());

        for index in 0..=MAX_PARTITIONS {
            list.push(part(&format!("p{index}")));
        }
        assert_eq!(list.partitions.len(), MAX_PARTITIONS);
        assert!(list.truncated, "超上限应标记截断");
    }
}
