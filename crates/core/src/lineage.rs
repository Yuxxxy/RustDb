//! F-88 字段血缘（只读聚合，证据分级）。
//!
//! 证据来源与置信度：
//! - 外键：`确定`，引用列指向被引用列；
//! - 视图定义：`高`，只解析 `SELECT` 列表里「源列 → 输出列」的显式映射；
//! - 历史 SQL：`高`（写入语句）/ `可能`（普通查询），只识别限定列名；
//! - 同名字段：`可能`，只作为「可能的来源」，不推测下游。
//!
//! 约束（需求 19.4）：只读聚合，不写数据库；证据分级不把推测标成事实；
//! 解析失败、截断与缺失都要显式警告，不静默丢弃。

use rustdb_db_api::{ForeignKeyInfo, ObjectPath};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// 单次聚合最多参与的表 / 视图 / 历史语句数。
pub const MAX_LINEAGE_TABLES: usize = 300;
pub const MAX_LINEAGE_VIEWS: usize = 300;
pub const MAX_LINEAGE_QUERIES: usize = 100;
/// 每个方向最多返回的边数。
pub const MAX_LINEAGE_EDGES: usize = 200;
/// 同名字段最多补充的候选数。
pub const MAX_SAME_NAME: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// 可能：同名字段、无法完全判定的历史 SQL。
    Possible,
    /// 高：视图定义、写入类历史 SQL。
    High,
    /// 确定：外键。
    Certain,
}

impl Confidence {
    pub const ALL: [Confidence; 3] = [Confidence::Certain, Confidence::High, Confidence::Possible];

    pub fn label(self) -> &'static str {
        match self {
            Self::Certain => "确定",
            Self::High => "高",
            Self::Possible => "可能",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    ForeignKey,
    ViewDefinition,
    QueryHistory,
    SameName,
}

impl EvidenceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ForeignKey => "外键",
            Self::ViewDefinition => "视图定义",
            Self::QueryHistory => "历史 SQL",
            Self::SameName => "同名字段",
        }
    }
}

/// 一张表在血缘输入里的形态：列名与外键足够，不拉索引 / 检查约束。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineageTable {
    pub path: ObjectPath,
    pub columns: Vec<String>,
    #[serde(default)]
    pub foreign_keys: Vec<ForeignKeyInfo>,
}

/// 一个视图及其定义文本；定义取不到时 `definition` 为空。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineageView {
    pub path: ObjectPath,
    pub definition: String,
}

/// 一条历史 SQL；`label` 用于证据说明（通常是时间或序号）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LineageQuery {
    pub sql: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LineageInput {
    pub database: String,
    #[serde(default)]
    pub tables: Vec<LineageTable>,
    #[serde(default)]
    pub views: Vec<LineageView>,
    #[serde(default)]
    pub queries: Vec<LineageQuery>,
    pub skipped_tables: usize,
    pub skipped_views: usize,
    pub skipped_queries: usize,
    #[serde(default)]
    pub failed_tables: usize,
    #[serde(default)]
    pub failed_views: usize,
}

/// 一条血缘边：`from` 是上游，`to` 是下游。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageEdge {
    pub from: ObjectPath,
    pub from_column: String,
    pub to: ObjectPath,
    pub to_column: String,
    pub confidence: Confidence,
    pub kind: EvidenceKind,
    pub evidence: String,
}

impl LineageEdge {
    pub fn from_label(&self) -> String {
        format!("{}.{}", self.from.display_label(), self.from_column)
    }

