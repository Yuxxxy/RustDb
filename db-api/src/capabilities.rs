use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpsertStyle {
    None,
    OnConflict,
    OnDuplicateKey,
    Merge,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelStyle {
    None,
    KillQuery,
    CancelToken,
    Interrupt,
    Attention,
    OciBreak,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainStyle {
    None,
    Explain,
    ExplainAnalyze,
    ExplainPlan,
    ShowplanXml,
    QueryPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonStyle {
    None,
    Native,
    TextJson,
    JsonFunctions,
}

/// 某驱动/某版本「能做什么」的唯一事实来源。
///
/// UI 只查这里的字段，禁止 `match DbKind`；版本相关能力在连接建立后修正。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub multi_statement: bool,
    pub returning: bool,
    pub cte: bool,
    pub window_functions: bool,
    pub upsert: UpsertStyle,
    pub cancel: CancelStyle,
    pub explain: ExplainStyle,
    pub ddl_transactional: bool,
    pub schemas: bool,
    pub multiple_catalogs: bool,
    pub arrays: bool,
    pub json: JsonStyle,
    pub enum_types: bool,
    pub sequences: bool,
    pub materialized_views: bool,
    pub case_sensitive_idents: bool,
    pub max_ident_len: usize,
    pub max_params: Option<usize>,
    pub read_only_txn: bool,
    /// 驱动是否真流式返回（false = 结果先物化再交回）。
    pub streaming_rows: bool,
    pub notices: bool,

    // --- v1.1 增补能力位（需求文档 19.8.1）---
    // 缺失（旧 JSON / 旧驱动）一律按 `false` 处理：UI 隐藏入口而不是假装支持（R5）。
    /// 方言能否把 DML 保守改写为只读预览（F-63）。
    #[serde(default)]
    pub dml_preview: bool,
    /// 例程（视图 / 过程 / 函数 / 触发器）源码可读且可保存（F-82）。
    #[serde(default)]
    pub routine_source_edit: bool,
    /// 可查询例程依赖关系（F-83）。
    #[serde(default)]
    pub routine_dependencies: bool,
    /// 可管理分区（F-85）。
    #[serde(default)]
    pub partition_admin: bool,
    /// 表数据的 WHERE / ORDER BY 可下推到服务端（F-70）。
    #[serde(default)]
    pub server_side_filter: bool,
    /// 外键元数据可用于跳转（F-73）。
    #[serde(default)]
    pub foreign_key_pivot: bool,
    /// 执行计划可结构化展示（F-19 增强）。
    #[serde(default)]
    pub explain_visual: bool,
}

impl Capabilities {
    /// 保守基线：所有可选能力关闭。具体驱动在此基础上打开。
    pub fn minimal() -> Self {
        Self {
            multi_statement: false,
            returning: false,
            cte: false,
            window_functions: false,
            upsert: UpsertStyle::None,
            cancel: CancelStyle::None,
            explain: ExplainStyle::None,
            ddl_transactional: false,
            schemas: false,
            multiple_catalogs: false,
            arrays: false,
            json: JsonStyle::None,
            enum_types: false,
            sequences: false,
            materialized_views: false,
            case_sensitive_idents: true,
            max_ident_len: 64,
            max_params: None,
            read_only_txn: false,
            streaming_rows: false,
            notices: false,
            dml_preview: false,
            routine_source_edit: false,
            routine_dependencies: false,
            partition_admin: false,
            server_side_filter: false,
            foreign_key_pivot: false,
            explain_visual: false,
        }
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::minimal()
    }
}
