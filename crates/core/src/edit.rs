//! 待提交变更集（F-35~F-38）：三态记录 + 方言化 DML 生成。
//!
//! 这里是**唯一**允许把 `Value` 渲染成 SQL 字面量的地方，UI 不得自行拼 SQL
//! （与「UI 零 `match DbKind`」同一条约束）。纯逻辑、无 IO，全部可单测。

use rustdb_db_api::{dialect, DbKind, ObjectPath, Value};

/// 变更三态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Insert,
    Update,
    Delete,
}

impl ChangeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Insert => "新增",
            Self::Update => "修改",
            Self::Delete => "删除",
        }
    }
}

/// 行级变更：原值快照用于乐观并发的 `WHERE`。
#[derive(Debug, Clone, PartialEq)]
pub struct RowChange {
    pub kind: ChangeKind,
    /// 结果集里的行下标（UI 高亮与撤销用）。
    pub row: usize,
    /// 原值快照；`Insert` 为空。
    pub old: Vec<Value>,
    /// 目标值（全列，未改动的列与原值相同）。
    pub new: Vec<Value>,
    /// 实际改动的列下标；`Insert` 为全列。
    pub dirty: Vec<usize>,
    /// 显式交给数据库默认值的列（INSERT 省略、UPDATE 用 `DEFAULT`）。
    pub defaults: Vec<usize>,
}

/// 一条待执行语句 + 它对应的行（提交后按影响行数判定冲突）。
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedStatement {
    pub sql: String,
    pub row: usize,
    pub kind: ChangeKind,
    /// `true` 表示这条语句必须影响 ≥1 行，否则判定「已被他人修改」。
    pub expects_row: bool,
}

#[derive(Debug, Clone)]
pub struct ChangeSet {
    path: ObjectPath,
    columns: Vec<String>,
    /// 定位列（主键）下标；空 = 无主键。
    key: Vec<usize>,
    changes: Vec<RowChange>,
}

impl ChangeSet {
    pub fn new(path: ObjectPath, columns: Vec<String>, key: Vec<usize>) -> Self {
        Self {
            path,
            columns,
            key,
            changes: Vec::new(),
        }
    }