    pub fn to_label(&self) -> String {
        format!("{}.{}", self.to.display_label(), self.to_column)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineageReport {
    pub table: Option<ObjectPath>,
    pub column: String,
    pub upstream: Vec<LineageEdge>,
    pub downstream: Vec<LineageEdge>,
    pub warnings: Vec<String>,
}

/// 计算某一列的上游 / 下游边。只读纯函数。
pub fn report(input: &LineageInput, table: &ObjectPath, column: &str) -> LineageReport {
    let mut report = LineageReport {
        table: Some(table.clone()),
        column: column.to_string(),
        ..LineageReport::default()
    };
    let column = column.trim();
    if column.is_empty() {
        report.warnings.push("未选择字段".into());
        return report;
    }
    let Some(target) = input
        .tables
        .iter()
        .find(|item| same_path(&item.path, table))
    else {
        report
            .warnings
            .push(format!("{} 不在本次血缘快照里", table.display_label()));
        return report;
    };
    if !target
        .columns
        .iter()
        .any(|item| item.eq_ignore_ascii_case(column))
    {
        report
            .warnings
            .push(format!("{} 上没有字段 {column}", table.display_label()));
        return report;
    }

    let index = TableIndex::new(&input.tables);
    let mut edges: BTreeMap<EdgeKey, LineageEdge> = BTreeMap::new();
    let mut unresolved_fk = 0usize;
    let mut unparsed_views = 0usize;

    for item in &input.tables {
        for fk in &item.foreign_keys {
            let Some((target_path, target_columns)) = index.resolve_fk(&item.path, fk) else {
                unresolved_fk += 1;
                continue;
            };
            for (position, column_name) in fk.columns.iter().enumerate() {
                let target_column = target_columns
                    .get(position)
                    .cloned()
                    .unwrap_or_else(|| column_name.clone());
                push_edge(
                    &mut edges,
                    LineageEdge {
                        from: target_path.clone(),
                        from_column: target_column,
                        to: item.path.clone(),
                        to_column: column_name.clone(),
                        confidence: Confidence::Certain,
                        kind: EvidenceKind::ForeignKey,
                        evidence: format!("外键 {}", fk.name),
                    },
                );
            }
        }
    }

    for view in &input.views {
        match parse_view(view, &index, input) {
            ViewParse::Edges(found) => {
                for (source, source_column, output, evidence) in found {
                    push_edge(
                        &mut edges,
                        LineageEdge {
                            from: source,
                            from_column: source_column,
                            to: view.path.clone(),
                            to_column: output,
                            confidence: Confidence::High,
                            kind: EvidenceKind::ViewDefinition,
                            evidence,
                        },
                    );
                }
            }
            ViewParse::Unparsed => {
                if mentions_table(&view.definition, table) {
                    unparsed_views += 1;
                }
            }
        }
    }

    for (position, query) in input.queries.iter().enumerate() {
        if query.sql.trim().is_empty() {
            continue;
        }
        if let QueryParse::Edges(found) = parse_query(query, position, &index, table, column) {
            for edge in found {
                push_edge(&mut edges, edge);
            }
        }
    }

    if input.skipped_tables > 0 {
        report.warnings.push(format!(
            "已跳过 {} 张表（单次上限 {MAX_LINEAGE_TABLES}）",
            input.skipped_tables
        ));
    }
    if input.skipped_views > 0 {
        report.warnings.push(format!(
            "已跳过 {} 个视图（单次上限 {MAX_LINEAGE_VIEWS}）",
            input.skipped_views
        ));
    }
    if input.skipped_queries > 0 {
        report.warnings.push(format!(
            "已跳过 {} 条历史 SQL（单次上限 {MAX_LINEAGE_QUERIES}）",
            input.skipped_queries
        ));
    }
    if input.failed_tables > 0 {
        report.warnings.push(format!(
            "{} 张表的列 / 外键元数据取回失败",
            input.failed_tables
        ));
    }
    if input.failed_views > 0 {
        report
            .warnings
            .push(format!("{} 个视图定义取回失败", input.failed_views));
    }
    if unresolved_fk > 0 {
        report.warnings.push(format!(
            "{unresolved_fk} 条外键指向的表不在本次快照里，已忽略"
        ));
    }
    if unparsed_views > 0 {
        report.warnings.push(format!(
            "{unparsed_views} 个视图定义未解析出显式字段映射（如 SELECT * 或复杂表达式）"
        ));
    }
    if input.queries.is_empty() {
        report
            .warnings
            .push("本次快照没有历史 SQL，血缘只来自外键与视图定义".into());
    }

    let mut upstream = Vec::new();
    let mut downstream = Vec::new();
    let mut same_name = 0usize;
    for edge in edges.into_values() {
        if same_path(&edge.from, table)
            && edge.from_column.eq_ignore_ascii_case(column)
            && same_path(&edge.to, table)
            && edge.to_column.eq_ignore_ascii_case(column)
        {
            continue;
        }
        if same_path(&edge.to, table) && edge.to_column.eq_ignore_ascii_case(column) {
            upstream.push(edge);
        } else if same_path(&edge.from, table) && edge.from_column.eq_ignore_ascii_case(column) {
            downstream.push(edge);
        }
    }

    // 同名字段只作为「可能的来源」补充，不推测下游。
    for item in &input.tables {
        if same_path(&item.path, table) {
            continue;
        }
        let Some(found) = item
            .columns
            .iter()
            .find(|name| name.eq_ignore_ascii_case(column))
        else {
            continue;
        };
        if upstream.iter().any(|edge| {
            same_path(&edge.from, &item.path) && edge.from_column.eq_ignore_ascii_case(found)
        }) {
            continue;
        }
        if same_name >= MAX_SAME_NAME {
            report
                .warnings
                .push(format!("同名字段候选超过 {MAX_SAME_NAME} 个，其余已截断"));
            break;
        }
        same_name += 1;
        upstream.push(LineageEdge {
            from: item.path.clone(),
            from_column: found.clone(),
            to: table.clone(),
            to_column: column.to_string(),
            confidence: Confidence::Possible,
            kind: EvidenceKind::SameName,
            evidence: "同名字段（仅名称一致，需人工确认）".into(),
        });
    }

    sort_edges(&mut upstream);
    sort_edges(&mut downstream);
    if upstream.len() > MAX_LINEAGE_EDGES {
        upstream.truncate(MAX_LINEAGE_EDGES);
        report
            .warnings
            .push(format!("上游超过 {MAX_LINEAGE_EDGES} 条，已截断"));
    }
    if downstream.len() > MAX_LINEAGE_EDGES {
        downstream.truncate(MAX_LINEAGE_EDGES);
        report
            .warnings
            .push(format!("下游超过 {MAX_LINEAGE_EDGES} 条，已截断"));
    }
    report.upstream = upstream;
    report.downstream = downstream;
    report
}

/// 按最低置信度与关键字过滤（UI 的过滤条共用）。关键字匹配表名、列名与证据说明。
pub fn filter_edges(edges: &[LineageEdge], min: Confidence, needle: &str) -> Vec<LineageEdge> {
    let needle = needle.trim().to_lowercase();
    edges
        .iter()
        .filter(|edge| edge.confidence >= min)
        .filter(|edge| {
            needle.is_empty()
                || edge.from_label().to_lowercase().contains(&needle)
                || edge.to_label().to_lowercase().contains(&needle)
                || edge.evidence.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect()
}

fn sort_edges(edges: &mut [LineageEdge]) {
    edges.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| {
                a.from_label()
                    .to_lowercase()
                    .cmp(&b.from_label().to_lowercase())
            })
            .then_with(|| {
                a.to_label()
                    .to_lowercase()
                    .cmp(&b.to_label().to_lowercase())
            })
            .then_with(|| a.evidence.cmp(&b.evidence))
    });
}

fn same_path(a: &ObjectPath, b: &ObjectPath) -> bool {
    a.name.eq_ignore_ascii_case(&b.name)
        && a.ns
            .display_label()
            .eq_ignore_ascii_case(&b.ns.display_label())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EdgeKey {
    from: String,
    from_column: String,
    to: String,
    to_column: String,
}

fn push_edge(map: &mut BTreeMap<EdgeKey, LineageEdge>, edge: LineageEdge) {
    let key = EdgeKey {
        from: edge.from.display_label().to_lowercase(),
        from_column: edge.from_column.to_lowercase(),
        to: edge.to.display_label().to_lowercase(),
        to_column: edge.to_column.to_lowercase(),
    };
    match map.entry(key) {
        std::collections::btree_map::Entry::Vacant(slot) => {
            slot.insert(edge);
        }
        std::collections::btree_map::Entry::Occupied(mut slot) => {
            let old = slot.get_mut();
            if edge.confidence > old.confidence {
                *old = edge;
            } else if edge.confidence == old.confidence
                && !old.evidence.contains(edge.evidence.as_str())
            {
                old.evidence = format!("{}；{}", old.evidence, edge.evidence);
            }
        }
    }
}

/// 表名索引：同名表跨 schema 时不做猜测。
struct TableIndex<'a> {
    tables: &'a [LineageTable],
    by_name: BTreeMap<String, Vec<usize>>,
}

impl<'a> TableIndex<'a> {
    fn new(tables: &'a [LineageTable]) -> Self {
        let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, table) in tables.iter().enumerate() {
            by_name
                .entry(table.path.name.to_lowercase())
                .or_default()
                .push(index);
        }
        Self { tables, by_name }
    }

