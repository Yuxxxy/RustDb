//! 整库逻辑导出（F-76）：把结构、数据与其它对象渲染成可回放的 SQL。
//!
//! 设计口径（需求文档 19.3）：
//! * 流式写盘，不要求先把整库数据拼进内存；
//! * `INSERT` 按行数与方言上限分批（Oracle 只支持单行 `VALUES`，SQL Server 单条上限 1000 行）；
//! * 明确标注**不是一致性快照**；
//! * 结构 / 数据 / 其它对象 / `DROP` / gzip 都是显式选项，默认不删、默认不压。
//!
//! 本模块只做文本渲染，不连数据库；驱动分发放在 [`crate::core::Core::request_dump`]。

use crate::core::object_from_clause;
use crate::edit::{ident, literal};
use rustdb_db_api::{DbKind, Namespace, ObjectKind, ObjectPath, Value};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};

/// 单次导出最多包含的表数；超出只在头部标注，不静默丢。
pub const MAX_DUMP_TABLES: usize = 500;
/// 单次导出最多包含的其它对象数。
pub const MAX_DUMP_OBJECTS: usize = 1000;
/// 默认 `INSERT` 每批行数。
pub const DEFAULT_INSERT_BATCH: usize = 500;
/// SQL Server 单条 `INSERT … VALUES` 上限。
const MSSQL_INSERT_LIMIT: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DumpErrorMode {
    /// 把失败写成 SQL 注释并继续。
    Comment,
    /// 首个失败即终止导出。
    Abort,
}

impl DumpErrorMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Comment => "写入注释并继续",
            Self::Abort => "遇到错误即终止",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DumpOptions {
    #[serde(default = "yes")]
    pub include_structure: bool,
    #[serde(default = "yes")]
    pub include_data: bool,
    #[serde(default = "yes")]
    pub include_objects: bool,
    #[serde(default)]
    pub include_drops: bool,
    #[serde(default)]
    pub gzip: bool,
    #[serde(default = "default_batch")]
    pub batch_rows: usize,
    #[serde(default = "default_error_mode")]
    pub error_mode: DumpErrorMode,
}

fn yes() -> bool {
    true
}

fn default_batch() -> usize {
    DEFAULT_INSERT_BATCH
}

fn default_error_mode() -> DumpErrorMode {
    DumpErrorMode::Comment
}

impl Default for DumpOptions {
    fn default() -> Self {
        Self {
            include_structure: true,
            include_data: true,
            include_objects: true,
            include_drops: false,
            gzip: false,
            batch_rows: DEFAULT_INSERT_BATCH,
            error_mode: DumpErrorMode::Comment,
        }
    }
}