    pub fn path(&self) -> &ObjectPath {
        &self.path
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    pub fn key(&self) -> &[usize] {
        &self.key
    }

    /// 能否安全定位行。无主键表只允许插入。
    pub fn can_locate(&self) -> bool {
        !self.key.is_empty()
    }

    pub fn len(&self) -> usize {
        self.changes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn changes(&self) -> &[RowChange] {
        &self.changes
    }

    /// 某行的状态（同行只保留一条变更；新增行再改仍是新增）。
    pub fn row_state(&self, row: usize) -> Option<ChangeKind> {
        self.changes.iter().find(|c| c.row == row).map(|c| c.kind)
    }

    /// 记录一次单元格修改：同行的多条修改合并为一条 `Update`。
    ///
    /// * 无主键 → 拒绝（R5：给显式原因，不静默降级）。
    /// * 同行已有 `Insert` → 直接改插入值，不产生 `Update`。
    /// * 同行已有 `Delete` → 拒绝（语义冲突）。
    pub fn record_cell(
        &mut self,
        row: usize,
        column: usize,
        old: Vec<Value>,
        new_value: Value,
    ) -> Result<(), String> {
        if column >= self.columns.len() {
            return Err(format!("列下标 {column} 越界"));
        }
        if let Some(existing) = self.changes.iter_mut().find(|c| c.row == row) {
            match existing.kind {
                ChangeKind::Insert => {
                    existing.new[column] = new_value;
                    existing.defaults.retain(|&index| index != column);
                    if !existing.dirty.contains(&column) {
                        existing.dirty.push(column);
                    }
                    return Ok(());
                }
                ChangeKind::Delete => return Err("该行已标记删除".into()),
                ChangeKind::Update => {
                    existing.new[column] = new_value.clone();
                    existing.defaults.retain(|&index| index != column);
                    if !existing.dirty.contains(&column) {
                        existing.dirty.push(column);
                    }
                    // 改回原值即撤销这一列的修改；全部改回则整行撤销。
                    let unchanged = existing
                        .dirty
                        .iter()
                        .all(|&i| existing.new[i] == existing.old[i]);
                    if unchanged {
                        self.changes.retain(|c| c.row != row);
                    }
                    return Ok(());
                }
            }
        }
        if !self.can_locate() {
            return Err("该表没有主键，无法安全定位行，禁止修改".into());
        }
        let mut new = old.clone();
        new[column] = new_value;
        self.changes.push(RowChange {
            kind: ChangeKind::Update,
            row,
            old,
            new,
            dirty: vec![column],
            defaults: Vec::new(),
        });
        Ok(())
    }

    /// 标记删除。
    ///
    /// 新增行还没进库，删除它等于撤销这条新增（不能生成 `DELETE`）。
    pub fn record_delete(&mut self, row: usize, old: Vec<Value>) -> Result<(), String> {
        if !self.can_locate() {
            return Err("该表没有主键，无法安全定位行，禁止删除".into());
        }
        if self
            .changes
            .iter()
            .any(|c| c.row == row && c.kind == ChangeKind::Insert)
        {
            self.revert(row);
            return Ok(());
        }
        self.changes.retain(|c| c.row != row);
        self.changes.push(RowChange {
            kind: ChangeKind::Delete,
            row,
            old,
            new: Vec::new(),
            dirty: Vec::new(),
            defaults: Vec::new(),
        });
        Ok(())
    }

    /// 新增一行（`values` 长度必须等于列数）。
    pub fn record_insert(&mut self, row: usize, values: Vec<Value>) -> Result<(), String> {
        if values.len() != self.columns.len() {
            return Err("新增行的列数与结果集不一致".into());
        }
        let dirty = (0..self.columns.len()).collect();
        self.changes.retain(|c| c.row != row);
        self.changes.push(RowChange {
            kind: ChangeKind::Insert,
            row,
            old: Vec::new(),
            new: values,
            dirty,
            defaults: Vec::new(),
        });
        Ok(())
    }

    /// 克隆一行：`exclude_key` 为真时把主键列交给数据库默认值。
    ///
    /// 主键是自增 / 序列时这是唯一可靠的做法（显式写 NULL 不会触发默认值）。
    pub fn record_clone(
        &mut self,
        row: usize,
        old: Vec<Value>,
        exclude_key: bool,
    ) -> Result<(), String> {
        if old.len() != self.columns.len() {
            return Err("克隆行的列数与结果集不一致".into());
        }
        let mut values = old;
        let mut defaults = Vec::new();
        if exclude_key {
            for &index in &self.key {
                if let Some(value) = values.get_mut(index) {
                    *value = Value::Null;
                    defaults.push(index);
                }
            }
        }
        let dirty = (0..self.columns.len()).collect();
        self.changes.retain(|c| c.row != row);
        self.changes.push(RowChange {
            kind: ChangeKind::Insert,
            row,
            old: Vec::new(),
            new: values,
            dirty,
            defaults,
        });
        Ok(())
    }

    /// 把某列设为数据库默认值。
    ///
    /// * 新增行：提交时省略该列（`DEFAULT` 才会生效）。
    /// * 修改行：`SET 列 = DEFAULT`，需要主键定位。
    /// * 删除行：拒绝（语义冲突）。
    pub fn record_set_default(
        &mut self,
        row: usize,
        column: usize,
        old: Vec<Value>,
    ) -> Result<(), String> {
        if column >= self.columns.len() {
            return Err(format!("列下标 {column} 越界"));
        }
        if let Some(existing) = self.changes.iter_mut().find(|c| c.row == row) {
            match existing.kind {
                ChangeKind::Insert | ChangeKind::Update => {
                    existing.new[column] = Value::Null;
                    if !existing.defaults.contains(&column) {
                        existing.defaults.push(column);
                    }
                    if !existing.dirty.contains(&column) {
                        existing.dirty.push(column);
                    }
                    return Ok(());
                }
                ChangeKind::Delete => return Err("该行已标记删除".into()),
            }
        }
        if !self.can_locate() {
            return Err("该表没有主键，无法安全定位行，禁止修改".into());
        }
        let mut new = old.clone();
        new[column] = Value::Null;
        self.changes.push(RowChange {
            kind: ChangeKind::Update,
            row,
            old,
            new,
            dirty: vec![column],
            defaults: vec![column],
        });
        Ok(())
    }

    /// 行级撤销。
    ///
    /// 撤销的是新增行时，后面的行下标要前移（新增行总是追加在末尾）。
    pub fn revert(&mut self, row: usize) -> bool {
        let before = self.changes.len();
        let removed_insert = self
            .changes
            .iter()
            .find(|c| c.row == row)
            .map(|c| c.kind == ChangeKind::Insert)
            .unwrap_or(false);
        self.changes.retain(|c| c.row != row);
        if removed_insert {
            for change in self.changes.iter_mut() {
                if change.row > row {
                    change.row -= 1;
                }
            }
        }
        before != self.changes.len()
    }

    pub fn clear(&mut self) {
        self.changes.clear();
    }

    /// 生成待执行语句（顺序 = 记录顺序；调用方据此判定冲突）。
    ///
    /// 有主键时 `WHERE` 用主键原值；无主键只允许 `Insert`，
    /// 因此这里不会产出无法定位的 `Update` / `Delete`。
    pub fn plan(&self, kind: DbKind) -> Result<Vec<PlannedStatement>, String> {
        let mut out = Vec::with_capacity(self.changes.len());
        let table = crate::core::object_from_clause(kind, &self.path);
        for change in &self.changes {
            let sql = match change.kind {
                ChangeKind::Insert => {
                    let include: Vec<usize> = (0..self.columns.len())
                        .filter(|index| !change.defaults.contains(index))
                        .collect();
                    let cols: Vec<String> = include
                        .iter()
                        .map(|&index| ident(kind, &self.columns[index]))
                        .collect();
                    let values: Vec<String> = include
                        .iter()
                        .map(|&index| literal(kind, change.new.get(index).unwrap_or(&Value::Null)))
                        .collect::<Result<_, _>>()?;
                    if include.is_empty() {
                        format!("INSERT INTO {table} DEFAULT VALUES;")
                    } else {
                        format!(
                            "INSERT INTO {table} ({}) VALUES ({});",
                            cols.join(", "),
                            values.join(", ")
                        )
                    }
                }
                ChangeKind::Update => {
                    let assignments: Vec<String> = change
                        .dirty
                        .iter()
                        .map(|&i| {
                            if change.defaults.contains(&i) {
                                return Ok(format!("{} = DEFAULT", ident(kind, &self.columns[i])));
                            }
                            let value = change.new.get(i).unwrap_or(&Value::Null);
                            Ok(format!(
                                "{} = {}",
                                ident(kind, &self.columns[i]),
                                literal(kind, value)?
                            ))
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    format!(
                        "UPDATE {table} SET {} WHERE {};",
                        assignments.join(", "),
                        self.where_clause(kind, &change.old)?
                    )
                }
                ChangeKind::Delete => format!(
                    "DELETE FROM {table} WHERE {};",
                    self.where_clause(kind, &change.old)?
                ),
            };
            out.push(PlannedStatement {
                sql,
                row: change.row,
                kind: change.kind,
                // 新增语句没法用影响行数判冲突（多数驱动给 1，但语义不同）。
                expects_row: change.kind != ChangeKind::Insert,
            });
        }
        Ok(out)
    }

    /// 主键等值定位；NULL 用 `IS NULL`（等号对 NULL 永远不成立）。
    fn where_clause(&self, kind: DbKind, old: &[Value]) -> Result<String, String> {
        if self.key.is_empty() {
            return Err("无主键无法定位行".into());
        }
        let mut parts = Vec::with_capacity(self.key.len());
        for &index in &self.key {
            let name = self
                .columns
                .get(index)
                .ok_or_else(|| format!("定位列下标 {index} 越界"))?;
            let value = old.get(index).unwrap_or(&Value::Null);
            parts.push(if value.is_null() {
                format!("{} IS NULL", ident(kind, name))
            } else {
                format!("{} = {}", ident(kind, name), literal(kind, value)?)
            });
        }
        Ok(parts.join(" AND "))
    }

    /// 预览用：全部语句拼成一段可复制的 SQL。
    pub fn preview_text(&self, kind: DbKind) -> Result<String, String> {
        let plan = self.plan(kind)?;
        Ok(plan
            .iter()
            .map(|s| s.sql.as_str())
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// 解析 TSV 粘贴内容（支持 Excel / 电子表格的引号字段与内嵌换行）。
///
/// 行内字段数可以不一致，由调用方按目标列裁剪；空输入返回空表。
pub fn parse_tsv(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut field_started = false;
    let mut dirty = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                other => field.push(other),
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() && !field_started => {
                in_quotes = true;
                field_started = true;
                dirty = true;
            }
            '\t' => {
                row.push(std::mem::take(&mut field));
                field_started = false;
                dirty = true;
            }
            '\r' | '\n' => {
                if ch == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                field_started = false;
                dirty = false;
            }
            other => {
                field.push(other);
                field_started = true;
                dirty = true;
            }
        }
    }
    if in_quotes {
        return Err("TSV 引号未闭合".into());
    }
    if dirty || !row.is_empty() || !field.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

/// 标识符引用（按方言）。
pub fn ident(kind: DbKind, name: &str) -> String {
    match kind {
        DbKind::Mysql | DbKind::Mariadb => dialect::quote_backtick(name),
        DbKind::Mssql => dialect::quote_bracket(name),
        _ => dialect::quote_double(name),
    }
}

/// `Value` → SQL 字面量。
///
/// 注意：这是**字符串拼接**（不是参数绑定），只用于变更集的批量提交与预览；
/// 参数化查询走 `ExecuteRequest.params`（F-30）。
pub fn literal(kind: DbKind, value: &Value) -> Result<String, String> {
    Ok(match value {
        Value::Null => "NULL".into(),
        Value::Bool(v) => match kind {
            // SQL Server / SQLite 没有布尔字面量。
            DbKind::Mssql | DbKind::Sqlite => {
                if *v {
                    "1".into()
                } else {
                    "0".into()
                }
            }
            _ => {
                if *v {
                    "TRUE".into()
                } else {
                    "FALSE".into()
                }
            }
        },
        Value::Int(v) => v.to_string(),
        Value::UInt(v) => v.to_string(),
        Value::Float(v) => {
            if v.is_finite() {
                format!("{v}")
            } else {
                return Err("浮点值不是有限数".into());
            }
        }
        // 定点数按原文拼，绝不经过 f64。
        Value::Decimal(v) => v.to_string(),
        Value::Text(v) => quote_text(kind, v),
        Value::Json(v) => quote_text(kind, v),
        Value::Bytes(bytes) => match kind {
            DbKind::Mssql => format!("0x{}", hex(bytes, "")),
            DbKind::Mysql | DbKind::Mariadb | DbKind::Sqlite => {
                format!("X'{}'", hex(bytes, ""))
            }
            DbKind::Oracle => format!("HEXTORAW('{}')", hex(bytes, "")),
            // PostgreSQL bytea 接受 `'\x...'` 文本形式。
            DbKind::Postgres => format!("'\\x{}'", hex(bytes, "")),
        },
        Value::Date(v) => quote_text(kind, &v.format("%Y-%m-%d").to_string()),
        Value::Time(v) => quote_text(kind, &v.format("%H:%M:%S%.6f").to_string()),
        Value::Timestamp(v) => quote_text(kind, &v.format("%Y-%m-%d %H:%M:%S%.6f").to_string()),
        Value::TimestampTz(v) => quote_text(kind, &v.to_rfc3339()),
        Value::Uuid(bytes) => quote_text(kind, &uuid_text(bytes)),
        Value::Array(_) => return Err("数组类型暂不支持写入变更集".into()),
        Value::Omitted { .. } => return Err("大字段未完整加载，不能写入".into()),
        Value::Unknown { text, .. } => quote_text(kind, text),
    })
}

fn hex(bytes: &[u8], sep: &str) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            out.push_str(sep);
        }
        out.push_str(&format!("{byte:02X}"));
    }
    out
}

pub(crate) fn uuid_text(bytes: &[u8; 16]) -> String {
    let h = hex(bytes, "");
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// 字符串字面量：单引号成对转义；MySQL 额外转义反斜杠（其默认模式把 `\` 当转义符）。
fn quote_text(kind: DbKind, text: &str) -> String {
    let escape_backslash = matches!(kind, DbKind::Mysql | DbKind::Mariadb);
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\'' => out.push_str("''"),
            '\\' if escape_backslash => out.push_str("\\\\"),
            '\0' => out.push_str("\\0"),
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::{Namespace, ObjectKind};

    fn path() -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("shop"), Some("public")),
            "orders",
            ObjectKind::Table,
        )
    }

    fn change_set(kind: DbKind) -> ChangeSet {
        let _ = kind;
        ChangeSet::new(
            path(),
            vec!["id".into(), "name".into(), "note".into()],
            vec![0],
        )
    }

    fn row(id: i64, name: &str, note: Option<&str>) -> Vec<Value> {
        vec![
            Value::Int(id),
            Value::Text(name.into()),
            note.map(|s| Value::Text(s.into())).unwrap_or(Value::Null),
        ]
    }

    #[test]
    fn update_touches_only_dirty_columns() {
        let mut set = change_set(DbKind::Postgres);
        set.record_cell(0, 1, row(1, "a", None), Value::Text("b".into()))
            .unwrap();
        let plan = set.plan(DbKind::Postgres).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(
            plan[0].sql.contains("SET \"name\" = 'b'"),
            "{}",
            plan[0].sql
        );
        assert!(!plan[0].sql.contains("note"), "未改动列不应进 SET");
        assert!(
            plan[0].sql.contains("WHERE \"id\" = 1"),
            "WHERE 应用主键原值：{}",
            plan[0].sql
        );
        assert!(plan[0].expects_row);
    }

    #[test]
    fn reverting_last_cell_change_drops_the_row() {
        let mut set = change_set(DbKind::Postgres);
        set.record_cell(0, 1, row(1, "a", None), Value::Text("b".into()))
            .unwrap();
        assert_eq!(set.len(), 1);
        set.record_cell(0, 1, row(1, "b", None), Value::Text("a".into()))
            .unwrap();
        assert!(set.is_empty(), "改回原值应撤销整行变更");
    }

    #[test]
    fn delete_requires_primary_key() {
        let mut set = ChangeSet::new(path(), vec!["a".into()], Vec::new());
        let err = set.record_delete(0, vec![Value::Int(1)]).unwrap_err();
        assert!(err.contains("主键"), "{err}");
        let err = set
            .record_cell(0, 0, vec![Value::Int(1)], Value::Int(2))
            .unwrap_err();
        assert!(err.contains("主键"), "{err}");
        // 无主键仍可插入。
        assert!(set.record_insert(9, vec![Value::Int(2)]).is_ok());
    }

    #[test]
    fn null_uses_is_null_in_where() {
        let mut set = ChangeSet::new(path(), vec!["code".into(), "v".into()], vec![0, 1]);
        set.record_delete(0, vec![Value::Int(1), Value::Null])
            .unwrap();
        let plan = set.plan(DbKind::Postgres).unwrap();
        assert!(plan[0].sql.contains("\"v\" IS NULL"), "{}", plan[0].sql);
        assert!(!plan[0].sql.contains("= NULL"), "{}", plan[0].sql);
    }

    #[test]
    fn literals_follow_dialect() {
        // 单引号成对转义
        assert_eq!(
            literal(DbKind::Postgres, &Value::Text("a'b".into())).unwrap(),
            "'a''b'"
        );
        // MySQL 额外转义反斜杠
        assert_eq!(
            literal(DbKind::Mysql, &Value::Text("a\\b".into())).unwrap(),
            "'a\\\\b'"
        );
        // 布尔：SQL Server / SQLite 用 1/0
        assert_eq!(literal(DbKind::Mssql, &Value::Bool(true)).unwrap(), "1");
        assert_eq!(
            literal(DbKind::Postgres, &Value::Bool(true)).unwrap(),
            "TRUE"
        );
        // 标识符
        assert_eq!(ident(DbKind::Mysql, "a`b"), "`a``b`");
        assert_eq!(ident(DbKind::Mssql, "a]b"), "[a]]b]");
        // 二进制：SQLite 必须用 X'..'，否则会存成文本；Oracle 用 HEXTORAW。
        assert_eq!(
            literal(DbKind::Mssql, &Value::Bytes(vec![0x0a, 0xff].into())).unwrap(),
            "0x0AFF"
        );
        assert_eq!(
            literal(DbKind::Sqlite, &Value::Bytes(vec![0x0a, 0xff].into())).unwrap(),
            "X'0AFF'"
        );
        assert_eq!(
            literal(DbKind::Oracle, &Value::Bytes(vec![0x0a, 0xff].into())).unwrap(),
            "HEXTORAW('0AFF')"
        );
    }

    #[test]
    fn insert_lists_all_columns_in_order() {
        let mut set = change_set(DbKind::Sqlite);
        set.record_insert(7, vec![Value::Int(9), Value::Text("x".into()), Value::Null])
            .unwrap();
        let plan = set.plan(DbKind::Sqlite).unwrap();
        assert_eq!(
            plan[0].sql,
            "INSERT INTO \"public\".\"orders\" (\"id\", \"name\", \"note\") VALUES (9, 'x', NULL);"
        );
        assert!(!plan[0].expects_row, "新增不靠影响行数判冲突");
    }

    #[test]
    fn plan_keeps_record_order_and_counts() {
        let mut set = change_set(DbKind::Postgres);
        set.record_delete(1, row(2, "b", None)).unwrap();
        set.record_cell(0, 2, row(1, "a", None), Value::Text("n".into()))
            .unwrap();
        let plan = set.plan(DbKind::Postgres).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].kind, ChangeKind::Delete);
        assert_eq!(plan[1].kind, ChangeKind::Update);
        assert_eq!(plan[0].row, 1);
        assert_eq!(plan[1].row, 0);
    }

    #[test]
    fn clone_can_leave_primary_key_to_database() {
        let mut set = change_set(DbKind::Postgres);
        set.record_clone(3, row(1, "a", None), true).unwrap();
        let plan = set.plan(DbKind::Postgres).unwrap();
        assert!(
            plan[0].sql.contains("(\"name\", \"note\")"),
            "应省略主键列：{}",
            plan[0].sql
        );
        assert!(!plan[0].sql.contains("\"id\""), "{}", plan[0].sql);

        let mut keep = change_set(DbKind::Postgres);
        keep.record_clone(3, row(1, "a", None), false).unwrap();
        let plan = keep.plan(DbKind::Postgres).unwrap();
        assert!(plan[0].sql.contains("\"id\""), "{}", plan[0].sql);
    }

    #[test]
    fn default_value_omitted_on_insert_and_used_on_update() {
        let mut set = change_set(DbKind::Postgres);
        set.record_clone(3, row(1, "a", None), true).unwrap();
        // 再显式填主键，defaults 标记应被撤销。
        set.record_cell(3, 0, row(1, "a", None), Value::Int(9))
            .unwrap();
        let plan = set.plan(DbKind::Postgres).unwrap();
        assert!(plan[0].sql.contains("\"id\""), "{}", plan[0].sql);
        assert!(!plan[0].sql.contains("DEFAULT"), "{}", plan[0].sql);

        let mut update = change_set(DbKind::Postgres);
        update
            .record_set_default(0, 2, row(1, "a", Some("x")))
            .unwrap();
        let plan = update.plan(DbKind::Postgres).unwrap();
        assert!(
            plan[0].sql.contains("\"note\" = DEFAULT"),
            "{}",
            plan[0].sql
        );
        assert!(plan[0].sql.contains("WHERE \"id\" = 1"), "{}", plan[0].sql);
    }

    #[test]
    fn insert_with_all_defaults_uses_default_values() {
        let mut set = ChangeSet::new(path(), vec!["id".into()], vec![0]);
        set.record_clone(0, vec![Value::Int(1)], true).unwrap();
        let plan = set.plan(DbKind::Sqlite).unwrap();
        assert!(
            plan[0].sql.ends_with("\"orders\" DEFAULT VALUES;"),
            "{}",
            plan[0].sql
        );
    }

    #[test]
    fn tsv_parser_handles_quotes_tabs_and_newlines() {
        let rows = parse_tsv("a\tb\nc\td\n").unwrap();
        assert_eq!(rows, vec![vec!["a", "b"], vec!["c", "d"]]);

        let quoted = parse_tsv("\"a\tb\"\t\"line1\nline2\"\r\n\"x\"\"y\"\tz").unwrap();
        assert_eq!(
            quoted,
            vec![
                vec!["a\tb".to_string(), "line1\nline2".to_string()],
                vec!["x\"y".to_string(), "z".to_string()]
            ]
        );

        assert_eq!(parse_tsv("a\t\n").unwrap(), vec![vec!["a", ""]]);
        assert!(parse_tsv("\"unterminated").is_err());
        assert!(parse_tsv("").unwrap().is_empty());
    }

    #[test]
    fn deleting_an_inserted_row_cancels_the_insert() {
        let mut set = change_set(DbKind::Postgres);
        set.record_insert(2, row(9, "x", None)).unwrap();
        // 新增行还没进库，删它等于撤销这条新增，不能生成 DELETE。
        set.record_delete(2, row(9, "x", None)).unwrap();
        assert!(set.is_empty(), "新增行被删后不应留下变更");
        assert!(set.plan(DbKind::Postgres).unwrap().is_empty());
    }

    #[test]
    fn reverting_an_inserted_row_shifts_following_rows() {
        let mut set = change_set(DbKind::Postgres);
        set.record_insert(2, row(9, "x", None)).unwrap();
        set.record_insert(3, row(10, "y", None)).unwrap();
        assert!(set.revert(2));
        assert_eq!(set.len(), 1);
        // 被撤销的新增行之后的行下标要前移。
        assert_eq!(set.changes()[0].row, 2);
        let plan = set.plan(DbKind::Postgres).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(plan[0].sql.contains("'y'"), "{}", plan[0].sql);
    }
}