    fn table(&self, name: &str) -> Option<&'a LineageTable> {
        let name = unquote_segments(name).pop()?;
        let found = self.by_name.get(&name.to_lowercase())?;
        if found.len() == 1 {
            self.tables.get(found[0])
        } else {
            None
        }
    }

    fn resolve_fk(
        &self,
        owner: &ObjectPath,
        fk: &ForeignKeyInfo,
    ) -> Option<(ObjectPath, Vec<String>)> {
        let raw = unquote_segments(&fk.referenced_table);
        let (target, columns) = if raw.len() >= 2 {
            let table = self.table(raw.last()?)?;
            (table.path.clone(), fk.referenced_columns.clone())
        } else {
            // 未限定时先在同命名空间找，找不到再按唯一表名找。
            let name = raw.first()?.to_lowercase();
            let same_ns = self
                .tables
                .iter()
                .find(|item| item.path.ns == owner.ns && item.path.name.to_lowercase() == name);
            match same_ns {
                Some(table) => (table.path.clone(), fk.referenced_columns.clone()),
                None => {
                    let table = self.table(&name)?;
                    (table.path.clone(), fk.referenced_columns.clone())
                }
            }
        };
        Some((target, columns))
    }
}

enum ViewParse {
    Edges(Vec<(ObjectPath, String, String, String)>),
    Unparsed,
}