impl DumpOptions {
    /// 按方言夹取单条 `INSERT` 的行数上限。
    pub fn batch_limit(&self, driver: DbKind) -> usize {
        let rows = self.batch_rows.clamp(1, MSSQL_INSERT_LIMIT);
        match driver {
            // Oracle 没有多行 `VALUES` 语法。
            DbKind::Oracle => 1,
            DbKind::Mssql => rows.min(MSSQL_INSERT_LIMIT),
            _ => rows,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DumpTableInput {
    pub path: ObjectPath,
    /// 驱动给出的表 DDL；`None` 且无错误表示驱动不提供。
    pub ddl: Option<String>,
    /// 取 DDL 失败的原因。
    pub ddl_error: Option<String>,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// 读数据失败的原因（分页途中失败也走这里）。
    pub data_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DumpObjectInput {
    pub path: ObjectPath,
    pub ddl: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DumpInput {
    pub driver: DbKind,
    pub namespace: Namespace,
    pub tables: Vec<DumpTableInput>,
    pub objects: Vec<DumpObjectInput>,
    /// 因上限未导出的表数。
    pub skipped: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DumpSummary {
    pub tables: usize,
    pub objects: usize,
    pub rows: u64,
    pub failed_rows: u64,
    pub failed_tables: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TableDumpOutcome {
    pub rows: u64,
    pub failed_rows: u64,
}

/// `DROP TABLE` 的方言差异：Oracle 没有 `IF EXISTS`，一并清掉外键。
pub fn drop_table_sql(driver: DbKind, path: &ObjectPath) -> String {
    let name = object_from_clause(driver, path);
    match driver {
        DbKind::Oracle => format!("DROP TABLE {name} CASCADE CONSTRAINTS;"),
        _ => format!("DROP TABLE IF EXISTS {name};"),
    }
}

/// 逐表 / 逐对象写 SQL；错误策略在构造时固定。
pub struct DumpWriter<W: Write> {
    out: W,
    driver: DbKind,
    options: DumpOptions,
    summary: DumpSummary,
}

impl<W: Write> DumpWriter<W> {
    pub fn new(out: W, driver: DbKind, options: DumpOptions) -> Self {
        Self {
            out,
            driver,
            options,
            summary: DumpSummary::default(),
        }
    }

    pub fn summary(&self) -> DumpSummary {
        self.summary
    }

    pub fn into_inner(self) -> W {
        self.out
    }

    /// 头部：用途、命名空间、选项与「非一致性快照」警告。
    pub fn header(&mut self, namespace: &Namespace, skipped: usize) -> io::Result<()> {
        self.summary.skipped = skipped;
        writeln!(self.out, "-- RustDb 逻辑导出")?;
        writeln!(self.out, "-- 命名空间：{}", namespace.display_label())?;
        writeln!(
            self.out,
            "-- 内容：{}{}{}",
            if self.options.include_structure {
                "结构"
            } else {
                "不含结构"
            },
            if self.options.include_data {
                " + 数据"
            } else {
                "（不含数据）"
            },
            if self.options.include_objects {
                " + 其它对象"
            } else {
                ""
            },
        )?;
        writeln!(
            self.out,
            "-- 注意：导出不是一致性快照；导出期间的数据变更不会反映在这里。"
        )?;
        if self.summary.skipped > 0 {
            writeln!(
                self.out,
                "-- 有 {} 张表因单次上限（{}）未导出。",
                self.summary.skipped, MAX_DUMP_TABLES
            )?;
        }
        writeln!(self.out)?;
        Ok(())
    }

    /// 写一张表：可选 `DROP` / 结构 DDL / 分批 `INSERT`。
    pub fn table(&mut self, table: &DumpTableInput) -> io::Result<TableDumpOutcome> {
        self.begin_table(table)?;
        let outcome = if self.options.include_data {
            if let Some(error) = &table.data_error {
                self.failure(&format!(
                    "表 {} 的数据读取失败：{error}",
                    table.path.display_label()
                ))?;
                TableDumpOutcome::default()
            } else {
                self.insert_rows(&table.path, &table.columns, &table.rows)?
            }
        } else {
            TableDumpOutcome::default()
        };
        self.end_table()?;
        Ok(outcome)
    }

    /// 表头：注释、可选 `DROP`、结构 DDL。数据可随后分页调用 [`Self::insert_rows`]。
    pub fn begin_table(&mut self, table: &DumpTableInput) -> io::Result<()> {
        self.summary.tables += 1;
        writeln!(self.out, "-- 表 {}", table.path.display_label())?;
        if let Some(error) = &table.ddl_error {
            self.failure(&format!(
                "表 {} 的结构获取失败：{error}",
                table.path.display_label()
            ))?;
        }
        if self.options.include_structure {
            if self.options.include_drops {
                writeln!(self.out, "{}", drop_table_sql(self.driver, &table.path))?;
            }
            match &table.ddl {
                Some(ddl) => {
                    let ddl = ddl.trim();
                    if ddl.is_empty() {
                        self.failure(&format!("表 {} 的结构为空", table.path.display_label()))?;
                    } else {
                        writeln!(
                            self.out,
                            "{ddl}{}",
                            if ddl.ends_with(';') { "" } else { ";" }
                        )?;
                    }
                }
                None if table.ddl_error.is_none() => {
                    self.failure(&format!(
                        "表 {} 的驱动不提供 DDL，已跳过结构",
                        table.path.display_label()
                    ))?;
                }
                None => {}
            }
        }
        Ok(())
    }

    /// 追加一批数据行（内部仍按方言批次上限拆分）。
    pub fn insert_rows(
        &mut self,
        path: &ObjectPath,
        columns: &[String],
        rows: &[Vec<Value>],
    ) -> io::Result<TableDumpOutcome> {
        if rows.is_empty() {
            return Ok(TableDumpOutcome::default());
        }
        if columns.is_empty() {
            self.failure(&format!(
                "表 {} 缺少列信息，已跳过数据",
                path.display_label()
            ))?;
            return Ok(TableDumpOutcome::default());
        }
        let table = DumpTableInput {
            path: path.clone(),
            ddl: None,
            ddl_error: None,
            columns: columns.to_vec(),
            rows: Vec::new(),
            data_error: None,
        };
        let outcome = self.inserts(&table, rows)?;
        self.summary.rows += outcome.rows;
        self.summary.failed_rows += outcome.failed_rows;
        Ok(outcome)
    }

    pub fn end_table(&mut self) -> io::Result<()> {
        writeln!(self.out)?;
        Ok(())
    }

    fn inserts(
        &mut self,
        table: &DumpTableInput,
        rows: &[Vec<Value>],
    ) -> io::Result<TableDumpOutcome> {
        let name = object_from_clause(self.driver, &table.path);
        let columns = table
            .columns
            .iter()
            .map(|column| ident(self.driver, column))
            .collect::<Vec<_>>()
            .join(", ");
        let limit = self.options.batch_limit(self.driver);
        let mut outcome = TableDumpOutcome::default();
        let mut batch: Vec<String> = Vec::with_capacity(limit);
        for (index, row) in rows.iter().enumerate() {
            match row
                .iter()
                .map(|value| literal(self.driver, value))
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(values) => batch.push(format!("({})", values.join(", "))),
                Err(error) => {
                    if self.options.error_mode == DumpErrorMode::Abort {
                        return Err(abort(&format!(
                            "表 {} 第 {} 行无法转为 SQL：{error}",
                            table.path.display_label(),
                            index + 1
                        )));
                    }
                    outcome.failed_rows += 1;
                    writeln!(
                        self.out,
                        "-- 跳过表 {} 第 {} 行：{error}",
                        table.path.display_label(),
                        index + 1
                    )?;
                }
            }
            if batch.len() == limit {
                self.flush_batch(&name, &columns, &mut batch)?;
                outcome.rows += limit as u64;
            }
        }
        if !batch.is_empty() {
            let rows = batch.len() as u64;
            self.flush_batch(&name, &columns, &mut batch)?;
            outcome.rows += rows;
        }
        Ok(outcome)
    }

    fn flush_batch(
        &mut self,
        name: &str,
        columns: &str,
        batch: &mut Vec<String>,
    ) -> io::Result<()> {
        writeln!(
            self.out,
            "INSERT INTO {name} ({columns}) VALUES {};",
            batch.join(", ")
        )?;
        batch.clear();
        Ok(())
    }

    /// 其它对象（视图 / 例程等）的 DDL，按调用顺序写出。
    pub fn object(&mut self, object: &DumpObjectInput) -> io::Result<()> {
        self.summary.objects += 1;
        writeln!(
            self.out,
            "-- {} {}",
            object.path.kind.label(),
            object.path.name
        )?;
        if let Some(error) = &object.error {
            self.failure(&format!(
                "{} {} 获取失败：{error}",
                object.path.kind.label(),
                object.path.display_label()
            ))?;
            writeln!(self.out)?;
            return Ok(());
        }
        match &object.ddl {
            Some(ddl) if !ddl.trim().is_empty() => {
                let ddl = ddl.trim();
                writeln!(
                    self.out,
                    "{ddl}{}",
                    if ddl.ends_with(';') { "" } else { ";" }
                )?;
            }
            _ => self.failure(&format!(
                "{} {} 的驱动不提供 DDL",
                object.path.kind.label(),
                object.path.display_label()
            ))?,
        }
        writeln!(self.out)?;
        Ok(())
    }

    pub fn footer(&mut self) -> io::Result<()> {
        writeln!(self.out, "-- 导出结束")?;
        Ok(())
    }

    /// 记录不阻断其它表的问题（如分页读取失败），策略与结构错误一致。
    pub fn note_failure(&mut self, message: &str) -> io::Result<()> {
        self.failure(message)
    }

    fn failure(&mut self, message: &str) -> io::Result<()> {
        self.summary.failed_tables += 1;
        if self.options.error_mode == DumpErrorMode::Abort {
            return Err(abort(message));
        }
        writeln!(self.out, "-- 失败：{message}")?;
        Ok(())
    }
}

fn abort(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, message.to_string())
}

/// 一次性渲染（测试 / 小库预览用）；大库走 [`DumpWriter`] 流式写。
pub fn render_script(
    input: &DumpInput,
    options: &DumpOptions,
) -> Result<(String, DumpSummary), String> {
    let mut writer = DumpWriter::new(Vec::new(), input.driver, options.clone());
    writer.summary.skipped = input.skipped;
    writer
        .header(&input.namespace, input.skipped)
        .map_err(|error| error.to_string())?;
    for table in &input.tables {
        writer.table(table).map_err(|error| error.to_string())?;
    }
    if options.include_objects {
        for object in &input.objects {
            writer.object(object).map_err(|error| error.to_string())?;
        }
    }
    writer.footer().map_err(|error| error.to_string())?;
    let summary = writer.summary();
    let bytes = writer.into_inner();
    String::from_utf8(bytes)
        .map(|text| (text, summary))
        .map_err(|error| error.to_string())
}

/// 视图 / 例程的导出顺序：表在前，视图其次，其余按类型与名称。
pub fn object_order(kind: ObjectKind) -> u8 {
    match kind {
        ObjectKind::Table => 0,
        ObjectKind::View | ObjectKind::MaterializedView => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ns() -> Namespace {
        Namespace::new(Some("main"), None::<&str>)
    }

    fn path(name: &str, kind: ObjectKind) -> ObjectPath {
        ObjectPath::new(ns(), name, kind)
    }

    fn table(name: &str, columns: &[&str], rows: Vec<Vec<Value>>) -> DumpTableInput {
        DumpTableInput {
            path: path(name, ObjectKind::Table),
            ddl: Some(format!("CREATE TABLE \"{name}\" (x INT)")),
            ddl_error: None,
            columns: columns.iter().map(|name| name.to_string()).collect(),
            rows,
            data_error: None,
        }
    }

    fn input(tables: Vec<DumpTableInput>) -> DumpInput {
        DumpInput {
            driver: DbKind::Sqlite,
            namespace: ns(),
            tables,
            objects: Vec::new(),
            skipped: 0,
        }
    }

    #[test]
    fn header_marks_non_snapshot_and_namespace() {
        let (script, _) = render_script(&input(Vec::new()), &DumpOptions::default()).unwrap();
        assert!(script.contains("-- 命名空间：main"), "{script}");
        assert!(script.contains("不是一致性快照"), "{script}");
        assert!(script.contains("结构 + 数据 + 其它对象"), "{script}");
    }

    #[test]
    fn structure_and_inserts_are_rendered() {
        let rows = vec![
            vec![Value::Int(1), Value::Text("a".into())],
            vec![Value::Int(2), Value::Null],
        ];
        let (script, summary) = render_script(
            &input(vec![table("users", &["id", "name"], rows)]),
            &DumpOptions::default(),
        )
        .unwrap();
        assert!(
            script.contains("CREATE TABLE \"users\" (x INT);"),
            "{script}"
        );
        assert!(
            script.contains("INSERT INTO \"users\" (\"id\", \"name\") VALUES (1, 'a'), (2, NULL);"),
            "{script}"
        );
        assert_eq!(summary.tables, 1);
        assert_eq!(summary.rows, 2);
        assert_eq!(summary.failed_rows, 0);
    }

    #[test]
    fn drop_precedes_create_and_can_be_disabled() {
        let options = DumpOptions {
            include_drops: true,
            ..DumpOptions::default()
        };
        let (script, _) =
            render_script(&input(vec![table("users", &["id"], Vec::new())]), &options).unwrap();
        let drop_at = script.find("DROP TABLE IF EXISTS").expect("缺少 DROP");
        let create_at = script.find("CREATE TABLE").expect("缺少 CREATE");
        assert!(drop_at < create_at, "{script}");
        assert!(
            script.contains("DROP TABLE IF EXISTS \"users\";"),
            "{script}"
        );

        let (plain, _) = render_script(
            &input(vec![table("users", &["id"], Vec::new())]),
            &DumpOptions::default(),
        )
        .unwrap();
        assert!(!plain.contains("DROP TABLE"), "{plain}");
    }

    #[test]
    fn inserts_are_split_by_batch_size() {
        let rows = (0..5).map(|n| vec![Value::Int(n)]).collect();
        let options = DumpOptions {
            batch_rows: 2,
            ..DumpOptions::default()
        };
        let (script, summary) =
            render_script(&input(vec![table("t", &["id"], rows)]), &options).unwrap();
        assert_eq!(script.matches("INSERT INTO").count(), 3, "{script}");
        assert_eq!(summary.rows, 5);
    }

    #[test]
    fn oracle_uses_single_row_inserts() {
        let rows = (0..3).map(|n| vec![Value::Int(n)]).collect();
        let mut source = input(vec![table("t", &["id"], rows)]);
        source.driver = DbKind::Oracle;
        let (script, _) = render_script(&source, &DumpOptions::default()).unwrap();
        assert_eq!(script.matches("INSERT INTO").count(), 3, "{script}");
        assert!(!script.contains("DROP TABLE"), "{script}");
    }

    #[test]
    fn oracle_drop_cascades_without_if_exists() {
        let sql = drop_table_sql(DbKind::Oracle, &path("t", ObjectKind::Table));
        assert_eq!(sql, "DROP TABLE \"main\".\"t\" CASCADE CONSTRAINTS;");
    }

    #[test]
    fn structure_and_data_switches_are_honored() {
        let options = DumpOptions {
            include_structure: false,
            include_data: true,
            ..DumpOptions::default()
        };
        let (script, _) = render_script(
            &input(vec![table("t", &["id"], vec![vec![Value::Int(1)]])]),
            &options,
        )
        .unwrap();
        assert!(!script.contains("CREATE TABLE"), "{script}");
        assert!(script.contains("INSERT INTO"), "{script}");
    }

    #[test]
    fn ddl_error_is_comment_in_comment_mode() {
        let mut source = input(vec![table("t", &["id"], vec![vec![Value::Int(1)]])]);
        source.tables[0].ddl = None;
        source.tables[0].ddl_error = Some("权限不足".into());
        let (script, summary) = render_script(&source, &DumpOptions::default()).unwrap();
        assert!(
            script.contains("-- 失败：表 main.t 的结构获取失败：权限不足"),
            "{script}"
        );
        assert_eq!(summary.failed_tables, 1);
        // 数据仍继续导出。
        assert!(script.contains("INSERT INTO"), "{script}");
    }

    #[test]
    fn abort_mode_stops_on_ddl_error() {
        let mut source = input(vec![table("t", &["id"], Vec::new())]);
        source.tables[0].ddl = None;
        source.tables[0].ddl_error = Some("权限不足".into());
        let options = DumpOptions {
            error_mode: DumpErrorMode::Abort,
            ..DumpOptions::default()
        };
        let error = render_script(&source, &options).unwrap_err();
        assert!(error.contains("权限不足"), "{error}");
    }

    #[test]
    fn non_finite_float_is_skipped_in_comment_mode() {
        let source = input(vec![table(
            "t",
            &["v"],
            vec![vec![Value::Float(f64::NAN)], vec![Value::Float(1.5)]],
        )]);
        let (script, summary) = render_script(&source, &DumpOptions::default()).unwrap();
        assert!(script.contains("-- 跳过表 main.t 第 1 行"), "{script}");
        assert!(script.contains("(1.5)"), "{script}");
        assert_eq!(summary.failed_rows, 1);
        assert_eq!(summary.rows, 1);
    }

    #[test]
    fn abort_mode_stops_on_literal_error() {
        let source = input(vec![table(
            "t",
            &["v"],
            vec![vec![Value::Float(f64::INFINITY)]],
        )]);
        let options = DumpOptions {
            error_mode: DumpErrorMode::Abort,
            ..DumpOptions::default()
        };
        let error = render_script(&source, &options).unwrap_err();
        assert!(error.contains("无法转为 SQL"), "{error}");
    }

    #[test]
    fn objects_follow_tables_and_include_ddl() {
        let mut source = input(vec![table("t", &["id"], Vec::new())]);
        source.objects = vec![DumpObjectInput {
            path: path("v_t", ObjectKind::View),
            ddl: Some("CREATE VIEW v_t AS SELECT * FROM t".into()),
            error: None,
        }];
        let (script, summary) = render_script(&source, &DumpOptions::default()).unwrap();
        let table_at = script.find("-- 表 main.t").unwrap();
        let view_at = script.find("-- view v_t").unwrap();
        assert!(table_at < view_at, "{script}");
        assert!(
            script.contains("CREATE VIEW v_t AS SELECT * FROM t;"),
            "{script}"
        );
        assert_eq!(summary.objects, 1);
    }

    #[test]
    fn objects_are_skipped_when_disabled() {
        let mut source = input(vec![table("t", &["id"], Vec::new())]);
        source.objects = vec![DumpObjectInput {
            path: path("v_t", ObjectKind::View),
            ddl: Some("CREATE VIEW v_t AS SELECT 1".into()),
            error: None,
        }];
        let options = DumpOptions {
            include_objects: false,
            ..DumpOptions::default()
        };
        let (script, summary) = render_script(&source, &options).unwrap();
        assert!(!script.contains("CREATE VIEW"), "{script}");
        assert_eq!(summary.objects, 0);
    }

    #[test]
    fn skipped_tables_are_reported_in_header() {
        let mut source = input(Vec::new());
        source.skipped = 7;
        let (script, summary) = render_script(&source, &DumpOptions::default()).unwrap();
        assert!(script.contains("有 7 张表因单次上限"), "{script}");
        assert_eq!(summary.skipped, 7);
    }

    #[test]
    fn qualified_names_follow_dialect() {
        let source = DumpInput {
            driver: DbKind::Mysql,
            namespace: ns(),
            tables: vec![table("users", &["id"], vec![vec![Value::Int(1)]])],
            objects: Vec::new(),
            skipped: 0,
        };
        let (script, _) = render_script(&source, &DumpOptions::default()).unwrap();
        assert!(
            script.contains("INSERT INTO `main`.`users` (`id`) VALUES (1);"),
            "{script}"
        );
    }

    #[test]
    fn batch_limit_is_clamped_per_dialect() {
        let options = DumpOptions {
            batch_rows: 5000,
            ..DumpOptions::default()
        };
        assert_eq!(options.batch_limit(DbKind::Postgres), 1000);
        assert_eq!(options.batch_limit(DbKind::Mssql), 1000);
        assert_eq!(options.batch_limit(DbKind::Oracle), 1);
        assert_eq!(
            DumpOptions {
                batch_rows: 0,
                ..DumpOptions::default()
            }
            .batch_limit(DbKind::Sqlite),
            1
        );
    }
}