fn parse_view(view: &LineageView, index: &TableIndex<'_>, _input: &LineageInput) -> ViewParse {
    if view.definition.trim().is_empty() {
        return ViewParse::Unparsed;
    }
    let words = tokenize(&view.definition);
    let aliases = alias_map(&view.definition, &words);
    let Some(items) = select_items(&view.definition, &words) else {
        return ViewParse::Unparsed;
    };
    let mut sources: BTreeSet<String> = aliases.values().cloned().collect();
    sources.extend(source_names(&view.definition, &words));
    let mut found = Vec::new();
    let mut saw_star = false;
    for item in &items {
        if item.trim() == "*" || item.trim().ends_with(".*") {
            saw_star = true;
            continue;
        }
        let Some(output) = output_name(item) else {
            continue;
        };
        let item_words = tokenize(item);
        let refs = qualified_refs(item, &item_words, &aliases);
        if let Some((table_name, column, _)) = refs.first() {
            let Some(source) = index.table(table_name) else {
                continue;
            };
            if !source
                .columns
                .iter()
                .any(|name| name.eq_ignore_ascii_case(column))
            {
                continue;
            }
            found.push((
                source.path.clone(),
                column.clone(),
                output,
                format!("视图 {} 定义", view.path.name),
            ));
            continue;
        }
        // 未限定表名的单列：只有唯一来源表时才接受。
        let bare = item_words
            .iter()
            .find(|word| !is_reserved(&word.lower) && !word.text.contains('('));
        if let Some(bare) = bare {
            if sources.len() == 1 {
                if let Some(source) = sources.iter().next().and_then(|name| index.table(name)) {
                    if source
                        .columns
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(&bare.text))
                    {
                        found.push((
                            source.path.clone(),
                            bare.text.clone(),
                            output,
                            format!("视图 {} 定义（未限定表名）", view.path.name),
                        ));
                    }
                }
            }
        }
    }
    if found.is_empty() && (saw_star || !items.is_empty()) {
        ViewParse::Unparsed
    } else {
        ViewParse::Edges(found)
    }
}

fn source_names(sql: &str, words: &[Word]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (index, word) in words.iter().enumerate() {
        if !matches!(word.lower.as_str(), "from" | "join") {
            continue;
        }
        let Some(table) = words.get(index + 1) else {
            continue;
        };
        if table.lower == "only" {
            continue;
        }
        let mut name = table.lower.clone();
        if let Some(next) = words.get(index + 2) {
            if is_dot_between(sql, table, next) {
                name = next.lower.clone();
            }
        }
        names.insert(name);
    }
    names
}

fn mentions_table(text: &str, table: &ObjectPath) -> bool {
    text.to_lowercase().contains(&table.name.to_lowercase())
}

enum QueryParse {
    Edges(Vec<LineageEdge>),
    Irrelevant,
}

fn parse_query(
    query: &LineageQuery,
    position: usize,
    index: &TableIndex<'_>,
    target: &ObjectPath,
    target_column: &str,
) -> QueryParse {
    let sql = query.sql.as_str();
    let words = tokenize(sql);
    let aliases = alias_map(sql, &words);
    let insert = insert_edges(
        query,
        position,
        index,
        aliases.clone(),
        target,
        target_column,
    );
    if !insert.is_empty() {
        return QueryParse::Edges(insert);
    }
    let refs = qualified_refs(sql, &words, &aliases);
    let hits_target = refs.iter().any(|(table_name, column, _)| {
        index
            .table(table_name)
            .is_some_and(|item| same_path(&item.path, target))
            && column.eq_ignore_ascii_case(target_column)
    });
    if !hits_target {
        return QueryParse::Irrelevant;
    }

    let label = if query.label.trim().is_empty() {
        format!("#{}", position + 1)
    } else {
        query.label.clone()
    };

    let mutation = mutation_target(sql, &words)
        .and_then(|name| index.table(&name).map(|item| item.path.clone()));
    let downstream_target = mutation
        .as_ref()
        .is_some_and(|path| same_path(path, target));

    let confidence = if downstream_target {
        Confidence::High
    } else {
        Confidence::Possible
    };
    let evidence = format!("历史 SQL {label}：{}", truncate_sql(query.sql.trim(), 60));

    let mut found: BTreeMap<(String, String), (ObjectPath, String)> = BTreeMap::new();
    for (table_name, column, _) in &refs {
        let Some(source) = index.table(table_name) else {
            continue;
        };
        if same_path(&source.path, target) {
            continue;
        }
        if !source
            .columns
            .iter()
            .any(|name| name.eq_ignore_ascii_case(column))
        {
            continue;
        }
        found.insert(
            (
                source.path.display_label().to_lowercase(),
                column.to_lowercase(),
            ),
            (source.path.clone(), column.clone()),
        );
    }
    if found.is_empty() {
        return QueryParse::Irrelevant;
    }
    let mut edges = Vec::new();
    for (_, (path, column)) in found {
        edges.push(if downstream_target {
            LineageEdge {
                from: path,
                from_column: column,
                to: target.clone(),
                to_column: target_column.to_string(),
                confidence,
                kind: EvidenceKind::QueryHistory,
                evidence: evidence.clone(),
            }
        } else {
            LineageEdge {
                from: target.clone(),
                from_column: target_column.to_string(),
                to: path,
                to_column: column,
                confidence,
                kind: EvidenceKind::QueryHistory,
                evidence: evidence.clone(),
            }
        });
    }
    QueryParse::Edges(edges)
}

/// `INSERT INTO 目标 (列…) SELECT … FROM 源` 的按位置映射；不是这种形态时返回空。
fn insert_edges(
    query: &LineageQuery,
    position: usize,
    index: &TableIndex<'_>,
    aliases: BTreeMap<String, String>,
    target: &ObjectPath,
    target_column: &str,
) -> Vec<LineageEdge> {
    let sql = query.sql.as_str();
    let words = tokenize(sql);
    let Some(insert) = words.iter().position(|word| word.lower == "insert") else {
        return Vec::new();
    };
    let Some(into) = words
        .iter()
        .enumerate()
        .skip(insert + 1)
        .take(4)
        .find(|(_, word)| word.lower == "into")
        .map(|(index, _)| index)
    else {
        return Vec::new();
    };
    let Some(table_word) = words.get(into + 1) else {
        return Vec::new();
    };
    let table_name = read_qualified(sql, &words, into + 1, table_word);
    let Some(insert_table) = index.table(&table_name) else {
        return Vec::new();
    };
    let depth = table_word.depth;
    let mut target_columns = Vec::new();
    for word in words.iter().skip(into + 2) {
        if word.depth == depth + 1 {
            target_columns.push(word.text.clone());
        } else if !target_columns.is_empty() {
            break;
        }
    }
    if target_columns.is_empty() {
        return Vec::new();
    }
    let Some(items) = select_items(sql, &words) else {
        return Vec::new();
    };
    let sources = source_names(sql, &words);
    if sources.len() != 1 {
        return Vec::new();
    }
    let Some(source) = index.table(sources.iter().next().expect("已确认非空")) else {
        return Vec::new();
    };
    let label = if query.label.trim().is_empty() {
        format!("#{}", position + 1)
    } else {
        query.label.clone()
    };
    let evidence = format!("历史 SQL {label}：{}", truncate_sql(query.sql.trim(), 60));
    let mut edges = Vec::new();
    for (position_in_list, target_col) in target_columns.iter().enumerate() {
        let Some(item) = items.get(position_in_list) else {
            break;
        };
        if item.contains('(') {
            continue;
        }
        let item_words = tokenize(item);
        let refs = qualified_refs(item, &item_words, &aliases);
        let source_column = match refs.first() {
            Some((table_name, column, _)) => match index.table(table_name) {
                Some(found) if same_path(&found.path, &source.path) => column.clone(),
                _ => continue,
            },
            None => match item_words.iter().find(|word| !is_reserved(&word.lower)) {
                Some(word) => word.text.clone(),
                None => continue,
            },
        };
        if !source
            .columns
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&source_column))
        {
            continue;
        }
        let source_hit =
            same_path(&source.path, target) && source_column.eq_ignore_ascii_case(target_column);
        let target_hit =
            same_path(&insert_table.path, target) && target_col.eq_ignore_ascii_case(target_column);
        if source_hit || target_hit {
            edges.push(LineageEdge {
                from: source.path.clone(),
                from_column: source_column,
                to: insert_table.path.clone(),
                to_column: target_col.clone(),
                confidence: Confidence::High,
                kind: EvidenceKind::QueryHistory,
                evidence: evidence.clone(),
            });
        }
    }
    edges
}

fn mutation_target(sql: &str, words: &[Word]) -> Option<String> {
    for (index, word) in words.iter().enumerate() {
        match word.lower.as_str() {
            "insert" => {
                if let Some(into) = words
                    .iter()
                    .skip(index + 1)
                    .take(3)
                    .position(|item| item.lower == "into")
                {
                    if let Some(name) = words.get(index + 1 + into + 1) {
                        return Some(read_qualified(sql, words, index + 1 + into + 1, name));
                    }
                }
            }
            "update" => {
                if let Some(name) = words.get(index + 1) {
                    return Some(read_qualified(sql, words, index + 1, name));
                }
            }
            "delete" => {
                if let Some(from) = words
                    .iter()
                    .skip(index + 1)
                    .take(3)
                    .position(|item| item.lower == "from")
                {
                    if let Some(name) = words.get(index + 1 + from + 1) {
                        return Some(read_qualified(sql, words, index + 1 + from + 1, name));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn read_qualified(sql: &str, words: &[Word], index: usize, word: &Word) -> String {
    let mut name = word.text.clone();
    if let Some(next) = words.get(index + 1) {
        if is_dot_between(sql, word, next) {
            name = next.text.clone();
        }
    }
    name
}

fn select_items(sql: &str, words: &[Word]) -> Option<Vec<String>> {
    let select = words.iter().position(|word| word.lower == "select")?;
    let depth = words[select].depth;
    let from = words
        .iter()
        .enumerate()
        .skip(select + 1)
        .find(|(_, word)| word.lower == "from" && word.depth == depth)
        .map(|(index, _)| index)?;
    let start = words[select].end;
    let end = words[from].start;
    Some(split_top_level(&sql[start..end]))
}

fn split_top_level(text: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (index, ch) in text.char_indices() {
        match quote {
            Some(close) => {
                if ch == close {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' | '`' => quote = Some(ch),
                '[' => quote = Some(']'),
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    let item = text[start..index].trim();
                    if !item.is_empty() {
                        items.push(item.to_string());
                    }
                    start = index + 1;
                }
                _ => {}
            },
        }
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        items.push(tail.to_string());
    }
    items
}

fn output_name(item: &str) -> Option<String> {
    let words = tokenize(item);
    if words.is_empty() {
        return None;
    }
    if let Some(as_index) = words.iter().rposition(|word| word.lower == "as") {
        return words.get(as_index + 1).map(|word| word.text.clone());
    }
    let last = words.last()?;
    if is_reserved(&last.lower) {
        return None;
    }
    if item.contains('(') && words.len() <= 2 {
        return None;
    }
    Some(last.text.clone())
}

fn truncate_sql(sql: &str, limit: usize) -> String {
    let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        flat
    } else {
        format!("{}…", flat.chars().take(limit).collect::<String>())
    }
}

fn unquote_segments(text: &str) -> Vec<String> {
    text.split('.')
        .map(|part| {
            part.trim()
                .trim_matches('"')
                .trim_matches('`')
                .trim_matches('[')
                .trim_matches(']')
                .to_string()
        })
        .filter(|part| !part.is_empty())
        .collect()
}

fn is_dot_between(sql: &str, a: &Word, b: &Word) -> bool {
    a.end <= b.start && sql[a.end..b.start].trim() == "."
}

#[derive(Debug, Clone)]
struct Word {
    text: String,
    lower: String,
    start: usize,
    end: usize,
    depth: i32,
}

fn tokenize(sql: &str) -> Vec<Word> {
    let chars: Vec<(usize, char)> = sql.char_indices().collect();
    let mut words = Vec::new();
    let mut index = 0;
    let mut depth = 0i32;
    while index < chars.len() {
        let (start, ch) = chars[index];
        if ch == '\'' {
            index += 1;
            while index < chars.len() && chars[index].1 != '\'' {
                index += 1;
            }
            index += 1;
            continue;
        }
        if ch == '"' || ch == '`' || ch == '[' {
            let close = if ch == '[' { ']' } else { ch };
            let mut cursor = index + 1;
            let mut text = String::new();
            while cursor < chars.len() && chars[cursor].1 != close {
                text.push(chars[cursor].1);
                cursor += 1;
            }
            if !text.is_empty() {
                let end = chars
                    .get(cursor)
                    .map(|(position, value)| position + value.len_utf8())
                    .unwrap_or(sql.len());
                words.push(Word {
                    lower: text.to_lowercase(),
                    text,
                    start,
                    end,
                    depth,
                });
            }
            index = cursor + 1;
            continue;
        }
        if ch == '(' {
            depth += 1;
            index += 1;
            continue;
        }
        if ch == ')' {
            depth -= 1;
            index += 1;
            continue;
        }
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            let mut cursor = index;
            let mut text = String::new();
            while cursor < chars.len()
                && (chars[cursor].1.is_alphanumeric()
                    || chars[cursor].1 == '_'
                    || chars[cursor].1 == '$')
            {
                text.push(chars[cursor].1);
                cursor += 1;
            }
            let end = chars
                .get(cursor)
                .map(|(position, _)| *position)
                .unwrap_or(sql.len());
            words.push(Word {
                lower: text.to_lowercase(),
                text,
                start,
                end,
                depth,
            });
            index = cursor;
            continue;
        }
        index += 1;
    }
    words
}

fn alias_map(sql: &str, words: &[Word]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (index, word) in words.iter().enumerate() {
        if !matches!(word.lower.as_str(), "from" | "join" | "update" | "into") {
            continue;
        }
        let Some(table_word) = words.get(index + 1) else {
            continue;
        };
        if table_word.lower == "only" {
            continue;
        }
        if sql[table_word.start..].trim_start().starts_with('(') {
            continue;
        }
        let mut table = table_word.lower.clone();
        let mut cursor = index + 2;
        if let Some(next) = words.get(cursor) {
            if is_dot_between(sql, table_word, next) {
                table = next.lower.clone();
                cursor += 1;
            }
        }
        let Some(candidate) = words.get(cursor) else {
            continue;
        };
        if sql[candidate.start..].trim_start().starts_with('(') {
            continue;
        }
        if candidate.lower == "as" {
            if let Some(alias) = words.get(cursor + 1) {
                map.insert(alias.lower.clone(), table);
            }
            continue;
        }
        if !is_reserved(&candidate.lower) {
            map.insert(candidate.lower.clone(), table);
        }
    }
    map
}

fn qualified_refs(
    sql: &str,
    words: &[Word],
    aliases: &BTreeMap<String, String>,
) -> Vec<(String, String, usize)> {
    let mut refs = Vec::new();
    let mut index = 0;
    while index < words.len() {
        if index + 2 < words.len()
            && is_dot_between(sql, &words[index], &words[index + 1])
            && is_dot_between(sql, &words[index + 1], &words[index + 2])
        {
            refs.push((
                words[index + 1].lower.clone(),
                words[index + 2].text.clone(),
                index + 2,
            ));
            index += 3;
            continue;
        }
        if index + 1 < words.len() && is_dot_between(sql, &words[index], &words[index + 1]) {
            let prefix = words[index].lower.clone();
            let table = aliases.get(&prefix).cloned().unwrap_or(prefix);
            refs.push((table, words[index + 1].text.clone(), index + 1));
            index += 2;
            continue;
        }
        index += 1;
    }
    refs
}

fn is_reserved(word: &str) -> bool {
    matches!(
        word,
        "as" | "on"
            | "using"
            | "where"
            | "group"
            | "order"
            | "limit"
            | "offset"
            | "union"
            | "except"
            | "intersect"
            | "join"
            | "left"
            | "right"
            | "inner"
            | "outer"
            | "full"
            | "cross"
            | "natural"
            | "select"
            | "from"
            | "set"
            | "values"
            | "returning"
            | "into"
            | "update"
            | "insert"
            | "delete"
            | "and"
            | "or"
            | "not"
            | "null"
            | "is"
            | "in"
            | "case"
            | "when"
            | "then"
            | "else"
            | "end"
            | "distinct"
            | "all"
            | "asc"
            | "desc"
            | "over"
            | "partition"
            | "by"
            | "with"
            | "create"
            | "view"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_db_api::{Namespace, ObjectKind};

    fn path(name: &str) -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("app"), Some("public")),
            name,
            ObjectKind::Table,
        )
    }

    fn view_path(name: &str) -> ObjectPath {
        ObjectPath::new(
            Namespace::new(Some("app"), Some("public")),
            name,
            ObjectKind::View,
        )
    }

    fn table(name: &str, columns: &[&str]) -> LineageTable {
        LineageTable {
            path: path(name),
            columns: columns.iter().map(|item| item.to_string()).collect(),
            foreign_keys: Vec::new(),
        }
    }

    fn fk(
        owner: &mut LineageTable,
        name: &str,
        columns: &[&str],
        target: &str,
        target_columns: &[&str],
    ) {
        owner.foreign_keys.push(ForeignKeyInfo {
            name: name.into(),
            columns: columns.iter().map(|item| item.to_string()).collect(),
            referenced_table: target.into(),
            referenced_columns: target_columns.iter().map(|item| item.to_string()).collect(),
            on_delete: None,
            on_update: None,
        });
    }

    fn input(tables: Vec<LineageTable>) -> LineageInput {
        LineageInput {
            database: "app.public".into(),
            tables,
            views: Vec::new(),
            queries: Vec::new(),
            skipped_tables: 0,
            skipped_views: 0,
            skipped_queries: 0,
            failed_tables: 0,
            failed_views: 0,
        }
    }

    #[test]
    fn foreign_keys_are_certain_in_both_directions() {
        let users = table("users", &["id", "name"]);
        let mut orders = table("orders", &["id", "user_id"]);
        fk(
            &mut orders,
            "fk_orders_user",
            &["user_id"],
            "users",
            &["id"],
        );
        let input = input(vec![users, orders]);

        let up = report(&input, &path("orders"), "user_id");
        assert_eq!(up.upstream.len(), 1, "{:?}", up);
        assert_eq!(up.upstream[0].from.name.as_ref(), "users");
        assert_eq!(up.upstream[0].from_column, "id");
        assert_eq!(up.upstream[0].confidence, Confidence::Certain);
        assert_eq!(up.upstream[0].kind, EvidenceKind::ForeignKey);

        let down = report(&input, &path("users"), "id");
        assert_eq!(down.downstream.len(), 1);
        assert_eq!(down.downstream[0].to.name.as_ref(), "orders");
        assert_eq!(down.downstream[0].to_column, "user_id");
        assert!(down
            .warnings
            .iter()
            .any(|item| item.contains("没有历史 SQL")));
    }

    #[test]
    fn view_alias_maps_source_to_output() {
        let mut tables = vec![
            table("users", &["id", "name"]),
            table("orders", &["id", "user_id"]),
        ];
        fk(
            &mut tables[1],
            "fk_orders_user",
            &["user_id"],
            "users",
            &["id"],
        );
        let mut input = input(tables);
        input.views.push(LineageView {
            path: view_path("v_orders"),
            definition: "CREATE VIEW v_orders AS SELECT o.id AS order_id, u.name AS user_name FROM orders o JOIN users u ON u.id = o.user_id".into(),
        });

        let found = report(&input, &path("users"), "name");
        assert_eq!(found.downstream.len(), 1, "{:?}", found);
        assert_eq!(found.downstream[0].to.name.as_ref(), "v_orders");
        assert_eq!(found.downstream[0].to_column, "user_name");
        assert_eq!(found.downstream[0].confidence, Confidence::High);

        let up = report(&input, &view_path("v_orders"), "user_name");
        assert!(up
            .warnings
            .iter()
            .any(|item| item.contains("不在本次血缘快照里")));
    }

    #[test]
    fn view_with_star_is_reported_as_unparsed() {
        let mut input = input(vec![table("users", &["id"])]);
        input.views.push(LineageView {
            path: view_path("v_users"),
            definition: "CREATE VIEW v_users AS SELECT * FROM users".into(),
        });
        let found = report(&input, &path("users"), "id");
        assert!(found
            .warnings
            .iter()
            .any(|item| item.contains("未解析出显式字段映射")));
    }

    #[test]
    fn history_insert_select_is_high_and_directional() {
        let mut input = input(vec![
            table("stage", &["id", "user_id"]),
            table("orders", &["id", "user_id"]),
        ]);
        input.queries.push(LineageQuery {
            sql: "INSERT INTO orders (id, user_id) SELECT id, user_id FROM stage".into(),
            label: "10:01".into(),
        });
        let found = report(&input, &path("orders"), "user_id");
        assert_eq!(found.upstream.len(), 1, "{:?}", found);
        assert_eq!(found.upstream[0].from.name.as_ref(), "stage");
        assert_eq!(found.upstream[0].confidence, Confidence::High);
        assert!(found.upstream[0].evidence.contains("10:01"));
    }

    #[test]
    fn history_select_marks_target_as_upstream() {
        let mut input = input(vec![
            table("users", &["id", "name"]),
            table("report", &["user_id"]),
        ]);
        input.queries.push(LineageQuery {
            sql: "SELECT u.name, r.user_id FROM users u JOIN report r ON r.user_id = u.id".into(),
            label: String::new(),
        });
        let found = report(&input, &path("users"), "name");
        assert_eq!(found.downstream.len(), 1, "{:?}", found);
        assert_eq!(found.downstream[0].to.name.as_ref(), "report");
        assert_eq!(found.downstream[0].confidence, Confidence::Possible);
    }

    #[test]
    fn same_name_candidates_are_possible_and_capped() {
        let mut tables = vec![table("target", &["code"])];
        for index in 0..(MAX_SAME_NAME + 3) {
            tables.push(table(&format!("copy_{index}"), &["code"]));
        }
        let found = report(&input(tables), &path("target"), "code");
        assert_eq!(found.upstream.len(), MAX_SAME_NAME);
        assert!(found
            .upstream
            .iter()
            .any(|edge| edge.kind == EvidenceKind::SameName));
        assert!(found
            .warnings
            .iter()
            .any(|item| item.contains("同名字段候选超过")));
    }

    #[test]
    fn duplicate_evidence_keeps_highest_confidence() {
        let users = table("users", &["id"]);
        let mut orders = table("orders", &["id", "user_id"]);
        fk(
            &mut orders,
            "fk_orders_user",
            &["user_id"],
            "users",
            &["id"],
        );
        let mut input = input(vec![users, orders]);
        input.queries.push(LineageQuery {
            sql: "UPDATE orders SET user_id = users.id FROM users".into(),
            label: "x".into(),
        });
        let found = report(&input, &path("orders"), "user_id");
        let edge = found
            .upstream
            .iter()
            .find(|edge| edge.from.name.as_ref() == "users")
            .expect("应有外键边");
        assert_eq!(edge.confidence, Confidence::Certain, "{:?}", edge);
    }

    #[test]
    fn missing_target_column_warns() {
        let found = report(
            &input(vec![table("users", &["id"])]),
            &path("users"),
            "nope",
        );
        assert!(found.upstream.is_empty());
        assert!(found
            .warnings
            .iter()
            .any(|item| item.contains("没有字段 nope")));
    }

    #[test]
    fn filter_applies_confidence_and_needle() {
        let edge = |confidence, from: &str, column: &str, evidence: &str| LineageEdge {
            from: path(from),
            from_column: column.into(),
            to: path("target"),
            to_column: "id".into(),
            confidence,
            kind: EvidenceKind::SameName,
            evidence: evidence.into(),
        };
        let edges = vec![
            edge(Confidence::Certain, "a", "id", "外键 fk_a"),
            edge(Confidence::Possible, "b", "id", "同名字段"),
        ];
        let certain = filter_edges(&edges, Confidence::Certain, "");
        assert_eq!(certain.len(), 1);
        let needle = filter_edges(&edges, Confidence::Possible, "fk_a");
        assert_eq!(needle.len(), 1);
        assert_eq!(needle[0].from.name.as_ref(), "a");
    }

    #[test]
    fn unresolved_foreign_key_warns() {
        let mut orders = table("orders", &["user_id"]);
        fk(&mut orders, "fk_missing", &["user_id"], "ghost", &["id"]);
        let found = report(&input(vec![orders]), &path("orders"), "user_id");
        assert!(found
            .warnings
            .iter()
            .any(|item| item.contains("1 条外键指向的表不在本次快照里")));
    }
}
