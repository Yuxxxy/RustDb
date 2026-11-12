//! 主视图：标题栏 + 活动栏 + 侧栏卡片 + SQL 工作区 + AI 卡片 + 状态栏。
//!
//! 布局参数与 RustShell 对齐：标题栏 28px、活动栏 38.4px、卡片间距 6px、圆角 8px。

use crate::theme::{current, to_hsla as hex};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    anchored, div, px, uniform_list, App, AppContext as _, ClipboardItem, Context, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Point,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Editor, EditorState, Input, InputEvent};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::Disableable as _;
use gpui_component::RopeExt as _;
use gpui_component::{h_flex, v_flex, Icon, Sizable as _, Size, TitleBar};
use gpui_kit_assets::IconName;
use rustdb_core::docs::DocInput;
use rustdb_core::history::{HistoryFilter, HistorySource};
use rustdb_core::import::{self, ImportOptions, ImportPlan, ParsedData};
use rustdb_core::results::ResultStore;
use rustdb_core::theme::ThemeMode;
use rustdb_core::{
    group_by_folder, AppEvent, ConnEvent, ConnectionProfile, DataCompareConfig, DataCompareEvent,
    DataCompareOptions, DataCompareRequest, DumpEvent, DumpOptions, EventRecvError, EventStream,
    ExportFormat, ExportOptions, MetaEvent, OptimizeKind, OptimizeRequest, OptimizeResult,
    QueryEvent, ResultSetId, TableCompareConfig, TransferEvent, TransferMode, TransferOptions,
    TxEvent,
};
use rustdb_db_api::{
    compare_values, Capabilities, ColumnInfo, ColumnMeta, DbKind, ForeignKeyInfo, Namespace,
    ObjectKind, ObjectPath, ObjectSummary, PageOrder, PageRequest, PageResult, QueryId, RowBatch,
    SessionId, Value, DEFAULT_PAGE_SIZE,
};
use rustdb_sql_lang::{ParamKind, ParamValue, PlaceholderStyle};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// AI 会话、上下文与 SQL 代码块。
mod ai_chat;
/// AI 侧栏（对话气泡 / 工具调用 / 上下文芯片）。
mod ai_column;
/// AI 模型面板（F-94）。
mod ai_panel;
/// 批量更新面板。
mod bulk;
/// 单元格详情与列筛选菜单。
mod cell_detail;
/// 单元格编辑与行操作。
mod cell_edit;
/// 结构对比面板（F-86）。
mod compare_panel;
/// 连接编辑器（F-02）。
mod connections;
/// 数据对比面板（F-78）。
mod data_compare_panel;
/// 表设计器（列 / 索引 / 外键 / 检查 / 触发器）。
mod designer;
/// 数据库文档面板（F-87）。
mod docs_panel;
/// 整库导出面板（F-76）。
mod dump_panel;
/// 结果集编辑 / 导出取数（从本文件拆出，便于单测与独立演进）。
mod editing;
/// 事件分派（AppEvent → UI 状态）。
mod events;
/// 连接配置加密交换面板（F-90）。
mod exchange_panel;
/// 执行链路（取 SQL / 参数 / 提交 / DML 预览）。
mod execute;
/// 行生成器与临时行撤销。
mod generator;
/// 结果网格交互（排序 / 选区 / 列宽 / 导出入口）。
mod grid;
/// 数据导入面板（F-74）。
mod import_panel;
/// 生命周期与全局状态（初始化 / 轮询 / 标签页 / 布局）。
mod lifecycle;
/// 字段血缘面板（F-88）。
mod lineage_panel;
/// 对象操作（变更集 / 分页 / DDL / 危险操作）。
mod objects;
/// AI 优化面板（F-92）。
mod optimize_panel;
/// 分区管理面板（F-85）。
mod partition_panel;
/// 布局与渲染实现（从本文件拆出）。
mod render;
/// 事务控制、结果标签、消息与运行条。
mod result_tabs;
/// 结果网格渲染。
mod result_view;
/// 例程面板（F-82）。
mod routine_panel;
/// 设置面板（外观 / 布局 / 工具 / 连接 / 关于）。
mod settings_panel;
/// 侧栏与导航（连接树 / 历史 / 文件 / 帮助）。
mod sidebar;
/// 状态栏与标题栏。
mod status_bar;
/// 跨库传输面板（F-75）。
mod transfer_panel;
/// 工作区布局、菜单与 Esc 分发。
mod workspace;

use editing::*;

const CARD_GAP: f32 = 6.;
const CARD_RADIUS: f32 = 8.;
const RAIL_W: f32 = 38.4;
const TITLE_BAR_HEIGHT: f32 = 28.;
const STATUS_BAR_HEIGHT: f32 = 22.;
/// 工作区固定外壳：标题栏 28 + 状态栏 22 + 标签栏 31 + 工具栏 34 + 卡片间距 12 + 边框 2。
const WORKSPACE_CHROME_H: f32 = 129.;
/// 可选面板占位（保守估计，仅用于夹取编辑器高度上限）。
const PARAMS_PANEL_H: f32 = 34.;
const IMPORT_PANEL_H: f32 = 40.;
const MAX_VIEW_ROWS: usize = 300;
/// 列宽下限 / 上限（拖拽把手按此夹取）。
const MIN_COL_W: f32 = 72.;
const MAX_COL_W: f32 = 640.;
/// 表头右侧列宽拖拽把手的宽度。
const COL_HANDLE_W: f32 = 6.;
/// 表头筛选按钮的宽度。
const COL_FILTER_W: f32 = 16.;
/// 表数据分页的页大小选项（循环切换，回到第 1 页）。
const PAGE_SIZE_CHOICES: [u32; 4] = [100, 200, 500, 1000];
/// 结果网格行高由 `LayoutSettings::grid_density` 决定（虚拟列表要求等行高）。
/// 网格单元格字号（表头与数据行一致，自适应列宽按此测量）。
const CELL_FONT_SIZE: f32 = crate::design::TS_CAPTION;
/// 单元格左右内边距合计（`.px_2()` 两侧各 8px）。
const CELL_PAD_X: f32 = 16.;
/// 自适应列宽时最多统计的行数（0.8.0 `fit_col` 只按可见内容估算）。
const FIT_SAMPLE_ROWS: usize = 200;

const DEFAULT_SQL: &str = "SELECT c.name AS 渠道,\n       COUNT(*) AS 订单数,\n       SUM(o.total) AS GMV\nFROM orders o\nJOIN channels c ON c.id = o.channel_id\nGROUP BY c.name\nORDER BY GMV DESC;\n\nSELECT status, COUNT(*) AS 订单数\nFROM orders\nGROUP BY status\nORDER BY 订单数 DESC;";

#[derive(Clone, Copy, PartialEq, Eq)]
enum PanelKind {
    Dashboard,
    Connections,
    Files,
    History,
}

impl PanelKind {
    const ALL: [PanelKind; 4] = [
        PanelKind::Dashboard,
        PanelKind::Connections,
        PanelKind::Files,
        PanelKind::History,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "概览",
            Self::Connections => "连接",
            Self::Files => "文件",
            Self::History => "历史",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Dashboard => IconName::LayoutDashboard,
            Self::Connections => IconName::Database,
            Self::Files => IconName::SquareCode,
            Self::History => IconName::Clock,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Dashboard => "dashboard",
            Self::Connections => "connections",
            Self::Files => "files",
            Self::History => "history",
        }
    }
}

/// 面板组 id：按「当前侧栏面板 + 哪几列在显示」分叉。
///
/// `resizable_panel().size(..)` 只在首次布局生效，同一个 id 下换面板会共用旧宽度；
/// 而面板组的 slot 是按位置存的，列数一变就会张冠李戴。两个维度都进 key，
/// 换面板或折叠列时新 state 首帧直接按当前配置布局。
fn layout_split_id(panel: PanelKind, sidebar_visible: bool, ai_visible: bool) -> gpui::ElementId {
    gpui::ElementId::Name(SharedString::from(format!(
        "main-split:{}:{}{}",
        panel.key(),
        if sidebar_visible { "s" } else { "-" },
        if ai_visible { "a" } else { "-" },
    )))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusKind {
    Idle,
    Busy,
    Ok,
    Warn,
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SortState {
    column: usize,
    ascending: bool,
}

/// 结果网格的矩形选区：锚点与焦点都是结果集内部的原始行下标。
/// 排序/筛选会改变显示顺序，相关操作发生时直接清空选区。
#[derive(Clone, Copy)]
struct GridSelection {
    result: ResultSetId,
    anchor: (usize, usize),
    focus: (usize, usize),
}

/// 选区在「当前显示顺序」中的矩形范围（行是显示位置，列是列下标）。
#[derive(Clone, Copy)]
struct GridRange {
    top: usize,
    bottom: usize,
    left: usize,
    right: usize,
}

impl GridRange {
    fn rows(&self) -> usize {
        self.bottom - self.top + 1
    }

    fn cols(&self) -> usize {
        self.right - self.left + 1
    }

    fn is_multi_cell(&self) -> bool {
        self.rows() * self.cols() > 1
    }
}

fn selection_range(selection: Option<GridSelection>, result: &ResultView) -> Option<GridRange> {
    let selection = selection.filter(|selection| selection.result == result.id)?;
    let order = result.ordered_row_indices();
    let anchor = order.iter().position(|row| *row == selection.anchor.0)?;
    let focus = order.iter().position(|row| *row == selection.focus.0)?;
    let left = selection.anchor.1.min(selection.focus.1);
    let right = selection.anchor.1.max(selection.focus.1);
    if right >= result.columns.len() {
        return None;
    }
    Some(GridRange {
        top: anchor.min(focus),
        bottom: anchor.max(focus),
        left,
        right,
    })
}

/// 复制取数范围：整结果，或当前多格选区。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PayloadScope {
    All,
    Selection,
}

/// 列宽拖拽进行中：按住表头把手时的起点与起始列宽（都在窗口坐标/像素）。
#[derive(Clone, Copy)]
struct ColResize {
    column: usize,
    start_x: f32,
    start_w: f32,
}

/// 编辑器 / 结果区分隔条拖拽中：起点 Y、起手高度、是否真的拖起来了。
/// `active` 把「单击 / 双击」与「拖拽」分开：只有拖起来才盖遮罩，
/// 否则遮罩会吃掉双击的第二次 mouse_down。
#[derive(Clone, Copy)]
struct EditorResize {
    start_y: f32,
    start_h: f32,
    active: bool,
}

/// 表设计器状态：现状（diff 基准） + 用户草稿 + 各页签输入框。
struct DesignerState {
    path: rustdb_db_api::ObjectPath,
    /// 现状列（diff 基准）。
    before: Vec<rustdb_db_api::ColumnInfo>,
    /// 现状索引 / 外键 / 检查约束（只读基线，删除它们会生成 DROP 语句）。
    before_indexes: Vec<rustdb_db_api::IndexInfo>,
    before_foreign_keys: Vec<rustdb_db_api::ForeignKeyInfo>,
    before_checks: Vec<rustdb_db_api::CheckConstraint>,
    /// 现状触发器：`None` = 驱动未提供该元数据。
    triggers: Option<Vec<rustdb_db_api::TriggerInfo>>,
    /// 用户草稿（列 / 索引 / 外键 / 检查约束 / 选项）。
    draft: rustdb_core::DesignerDraft,
    /// 当前页签。
    tab: rustdb_core::DesignerTab,
    /// 保存前校验结论。
    issues: Vec<rustdb_core::ValidationIssue>,
    /// 上次迁移失败：明细 + 恢复建议。
    failure: Option<DesignerFailure>,
    /// 各页签输入框（按当前页签懒创建）。
    inputs: DesignerInputs,
}

/// 迁移失败后的现场信息。
#[derive(Clone)]
pub(crate) struct DesignerFailure {
    message: String,
    hint: String,
}

/// 设计器里刚下发的一条外键操作（等它完成 / 失败，好把基线同步过去）。
#[derive(Clone)]
pub(crate) struct DesignerFkOp {
    name: String,
    /// `true` = 新增（完成后写进基线），`false` = 删除（完成后从基线移除）。
    add: bool,
    /// 新增时用来写回基线的外键定义。
    definition: Option<rustdb_db_api::ForeignKeyInfo>,
}

/// F-83：依赖区当前展示方向。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum RoutineDepsTab {
    #[default]
    DependsOn,
    DependedBy,
}

/// F-83：例程依赖区状态（加载 / 失败 / 结果）。
#[derive(Clone, Default)]
struct RoutineDeps {
    tab: RoutineDepsTab,
    loading: bool,
    error: Option<String>,
    list: Option<rustdb_db_api::DependencyList>,
}

/// F-82：例程源码面板（视图 / 存储过程 / 函数 / 触发器 / 程序包）。
#[derive(Clone)]
pub(crate) struct RoutinePanel {
    path: rustdb_db_api::ObjectPath,
    kind: rustdb_core::RoutineKind,
    /// 面板标题（对象限定名）。
    label: String,
    /// 库里的原始源码（保存成功后作为新基线）。
    original: String,
    /// 当前草稿（与编辑器同步，事件回调里没有窗口也能取到）。
    draft: String,
    /// 源码编辑器。
    editor: gpui::Entity<EditorState>,
    /// 搜索框。
    search: gpui::Entity<gpui_component::input::InputState>,
    /// 上次保存失败：明细 + 恢复建议。
    failure: Option<DesignerFailure>,
    /// 正在保存的语句 id。
    saving: Option<QueryId>,
    /// 该对象不能保存的原因（显式降级，不静默）。
    readonly_reason: Option<String>,
    /// 正在读取源码。
    loading: bool,
    /// F-83：对象依赖。
    deps: RoutineDeps,
}

/// F-82：例程保存前的确认内容（预览 + 警告）。
struct RoutineSavePreview {
    plan: rustdb_core::RoutineSavePlan,
}

/// F-85：分区表单当前动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartitionForm {
    /// 表单未展开。
    None,
    /// 新建分区。
    Create,
    /// 把已有表附加为分区。
    Attach,
}

/// F-85：分区管理面板（首期 PostgreSQL 声明式分区）。
#[derive(Clone)]
pub(crate) struct PartitionPanel {
    path: rustdb_db_api::ObjectPath,
    label: String,
    loading: bool,
    error: Option<String>,
    list: Option<rustdb_db_api::PartitionList>,
    /// 表单动作（新建 / 附加 / 收起）。
    form: PartitionForm,
    /// 分区名，或待附加的表名。
    name: gpui::Entity<gpui_component::input::InputState>,
    /// 范围边界（起点 / 终点）。
    range_from: gpui::Entity<gpui_component::input::InputState>,
    range_to: gpui::Entity<gpui_component::input::InputState>,
    /// 列表边界（逗号分隔，原样拼入 SQL）。
    list_values: gpui::Entity<gpui_component::input::InputState>,
    /// 哈希边界。
    modulus: gpui::Entity<gpui_component::input::InputState>,
    remainder: gpui::Entity<gpui_component::input::InputState>,
    /// 新建 / 附加为 DEFAULT 分区（哈希策略不支持）。
    is_default: bool,
    /// 分离时使用 CONCURRENTLY（PostgreSQL 14+）。
    detach_concurrently: bool,
    /// 待确认的 DDL 预览。
    preview: Option<PartitionPreview>,
    /// 正在执行的分区语句。
    running: Option<QueryId>,
    /// 上一次失败：明细 + 恢复建议。
    failure: Option<DesignerFailure>,
}

/// F-85：分区 DDL 预览内容。
#[derive(Clone)]
struct PartitionPreview {
    plan: rustdb_core::PartitionPlan,
}

/// F-90：配置交换面板页签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExchangeTab {
    Export,
    Import,
}

/// F-90：导出列表里的一行（勾选状态 + 源连接 id）。
#[derive(Clone)]
struct ExchangeExportRow {
    id: String,
    profile: rustdb_core::ExchangeProfile,
    selected: bool,
}

/// F-90：连接配置的加密导出 / 导入预览面板。
#[derive(Clone)]
pub(crate) struct ExchangePanel {
    tab: ExchangeTab,
    /// 导出页：当前连接的非密钥副本。
    export_rows: Vec<ExchangeExportRow>,
    /// 导出时是否带上 SSH 隧道（只保留主机 / 用户名 / 认证方式，不含口令）。
    include_tunnels: bool,
    /// 加密口令；留空表示明文导出（需二次确认）。
    passphrase: gpui::Entity<gpui_component::input::InputState>,
    /// 明文导出的风险确认已点过一次。
    confirm_plain: bool,
    /// 导入页：解密后的预览。
    preview: Option<rustdb_core::ExchangePreview>,
    /// 与 `preview.profiles` 一一对应的勾选状态。
    selected: Vec<bool>,
    /// 已读入但仍需口令的文件内容。
    pending_text: Option<String>,
    /// 当前文件需要口令才能预览。
    needs_passphrase: bool,
    /// 预览来自其他客户端（草稿）。
    drafts: bool,
    /// 面板内提示（文本, 是否错误）。
    message: Option<(String, bool)>,
}

/// F-02：连接编辑器（新建 / 编辑连接配置）。
struct ConnEditor {
    /// 正在编辑的配置 id；`None` 表示新建。
    id: Option<String>,
    name: gpui::Entity<gpui_component::input::InputState>,
    folder: gpui::Entity<gpui_component::input::InputState>,
    driver: DbKind,
    host: gpui::Entity<gpui_component::input::InputState>,
    port: gpui::Entity<gpui_component::input::InputState>,
    database: gpui::Entity<gpui_component::input::InputState>,
    username: gpui::Entity<gpui_component::input::InputState>,
    password: gpui::Entity<gpui_component::input::InputState>,
    ssl_mode: gpui::Entity<gpui_component::input::InputState>,
    read_only: bool,
    production: bool,
    /// F-91：是否走 SSH 隧道。
    tunnel: bool,
    /// 跳板编辑行（按顺序建链）。
    hops: Vec<HopEditor>,
    /// F-91a：是否走代理。
    proxy: bool,
    proxy_kind: rustdb_db_api::ProxyKind,
    proxy_host: gpui::Entity<gpui_component::input::InputState>,
    proxy_port: gpui::Entity<gpui_component::input::InputState>,
    proxy_username: gpui::Entity<gpui_component::input::InputState>,
    /// 代理口令（**只留在内存**，不落盘）。
    proxy_password: gpui::Entity<gpui_component::input::InputState>,
    /// SOCKS5：域名交给代理解析。
    proxy_remote_dns: bool,
    /// 代理相对 SSH 跳板的位置。
    proxy_order: rustdb_db_api::ProxyOrder,
    /// F-91a：本地转发是否对局域网开放（默认关闭）。
    forward_lan: bool,
    /// 校验 / 保存失败的提示。
    error: Option<String>,
}

/// F-91：一层跳板的编辑状态。
struct HopEditor {
    host: gpui::Entity<gpui_component::input::InputState>,
    port: gpui::Entity<gpui_component::input::InputState>,
    user: gpui::Entity<gpui_component::input::InputState>,
    auth: HopAuth,
    /// 私钥路径（`HopAuth::Key` / `KeyPassphrase` 时有效）。
    key_path: gpui::Entity<gpui_component::input::InputState>,
    /// 私钥口令或 SSH 口令（**只留在内存**，不落盘）。
    secret: gpui::Entity<gpui_component::input::InputState>,
}

/// F-91：跳板的认证方式（界面选项）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum HopAuth {
    /// 私钥（无口令）。
    Key,
    /// 私钥 + 口令。
    KeyPassphrase,
    /// 密码。
    Password,
    /// 无认证。
    None,
}

impl HopAuth {
    const ALL: [Self; 4] = [Self::Key, Self::KeyPassphrase, Self::Password, Self::None];

    fn label(self) -> &'static str {
        match self {
            Self::Key => "私钥",
            Self::KeyPassphrase => "私钥+口令",
            Self::Password => "密码",
            Self::None => "无认证",
        }
    }
}

/// F-91：待用户确认的主机密钥。
struct HostKeyPrompt {
    profile_id: String,
    /// 第几层跳板。
    layer: usize,
    host: Option<String>,
    port: Option<u16>,
    fingerprint: String,
    public_key: String,
    /// true = 与已有记录不一致（可能是中间人替换）。
    changed: bool,
    /// 变更时的旧指纹。
    recorded: Option<String>,
}

/// 设计器各页签的输入框（按当前页签懒创建，避免一次造十三个输入框）。
#[derive(Default)]
struct DesignerInputs {
    column_name: Option<gpui::Entity<gpui_component::input::InputState>>,
    column_type: Option<gpui::Entity<gpui_component::input::InputState>>,
    index_name: Option<gpui::Entity<gpui_component::input::InputState>>,
    index_columns: Option<gpui::Entity<gpui_component::input::InputState>>,
    fk_name: Option<gpui::Entity<gpui_component::input::InputState>>,
    fk_columns: Option<gpui::Entity<gpui_component::input::InputState>>,
    fk_ref_table: Option<gpui::Entity<gpui_component::input::InputState>>,
    fk_ref_columns: Option<gpui::Entity<gpui_component::input::InputState>>,
    check_name: Option<gpui::Entity<gpui_component::input::InputState>>,
    check_expr: Option<gpui::Entity<gpui_component::input::InputState>>,
    engine: Option<gpui::Entity<gpui_component::input::InputState>>,
    collation: Option<gpui::Entity<gpui_component::input::InputState>>,
    comment: Option<gpui::Entity<gpui_component::input::InputState>>,
}

impl DesignerInputs {
    /// 按当前页签补齐该页需要的输入框（其余页签的不建）。
    fn ensure(
        &mut self,
        tab: rustdb_core::DesignerTab,
        options: &rustdb_core::TableOptionsDraft,
        window: &mut Window,
        cx: &mut Context<MainView>,
    ) {
        match tab {
            rustdb_core::DesignerTab::Columns => {
                if self.column_name.is_none() {
                    self.column_name = Some(designer_input(window, cx, "列名", None));
                }
                if self.column_type.is_none() {
                    self.column_type = Some(designer_input(window, cx, "类型，如 text", None));
                }
            }
            rustdb_core::DesignerTab::Indexes => {
                if self.index_name.is_none() {
                    self.index_name = Some(designer_input(window, cx, "索引名", None));
                }
                if self.index_columns.is_none() {
                    self.index_columns = Some(designer_input(window, cx, "列，逗号分隔", None));
                }
            }
            rustdb_core::DesignerTab::ForeignKeys => {
                if self.fk_name.is_none() {
                    self.fk_name = Some(designer_input(window, cx, "外键名", None));
                }
                if self.fk_columns.is_none() {
                    self.fk_columns = Some(designer_input(window, cx, "本地列", None));
                }
                if self.fk_ref_table.is_none() {
                    self.fk_ref_table = Some(designer_input(window, cx, "引用表", None));
                }
                if self.fk_ref_columns.is_none() {
                    self.fk_ref_columns = Some(designer_input(window, cx, "引用列", None));
                }
            }
            rustdb_core::DesignerTab::Checks => {
                if self.check_name.is_none() {
                    self.check_name = Some(designer_input(window, cx, "约束名", None));
                }
                if self.check_expr.is_none() {
                    self.check_expr =
                        Some(designer_input(window, cx, "表达式，如 amount > 0", None));
                }
            }
            rustdb_core::DesignerTab::Triggers => {}
            rustdb_core::DesignerTab::Options => {
                if self.engine.is_none() {
                    self.engine = Some(designer_input(
                        window,
                        cx,
                        "存储引擎，如 InnoDB",
                        options.engine.as_deref(),
                    ));
                }
                if self.collation.is_none() {
                    self.collation = Some(designer_input(
                        window,
                        cx,
                        "排序规则",
                        options.collation.as_deref(),
                    ));
                }
                if self.comment.is_none() {
                    self.comment = Some(designer_input(
                        window,
                        cx,
                        "表注释",
                        options.comment.as_deref(),
                    ));
                }
            }
        }
    }
}

/// 造一个设计器用的单行输入框（`initial` 非空时预填现值）。
fn designer_input(
    window: &mut Window,
    cx: &mut Context<MainView>,
    placeholder: &str,
    initial: Option<&str>,
) -> gpui::Entity<gpui_component::input::InputState> {
    let entity = cx.new(|cx| {
        gpui_component::input::InputState::new(window, cx).placeholder(placeholder.to_string())
    });
    if let Some(initial) = initial.filter(|text| !text.is_empty()) {
        entity.update(cx, |state, cx| {
            state.set_value(initial.to_string(), window, cx)
        });
    }
    entity
}

/// 连接编辑器用的单行输入框（同 `designer_input`，语义上分开更清楚）。
fn conn_input(
    window: &mut Window,
    cx: &mut Context<MainView>,
    placeholder: &str,
    initial: Option<&str>,
) -> gpui::Entity<gpui_component::input::InputState> {
    designer_input(window, cx, placeholder, initial)
}

/// F-86：把确认过的重命名写成输入框文本。
fn rename_text(pairs: &[rustdb_core::RenamePair]) -> String {
    pairs
        .iter()
        .map(|pair| format!("{}=>{}", pair.from, pair.to))
        .collect::<Vec<_>>()
        .join(", ")
}

/// F-86：解析 `旧表=>新表`（也接受 `->`）清单；非法项直接忽略。
fn parse_rename_pairs(text: &str) -> Vec<rustdb_core::RenamePair> {
    text.split([',', '，', '\n', '\r', ';', '；'])
        .filter_map(|item| {
            let item = item.trim();
            if item.is_empty() {
                return None;
            }
            let (from, to) = item.split_once("=>").or_else(|| item.split_once("->"))?;
            let (from, to) = (from.trim(), to.trim());
            if from.is_empty() || to.is_empty() {
                return None;
            }
            Some(rustdb_core::RenamePair {
                from: from.to_string(),
                to: to.to_string(),
            })
        })
        .take(rustdb_core::MAX_RENAME_CANDIDATES)
        .collect()
}

/// F-90：交换面板里的勾选框（纯展示，点击由整行处理）。
fn exchange_check(checked: bool, t: &rustdb_core::theme::ThemeTokens) -> gpui::AnyElement {
    div()
        .w(px(13.))
        .h(px(13.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .border_1()
        .border_color(hex(if checked { t.accent } else { t.border_subtle }))
        .when(checked, |box_| box_.bg(hex(t.accent)))
        .when(checked, |box_| {
            box_.child(
                Icon::new(IconName::Check)
                    .small()
                    .text_color(hex(t.on_accent)),
            )
        })
        .into_any_element()
}

/// 连接编辑器里的一行：标签 + 输入框。
fn conn_field(
    t: &rustdb_core::theme::ThemeTokens,
    label: &str,
    input: &gpui::Entity<gpui_component::input::InputState>,
    width: gpui::Pixels,
) -> gpui::AnyElement {
    h_flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(38.))
                .text_size(px(crate::design::TS_CAPTION))
                .text_color(hex(t.fg_muted))
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .w(width)
                .child(gpui_component::input::Input::new(input)),
        )
        .into_any_element()
}

/// 连接编辑器里的开关方块（选中时高亮）。
fn toggle_box(t: &rustdb_core::theme::ThemeTokens, checked: bool) -> gpui::AnyElement {
    div()
        .w(px(12.))
        .h(px(12.))
        .rounded(px(3.))
        .border_1()
        .border_color(hex(if checked { t.accent } else { t.border_subtle }))
        .bg(hex(if checked { t.accent } else { t.bg_base }))
        .into_any_element()
}

/// 把一份计划的语句与「不支持」清单并进另一份计划。
fn merge_plan(target: &mut rustdb_core::MigrationPlan, other: rustdb_core::MigrationPlan) {
    target.statements.extend(other.statements);
    target.unsupported.extend(other.unsupported);
}

/// 该草稿在 SQLite 下是否必须重建表（改列 / 改外键 / 改检查约束）。
fn designer_needs_rebuild(kind: DbKind, designer: &DesignerState) -> bool {
    if kind != DbKind::Sqlite {
        return false;
    }
    let columns_changed = rustdb_core::columns_changed(&designer.before, &designer.draft.columns);
    let fk_changed = designer.draft.foreign_keys.len() != designer.before_foreign_keys.len()
        || designer
            .draft
            .foreign_keys
            .iter()
            .zip(designer.before_foreign_keys.iter())
            .any(|(draft, old)| draft.name != old.name);
    let check_changed = designer.draft.checks.len() != designer.before_checks.len()
        || designer
            .draft
            .checks
            .iter()
            .zip(designer.before_checks.iter())
            .any(|(draft, old)| draft.name != old.name);
    columns_changed || fk_changed || check_changed
}

/// 对象种类 → 可查看 / 编辑源码的例程种类（非例程返回 `None`）。
fn routine_kind_of(kind: ObjectKind) -> Option<rustdb_core::RoutineKind> {
    use rustdb_core::RoutineKind as R;
    match kind {
        ObjectKind::View => Some(R::View),
        ObjectKind::MaterializedView => Some(R::MaterializedView),
        ObjectKind::Procedure => Some(R::Procedure),
        ObjectKind::Function => Some(R::Function),
        ObjectKind::Trigger => Some(R::Trigger),
        ObjectKind::Package => Some(R::Package),
        _ => None,
    }
}

/// 拖拽排序载荷：gpui 的拖拽预览需要一个可渲染实体。
#[derive(Clone)]
struct ColumnDrag {
    index: usize,
    label: String,
}

impl Render for ColumnDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = current(cx);
        div()
            .px_2()
            .py_1()
            .rounded(px(4.))
            .border_1()
            .border_color(hex(t.accent))
            .bg(hex(t.bg_elevated))
            .text_size(px(crate::design::TS_CAPTION))
            .text_color(hex(t.fg_primary))
            .child(SharedString::from(self.label.clone()))
    }
}

/// 「打开表」后的编辑目标：表路径 + 主键列名（由 TableDetail 异步补全）。
#[derive(Clone)]
struct TableEditTarget {
    path: rustdb_db_api::ObjectPath,
    /// `None` 表示表结构还没取回。
    pk: Option<Vec<String>>,
}

/// 单元格详情查看器的内容。
#[derive(Clone)]
struct CellDetail {
    column: String,
    /// 字段的原生类型（拿不到列元数据时退回值类型）。
    native_type: String,
    nullable: bool,
    /// 字节数（二进制 / 大字段用）。
    size: usize,
    value: Value,
    /// F-80：正文显示方式（JSON 格式化 / 二进制十六进制等）。
    mode: crate::detail::DetailMode,
    /// JSON Pointer（`/a/b/0`）。
    pointer: String,
}

impl CellDetail {
    fn new(column: String, meta: Option<&ColumnMeta>, value: Value) -> Self {
        let native_type = meta
            .map(|meta| meta.native_type.clone())
            .unwrap_or_else(|| value.type_label().to_string());
        Self {
            column,
            native_type,
            nullable: meta.map(|meta| meta.nullable).unwrap_or(true),
            size: value.approx_bytes(),
            mode: crate::detail::default_mode(&value),
            value,
            pointer: String::new(),
        }
    }
}

/// 连接树里右键某个对象时弹出的菜单状态。
#[derive(Clone)]
struct ObjectMenu {
    path: rustdb_db_api::ObjectPath,
    /// 菜单左上角（窗口坐标）。
    at: Point<gpui::Pixels>,
}

/// 连接树里右键库节点时弹出的菜单状态。
#[derive(Clone)]
struct NamespaceMenu {
    ns: Namespace,
    at: Point<gpui::Pixels>,
}

/// 需要二次确认的破坏性动作。
#[derive(Clone)]
pub(crate) enum DangerAction {
    /// 清空表数据（`DELETE FROM`）。
    Truncate { path: rustdb_db_api::ObjectPath },
    /// 删除对象（`DROP …`）。
    Drop { path: rustdb_db_api::ObjectPath },
    /// 重命名对象（`ALTER … RENAME TO`），需要输入新名字。
    Rename { path: rustdb_db_api::ObjectPath },
    /// F-73：按条件批量更新（确认后执行面板里预览过的 UPDATE）。
    BulkUpdate { path: rustdb_db_api::ObjectPath },
}

/// 右键菜单里的一个动作。菜单只是数据，行为集中在 `dispatch_menu`。
#[derive(Clone)]
pub(crate) enum MenuAction {
    /// 连接节点：打开（或重连）。
    Connect(String),
    /// 连接节点：断开。
    Disconnect,
    /// 连接节点：在该连接上开一个查询页。
    NewQuery(Namespace),
    /// 库节点：打开（= 双击）。
    OpenDatabase(Namespace),
    /// 库节点：刷新对象缓存。
    RefreshNamespace(Namespace),
    /// 库节点：按方言插入建表模板。
    NewObject(Namespace, ObjectKind),
    /// 复制文本（对象名 / 限定名 / 库名）。
    Copy(String),
    /// 结果区：按指定格式导出当前结果（F-72）。
    Export(ExportFormat),
    /// 结果区：按指定格式复制当前结果到剪贴板（F-72）。
    CopyAs(ExportFormat),
    /// 表 / 视图：查询前 100 行。
    OpenData(rustdb_db_api::ObjectPath),
    /// 表：设计表（结构面板）。
    DesignTable(rustdb_db_api::ObjectPath),
    /// 表 / 视图：查看 DDL。
    ShowDdl(rustdb_db_api::ObjectPath),
    /// 例程：查看 / 编辑源码（F-82）。
    EditRoutine(rustdb_db_api::ObjectPath),
    /// 表：分区管理（F-85）。
    ManagePartitions(rustdb_db_api::ObjectPath),
    /// 表：字段血缘（F-88）。
    ShowLineage(rustdb_db_api::ObjectPath),
    /// 表 / 视图：统计行数。
    CountRows(rustdb_db_api::ObjectPath),
    /// 表：转储 SQL（DDL + 数据骨架）。
    DumpSql(rustdb_db_api::ObjectPath),
    /// 表：从文件导入数据（F-74）。
    ImportTable(rustdb_db_api::ObjectPath),
    /// 表：维护（OPTIMIZE / VACUUM）。
    MaintainTable(rustdb_db_api::ObjectPath),
    /// 表 / 视图：重命名。
    RenameObject(rustdb_db_api::ObjectPath),
    /// 对象面板：置顶 / 取消置顶。
    TogglePin(rustdb_db_api::ObjectPath),
    /// 表：清空数据。
    Truncate(rustdb_db_api::ObjectPath),
    /// 任意对象：删除。
    Drop(rustdb_db_api::ObjectPath),
    /// 连接节点：设置 / 取消生产标记（F-89）。
    SetProduction(String, bool),
    /// F-02：编辑连接配置。
    EditConnection(String),
    /// F-02：删除连接配置。
    DeleteConnection(String),
    /// 连接节点：临时解锁写操作，参数为解锁秒数（F-89）。
    UnlockWrites(String, u64),
    /// 连接节点：立即回锁（F-89）。
    LockWrites(String),
    /// 结果网格（F-71）：追加 N 行。
    AppendRows(usize),
    /// 结果网格（F-71）：克隆选中行（主键交给数据库默认值）。
    CloneRow,
    /// 结果网格（F-71）：从剪贴板粘贴 TSV。
    PasteTsv,
    /// 结果网格（F-71）：打开数据生成器。
    OpenGenerator,
    /// 结果网格（F-71）：选中单元格设为数据库默认值。
    SetCellDefault,
    /// 结果网格（F-71）：撤销选中行的待提交变更。
    RevertRow,
}

/// 历史面板的日期快捷范围。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryRange {
    All,
    Today,
    Days7,
    Days30,
}

impl HistoryRange {
    const ALL: [Self; 4] = [Self::All, Self::Today, Self::Days7, Self::Days30];

    fn label(self) -> &'static str {
        match self {
            Self::All => "全部时间",
            Self::Today => "今天",
            Self::Days7 => "近 7 天",
            Self::Days30 => "近 30 天",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Today => "today",
            Self::Days7 => "days7",
            Self::Days30 => "days30",
        }
    }

    /// 起始时间（含），毫秒；`All` 返回 `None`。
    fn from_ms(self, now_ms: i64) -> Option<i64> {
        use chrono::{Local, TimeZone};
        match self {
            Self::All => None,
            Self::Today => {
                let local = Local.timestamp_millis_opt(now_ms).single()?;
                let midnight = local.date_naive().and_hms_opt(0, 0, 0)?;
                Some(
                    midnight
                        .and_local_timezone(Local)
                        .single()?
                        .timestamp_millis(),
                )
            }
            Self::Days7 => Some(now_ms - 7 * 86_400_000),
            Self::Days30 => Some(now_ms - 30 * 86_400_000),
        }
    }
}

/// 历史面板当前打开的下拉（来源 / 时间）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HistoryMenu {
    Source,
    Range,
}

fn history_source_key(source: Option<HistorySource>) -> &'static str {
    match source {
        None => "all",
        Some(HistorySource::Manual) => "manual",
        Some(HistorySource::Ai) => "ai",
        Some(HistorySource::Mcp) => "mcp",
        Some(HistorySource::Cli) => "cli",
        Some(HistorySource::File) => "file",
    }
}

/// 历史筛选下拉里选中的一项。
#[derive(Clone, Copy)]
pub(crate) enum HistoryPick {
    Source(Option<HistorySource>),
    Range(HistoryRange),
}

fn history_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 菜单里的一行：动作 + 展示文案 + 图标 + 右侧提示 + 是否危险色。
pub(crate) struct MenuEntry {
    /// 稳定的 ASCII 标识（测试用 selector）；为空时退回 label。
    key: &'static str,
    action: MenuAction,
    label: String,
    icon: IconName,
    hint: Option<String>,
    danger: bool,
}

impl MenuEntry {
    fn new(action: MenuAction, label: impl Into<String>, icon: IconName) -> Self {
        Self {
            key: "",
            action,
            label: label.into(),
            icon,
            hint: None,
            danger: false,
        }
    }

    fn key(mut self, key: &'static str) -> Self {
        self.key = key;
        self
    }

    fn hint(mut self, hint: &str) -> Self {
        self.hint = Some(hint.to_string());
        self
    }

    fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
}

pub(crate) struct ResultView {
    id: ResultSetId,
    columns: Vec<ColumnMeta>,
    rows: Vec<Vec<Value>>,
    loaded: u64,
    elapsed_ms: u64,
    truncated: bool,
    affected: Option<u64>,
    /// 客户端排序（仅作用于已加载行；重新查询或切换结果集不清除）。
    sort: Option<SortState>,
    /// 每列的用户定宽（像素）；`None` 表示交给 flex 均分剩余空间。
    /// 只在用户拖过表头把手后才落值，重跑查询不复位。
    widths: Vec<Option<f32>>,
    /// 每列的筛选条件（`None` 或空串表示不过滤）。
    filters: Vec<Option<String>>,
    /// 可编辑目标（仅「打开表」产生的结果集有值）：表路径 + 主键列下标。
    editable: Option<(rustdb_db_api::ObjectPath, Vec<usize>)>,
    /// 待提交变更集（首次编辑时创建）。
    changes: Option<rustdb_core::ChangeSet>,
    /// F-70：服务端分页状态；仅「打开表」走分页时存在。
    page: Option<TablePage>,
}

/// 服务端分页视图的状态（页码 1 起，`order` 按列名下推到服务端）。
#[derive(Clone, Debug)]
struct TablePage {
    path: rustdb_db_api::ObjectPath,
    page: u64,
    page_size: u32,
    has_more: bool,
    order: Option<(String, bool)>,
}

impl ResultView {
    /// 第 `index` 列的用户定宽；`None` 表示交给 flex 均分剩余空间。
    fn fixed_width(&self, index: usize) -> Option<f32> {
        self.widths
            .get(index)
            .copied()
            .flatten()
            .map(|width| width.clamp(MIN_COL_W, MAX_COL_W))
    }

    /// 列的最小总宽：拖过的列用定宽，其余按下限 `MIN_COL_W` 累加。
    /// flex 列在空间不足时被 `min_w` 顶到这个宽度，超出部分靠横向滚动看。
    fn min_total_width(&self) -> f32 {
        (0..self.columns.len())
            .map(|index| self.fixed_width(index).unwrap_or(MIN_COL_W))
            .sum()
    }

    /// 设置列宽（拖拽把手用），越界值夹到上下限。
    fn set_width(&mut self, index: usize, width: f32) -> bool {
        if index >= self.columns.len() {
            return false;
        }
        if self.widths.len() < self.columns.len() {
            self.widths.resize(self.columns.len(), None);
        }
        let width = width.clamp(MIN_COL_W, MAX_COL_W);
        if self.widths[index] == Some(width) {
            return false;
        }
        self.widths[index] = Some(width);
        true
    }

    /// 结果集标签：DML 显示影响行数，查询显示行数。
    fn tab_label(&self, index: usize) -> String {
        match (self.columns.is_empty(), self.affected) {
            (true, Some(n)) => format!("语句 {} · {n} 行", index + 1),
            (true, None) => format!("语句 {}", index + 1),
            (false, _) => format!("结果 {}", index + 1),
        }
    }

    /// 导出数据：表头 + 按当前排序展开的行（不截断、NULL 为空）。仅测试使用。
    #[cfg(test)]
    fn export_rows(&self) -> (Vec<String>, Vec<Vec<String>>) {
        let headers: Vec<String> = self.columns.iter().map(|c| c.name.to_string()).collect();
        let rows: Vec<Vec<String>> = self
            .ordered_row_indices()
            .into_iter()
            .map(|index| {
                self.rows[index]
                    .iter()
                    .map(|value| value.export_text())
                    .collect()
            })
            .collect();
        (headers, rows)
    }

    /// 按当前排序返回行下标；NULL 无论升降序都排在最后。
    fn ordered_row_indices(&self) -> Vec<usize> {
        ordered_indices(&self.rows, self.sort, &self.filters)
    }

    /// 某列的筛选关键字（去空白后小写）；没有筛选返回 `None`。
    fn filter_of(&self, index: usize) -> Option<&str> {
        self.filters
            .get(index)
            .and_then(|filter| filter.as_deref())
            .map(str::trim)
            .filter(|filter| !filter.is_empty())
    }

    /// 设置 / 清除某列筛选，返回是否有变化。
    fn set_filter(&mut self, index: usize, value: Option<String>) {
        if index >= self.columns.len() {
            return;
        }
        if self.filters.len() < self.columns.len() {
            self.filters.resize(self.columns.len(), None);
        }
        let value = value.filter(|text| !text.trim().is_empty());
        self.filters[index] = value;
    }

    /// 待提交变更条数。
    fn change_count(&self) -> usize {
        self.changes.as_ref().map(|set| set.len()).unwrap_or(0)
    }

    /// 某行的变更种类（行底色 / 行号标记用）。
    fn change_of(&self, row: usize) -> Option<rustdb_core::ChangeKind> {
        self.changes.as_ref().and_then(|set| set.row_state(row))
    }

    /// 某单元格是否被改过（单元格左上角标记用）。
    fn cell_dirty(&self, row: usize, col: usize) -> bool {
        self.changes
            .as_ref()
            .map(|set| {
                set.changes()
                    .iter()
                    .any(|change| change.row == row && change.dirty.contains(&col))
            })
            .unwrap_or(false)
    }

    /// 某单元格是否交回数据库默认值（网格里显示「(默认)」）。
    fn cell_default(&self, row: usize, col: usize) -> bool {
        self.changes
            .as_ref()
            .map(|set| {
                set.changes()
                    .iter()
                    .any(|change| change.row == row && change.defaults.contains(&col))
            })
            .unwrap_or(false)
    }

    /// 过滤后仍可见的行数（底栏展示「已筛选 n / m」）。
    fn visible_row_count(&self) -> usize {
        self.ordered_row_indices().len()
    }

    /// 所有生效的筛选条件（只看是否有，不关心具体值）。
    fn filter_of_all(&self) -> Vec<(usize, &str)> {
        (0..self.columns.len())
            .filter_map(|index| self.filter_of(index).map(|text| (index, text)))
            .collect()
    }
}

/// 排序 + 筛选后的行下标；`sort = None` 时保持原始顺序。
fn ordered_indices(
    rows: &[Vec<Value>],
    sort: Option<SortState>,
    filters: &[Option<String>],
) -> Vec<usize> {
    let active: Vec<(usize, String)> = filters
        .iter()
        .enumerate()
        .filter_map(|(index, filter)| {
            filter
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| (index, text.to_lowercase()))
        })
        .collect();

    let mut indices: Vec<usize> = (0..rows.len())
        .filter(|&row| {
            active.iter().all(|(column, needle)| {
                rows[row]
                    .get(*column)
                    .map(|value| value.display_text().to_lowercase().contains(needle))
                    .unwrap_or(false)
            })
        })
        .collect();

    let Some(sort) = sort else {
        return indices;
    };
    indices.sort_by(|&left, &right| {
        let a = rows[left].get(sort.column);
        let b = rows[right].get(sort.column);
        let ordering = match (a, b) {
            (Some(Value::Null), Some(Value::Null)) => return std::cmp::Ordering::Equal,
            (Some(Value::Null), _) => return std::cmp::Ordering::Greater,
            (_, Some(Value::Null)) => return std::cmp::Ordering::Less,
            (Some(a), Some(b)) => compare_values(a, b),
            _ => std::cmp::Ordering::Equal,
        };
        if sort.ascending {
            ordering
        } else {
            ordering.reverse()
        }
    });
    indices
}

/// 一个 SQL 标签页：绑定一个库（命名空间），各自持有编辑内容与结果集。
///
/// 编辑内容是切换标签时保存的文本快照：切换会写回 `TextareaState`。
/// 布局固定的单编辑器 + 快照，避免在无窗口环境（事件回调）里创建输入实体。
pub(crate) struct SqlTab {
    id: u64,
    ns: Option<Namespace>,
    sql: String,
    /// 打开的 `.sql` 文件；文件标签在保存时写回这里。
    file_path: Option<PathBuf>,
    results: Vec<ResultView>,
    active_result: usize,
    running: Option<QueryId>,
    /// 本次（或刚失败的）批量语句，与 query_id 绑定；「继续」从此处接着跑。
    batch: Option<(QueryId, Vec<BatchStatement>)>,
    /// 失败语句下标（0 起）；后面还有语句时状态栏出现「继续」。
    failed_at: Option<usize>,
    /// 正在执行的语句序号（1 起）与总数。
    current_statement: Option<(usize, usize)>,
    /// 最近一次服务端错误的定位（编辑器字节范围）与原文。
    sql_error: Option<SqlErrorMark>,
    /// 本次执行的告警 / `NOTICE` 原文（服务端顺序）。
    messages: Vec<TabMessage>,
    /// 结果区正显示「消息」页而不是某个结果集。
    show_messages: bool,
    /// F-93：AI 试跑标签（只读、临时），标题带前缀便于区分。
    temporary: bool,
}

/// 消息面板里的一行。
struct TabMessage {
    level: rustdb_db_api::WarningLevel,
    text: String,
}

/// 一条待执行语句：原文 + 它在编辑器全文中的字节起点（用于错误定位）。
#[derive(Clone)]
struct BatchStatement {
    text: String,
    start: usize,
}

/// 服务端错误在编辑器里的位置；`start == end` 时只用于跳转。
#[derive(Clone)]
struct SqlErrorMark {
    message: String,
    start: usize,
    end: usize,
}

/// 执行前的参数面板（F-62）：先收集值、再渲染成可执行 SQL。
struct ParamsPanel {
    /// 打开面板时的语句原文（编辑器未改才复用已填的值）。
    sql: String,
    kind: rustdb_db_api::DbKind,
    /// 选区执行的起点；`None` 表示执行全文。
    selection_start: Option<usize>,
    slots: Vec<ParamSlot>,
    /// 缺值 / 原始 SQL 的提示。
    warning: Option<String>,
    /// 含原始 SQL 参数时，第一次点执行只做确认。
    confirm_raw: bool,
}

#[derive(Clone)]
struct ParamSlot {
    name: String,
    style: PlaceholderStyle,
    input: gpui::Entity<gpui_component::input::InputState>,
    /// `None` 表示按输入自动推断；显式类型会覆盖推断。
    kind: Option<ParamKind>,
}

/// 选择导入文件后、表结构回来之前暂存的上下文。
pub(crate) struct ImportPending {
    path: ObjectPath,
    file_path: PathBuf,
    bytes: Vec<u8>,
}

/// 导入面板（F-74）：文件 → 选项 → 映射 → 批次执行。
struct ImportPanel {
    path: ObjectPath,
    file_path: PathBuf,
    bytes: Vec<u8>,
    target: Vec<ColumnInfo>,
    /// 方言限定名（`"schema"."table"`）。
    table: String,
    options: ImportOptions,
    parsed: Option<ParsedData>,
    plan: Option<ImportPlan>,
    /// 源列下标 → 目标列下标；`None` 表示整列忽略。
    mapping: Vec<Option<usize>>,
    error: Option<String>,
    warning: Option<String>,
    confirm_truncate: bool,
}

impl ImportPanel {
    fn file_name(&self) -> String {
        file_label(&self.file_path)
    }

    /// 按当前选项重新解析 + 预检；映射长度与源列数不一致时回到自动映射。
    fn rebuild(&mut self, kind: DbKind) {
        self.plan = None;
        self.error = None;
        self.warning = None;
        self.confirm_truncate = false;
        match import::parse(&self.bytes, &self.options) {
            Ok(data) => {
                if self.mapping.len() != data.columns.len() {
                    self.mapping = import::auto_mapping(&data.columns, &self.target);
                    self.options.mapping = self.mapping.clone();
                }
                match import::build_plan(kind, &self.table, &self.target, &data, &self.options) {
                    Ok(plan) => self.plan = Some(plan),
                    Err(error) => self.error = Some(error.to_string()),
                }
                self.parsed = Some(data);
            }
            Err(error) => {
                self.parsed = None;
                self.mapping.clear();
                self.options.mapping.clear();
                self.error = Some(error.to_string());
            }
        }
    }
}

/// 数据生成器面板（F-71）：对某一列批量生成测试数据。
#[derive(Clone)]
struct GeneratorPanel {
    /// 目标列下标（结果集列）。
    column: usize,
    /// 生成表达式（`seq:1:1` / `int:1:100` / `date` / `uuid` / `regex:…` / `fk:列名`）。
    expr: gpui::Entity<gpui_component::input::InputState>,
    /// 生成行数。
    count: gpui::Entity<gpui_component::input::InputState>,
    /// 上一次的错误提示。
    error: Option<String>,
}

/// F-73：批量更新的一条条件（字段 / 运算符 / 值）。
#[derive(Clone)]
struct BulkConditionRow {
    column: gpui::Entity<gpui_component::input::InputState>,
    op: rustdb_core::BulkOp,
    value: gpui::Entity<gpui_component::input::InputState>,
}

/// F-73：按条件批量更新面板（SET 单列 + 结构化条件或自定义 WHERE）。
#[derive(Clone)]
struct BulkPanel {
    target: rustdb_db_api::ObjectPath,
    /// 目标表列名（占位提示用）。
    columns: Vec<String>,
    set_column: gpui::Entity<gpui_component::input::InputState>,
    set_value: gpui::Entity<gpui_component::input::InputState>,
    conditions: Vec<BulkConditionRow>,
    join: rustdb_core::BulkJoin,
    /// true = 使用自定义 WHERE。
    custom: bool,
    custom_where: gpui::Entity<gpui_component::input::InputState>,
    /// 最近一次校验通过的预览计划。
    plan: Option<rustdb_core::BulkUpdatePlan>,
    /// 行数预览查询（结果不进标签页）。
    count_query: Option<QueryId>,
    /// 预览得到的影响行数。
    count: Option<u64>,
    /// 执行中的 UPDATE 查询。
    exec_query: Option<QueryId>,
    error: Option<String>,
}

/// F-87：数据库文档面板。左栏编辑笔记，右栏预览 Markdown。
#[derive(Clone)]
struct DocsPanel {
    namespace: Namespace,
    loading: bool,
    input: Option<DocInput>,
    notes: rustdb_core::DocNotes,
    project_note: gpui::Entity<gpui_component::input::InputState>,
    table_note: gpui::Entity<gpui_component::input::InputState>,
    column_note: gpui::Entity<gpui_component::input::InputState>,
    selected_table: Option<usize>,
    selected_column: Option<String>,
    preview: Option<rustdb_core::Documentation>,
    error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocFormat {
    Markdown,
    Dbml,
}

impl DocFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Dbml => "dbml",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Dbml => "DBML",
        }
    }

    fn text(self, docs: &rustdb_core::Documentation) -> &str {
        match self {
            Self::Markdown => &docs.markdown,
            Self::Dbml => &docs.dbml,
        }
    }
}

/// F-88：字段血缘面板。只读聚合，证据分级展示。
#[derive(Clone)]
struct LineagePanel {
    namespace: Namespace,
    loading: bool,
    input: Option<rustdb_core::LineageInput>,
    table: ObjectPath,
    columns: Vec<String>,
    selected: Option<String>,
    filter: gpui::Entity<gpui_component::input::InputState>,
    /// 展示的最低置信度。
    min: rustdb_core::Confidence,
    report: Option<rustdb_core::LineageReport>,
    error: Option<String>,
}

/// F-86：结构对比面板。源 = 参照（期望），目标 = 待修改的实际库。
#[derive(Clone)]
struct ComparePanel {
    session: SessionId,
    /// 当前连接名，显示在标题里（配置文件本身是本机一份）。
    profile: String,
    /// 当前连接的命名空间；「载入当前库」用它。
    namespace: Namespace,
    source: Option<rustdb_core::SchemaSnapshot>,
    target: Option<rustdb_core::SchemaSnapshot>,
    /// [source, target]
    loading: [bool; 2],
    error: [Option<String>; 2],
    config: rustdb_core::CompareConfig,
    report: Option<rustdb_core::CompareReport>,
    ignore_input: gpui::Entity<gpui_component::input::InputState>,
    rename_input: gpui::Entity<gpui_component::input::InputState>,
    drop_tables: bool,
    drop_columns: bool,
    show_rollback: bool,
    /// 部署脚本至少要点两次才执行（中间展示影响与回滚）。
    confirm_execute: bool,
    /// 快照 / 配置变了，下一帧重新算差异（事件回调里拿不到输入框）。
    dirty: bool,
    message: Option<String>,
}

impl ComparePanel {
    fn options(&self) -> rustdb_core::DeployOptions {
        rustdb_core::DeployOptions {
            drop_extra_tables: self.drop_tables,
            drop_extra_columns: self.drop_columns,
        }
    }
}

/// F-76：整库逻辑导出面板。默认导出当前库的全部表，可逐表取消。
#[derive(Clone)]
struct DumpPanel {
    session: SessionId,
    profile: String,
    namespace: Namespace,
    /// 已加载元数据里的表名（排好序）。
    tables: Vec<String>,
    selected: HashSet<String>,
    /// 连接下的全部库 / Schema；多库时可按文件拆分导出。
    namespaces: Vec<Namespace>,
    /// true = 全部库各导一个文件（表选择不生效）。
    export_all: bool,
    options: DumpOptions,
    running: bool,
    /// 计划导出的表数 / 已完成的表数。
    total: usize,
    done: usize,
    rows: u64,
    /// 已完成的其它对象数（视图 / 例程等）。
    objects: usize,
    failed: usize,
    /// 正在导出的表名。
    current: Option<String>,
    path: Option<PathBuf>,
    bytes: u64,
    error: Option<String>,
}

impl DumpPanel {
    fn selected_tables(&self) -> Vec<String> {
        self.tables
            .iter()
            .filter(|table| self.selected.contains(*table))
            .cloned()
            .collect()
    }

    fn all_selected(&self) -> bool {
        !self.tables.is_empty() && self.selected.len() == self.tables.len()
    }
}

/// F-75：跨库数据传输面板。源 = 当前会话，目标按连接配置临时建链。
#[derive(Clone)]
pub(crate) struct TransferPanel {
    session: SessionId,
    profile: String,
    namespace: Namespace,
    tables: Vec<String>,
    selected: HashSet<String>,
    /// 可选目标：(配置 id, 显示名)。
    targets: Vec<(String, String)>,
    target: Option<String>,
    options: TransferOptions,
    previewing: bool,
    /// 预览成功后才能执行；选项 / 表选择 / 目标变化会清空。
    preview_ready: bool,
    /// 预览请求发出后选项又变了：迟到的 Preview 一律丢弃。
    preview_stale: bool,
    lines: Vec<String>,
    warnings: Vec<String>,
    running: bool,
    total: usize,
    done: usize,
    rows: u64,
    /// 当前表已写入行数（`TableProgress`）。
    table_rows: u64,
    failed: usize,
    failures: Vec<String>,
    current: Option<String>,
    backup: Option<(String, u64)>,
    elapsed_ms: u64,
    error: Option<String>,
}

impl TransferPanel {
    fn selected_tables(&self) -> Vec<String> {
        self.tables
            .iter()
            .filter(|table| self.selected.contains(*table))
            .cloned()
            .collect()
    }

    fn all_selected(&self) -> bool {
        !self.tables.is_empty() && self.selected.len() == self.tables.len()
    }

    fn target_label(&self) -> Option<&str> {
        let id = self.target.as_deref()?;
        self.targets
            .iter()
            .find(|(target_id, _)| target_id == id)
            .map(|(_, label)| label.as_str())
    }

    fn reset_preview(&mut self) {
        self.preview_ready = false;
        // 正在生成的预览保留 `previewing`，等它回来时按 stale 丢弃，避免并发两份预览串台。
        self.preview_stale = true;
        self.lines.clear();
        self.warnings.clear();
        self.error = None;
    }
}

/// F-78：数据对比与同步面板。源 = 当前会话，目标按连接配置临时建链。
#[derive(Clone)]
pub(crate) struct DataComparePanel {
    session: SessionId,
    profile_id: String,
    profile: String,
    namespace: Namespace,
    tables: Vec<String>,
    selected: HashSet<String>,
    targets: Vec<(String, String)>,
    target: Option<String>,
    /// 关键字段（逗号分隔，空 = 源表主键）；应用到所有选中表。
    keys: gpui::Entity<gpui_component::input::InputState>,
    options: DataCompareOptions,
    previewing: bool,
    preview_ready: bool,
    preview_stale: bool,
    lines: Vec<String>,
    warnings: Vec<String>,
    insert: u64,
    update: u64,
    delete: u64,
    running: bool,
    /// 本次运行是否包含同步（决定结束后是否让预览失效）。
    applying: bool,
    total: usize,
    done: usize,
    failed: usize,
    failures: Vec<String>,
    current: Option<String>,
    applied: u64,
    statements: u64,
    elapsed_ms: u64,
    error: Option<String>,
    config_note: Option<String>,
}

impl DataComparePanel {
    fn selected_tables(&self) -> Vec<String> {
        self.tables
            .iter()
            .filter(|table| self.selected.contains(*table))
            .cloned()
            .collect()
    }

    fn all_selected(&self) -> bool {
        !self.tables.is_empty() && self.selected.len() == self.tables.len()
    }

    fn target_label(&self) -> Option<&str> {
        let id = self.target.as_deref()?;
        self.targets
            .iter()
            .find(|(target_id, _)| target_id == id)
            .map(|(_, label)| label.as_str())
    }

    fn reset_preview(&mut self) {
        self.preview_ready = false;
        self.preview_stale = true;
        self.lines.clear();
        self.warnings.clear();
        self.error = None;
        self.config_note = None;
    }

    fn parse_keys(&self, cx: &gpui::App) -> Vec<String> {
        self.keys
            .read(cx)
            .value()
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .collect()
    }
}

/// F-92：AI 优化 / 方言转换面板（只产文本，插入编辑器后仍走原执行入口）。
struct OptimizePanel {
    kind: OptimizeKind,
    /// 源方言 = 当前连接。
    source: DbKind,
    /// 目标方言（仅「转换」使用）。
    target: DbKind,
    /// 改写要求（仅「改写」使用，按需创建）。
    instruction: Option<gpui::Entity<gpui_component::input::InputState>>,
    /// 打开面板时的原文；diff 与「拒绝」都以它为基准。
    original: String,
    running: bool,
    error: Option<String>,
    result: Option<OptimizeResult>,
    /// 每个 hunk 是否采纳（与 `result.hunks` 下标对齐）。
    accepted: Vec<bool>,
}

/// F-93：AI 上下文选择面板。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AiCtxMenu {
    Table,
    Saved,
}

/// F-93：AI 回答里某个 SQL 块的后台执行记录。
struct AiBlockExec {
    /// 执行所在标签页的 id（不是下标，关标签后下标会变）；提交失败时为 `None`。
    tab_id: Option<u64>,
    /// 提交成功后的查询 id。
    query_id: Option<QueryId>,
    /// 提交阶段就失败的原因（如生产未解锁 / 试跑只读拦截）。
    note: Option<String>,
    temporary: bool,
}

/// F-94：模型配置面板的编辑态。
pub(crate) struct AiModelPanel {
    /// 正在编辑的配置 id；`None` = 从预设新建。
    editing_id: Option<String>,
    kind: rustdb_core::AiKind,
    auth: rustdb_core::AuthKind,
    name: gpui::Entity<gpui_component::input::InputState>,
    base_url: gpui::Entity<gpui_component::input::InputState>,
    model: gpui::Entity<gpui_component::input::InputState>,
    /// 本次会话的密钥（不落盘）。
    key: gpui::Entity<gpui_component::input::InputState>,
    key_ref: gpui::Entity<gpui_component::input::InputState>,
    /// 自定义认证头的名称（`auth = Header` 时生效）。
    auth_header: gpui::Entity<gpui_component::input::InputState>,
    temperature: gpui::Entity<gpui_component::input::InputState>,
    context_window: gpui::Entity<gpui_component::input::InputState>,
    max_turns: gpui::Entity<gpui_component::input::InputState>,
    retries: gpui::Entity<gpui_component::input::InputState>,
    proxy: gpui::Entity<gpui_component::input::InputState>,
    /// 每行一个 `Name=Value`。
    headers: gpui::Entity<gpui_component::input::InputState>,
    instruction: gpui::Entity<gpui_component::input::InputState>,
    cli_command: gpui::Entity<gpui_component::input::InputState>,
    errors: Vec<String>,
    report: Option<rustdb_core::ConnectivityReport>,
    /// 发现到的模型（点击即填入模型名）。
    models: Vec<String>,
    busy: Option<AiPanelTask>,
}

/// F-94：面板里的异步任务（按钮期间置灰）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiPanelTask {
    Test,
    Discover,
}

/// 导入面板的选项胶囊：`debug_selector` 挂在外层 div 上（Button 自身不支持）。
fn import_option_chip(
    id: &'static str,
    label: String,
    tip: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Div {
    div().debug_selector(move || id.to_string()).child(
        Button::new(SharedString::from(id))
            .small()
            .ghost()
            .label(SharedString::from(label))
            .tooltip(tip)
            .on_click(on_click),
    )
}

fn next_param_kind(current: Option<ParamKind>) -> Option<ParamKind> {
    match current {
        None => Some(ParamKind::Text),
        Some(ParamKind::Text) => Some(ParamKind::Number),
        Some(ParamKind::Number) => Some(ParamKind::Bool),
        Some(ParamKind::Bool) => Some(ParamKind::Null),
        Some(ParamKind::Null) => Some(ParamKind::Raw),
        Some(ParamKind::Raw) => Some(ParamKind::List),
        Some(ParamKind::List) => None,
    }
}

fn param_slot_value(slot: &ParamSlot, raw: &str) -> ParamValue {
    match slot.kind {
        None => ParamValue::infer(raw),
        Some(ParamKind::Text) => ParamValue::Text(raw.to_string()),
        Some(ParamKind::Number) => ParamValue::Number(raw.to_string()),
        Some(ParamKind::Bool) => ParamValue::Bool(matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes"
        )),
        Some(ParamKind::Null) => ParamValue::Null,
        Some(ParamKind::Raw) => ParamValue::Raw(raw.to_string()),
        Some(ParamKind::List) => ParamValue::List(
            raw.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect(),
        ),
    }
}

fn param_kind_label(slot: &ParamSlot, raw: &str) -> String {
    match slot.kind {
        None => format!("自动 · {}", ParamValue::infer(raw).kind().label()),
        Some(kind) => kind.label().to_string(),
    }
}

/// 当前连接上的事务（工具栏状态）。
struct TxView {
    started: Instant,
    statements: u64,
    read_only: bool,
}

impl SqlTab {
    fn new(id: u64, ns: Option<Namespace>) -> Self {
        Self {
            id,
            ns,
            sql: String::new(),
            file_path: None,
            results: Vec::new(),
            active_result: 0,
            running: None,
            batch: None,
            failed_at: None,
            current_statement: None,
            sql_error: None,
            messages: Vec::new(),
            show_messages: false,
            temporary: false,
        }
    }

    fn ns_key(&self) -> Option<String> {
        self.ns.as_ref().map(ns_key)
    }

    /// 标签标题：文件标签显示文件名，其余显示库名（PG 显示 schema）。
    fn title(&self) -> String {
        if let Some(path) = &self.file_path {
            return file_label(path);
        }
        let label = self
            .ns
            .as_ref()
            .map(ns_label)
            .unwrap_or_else(|| "查询".to_string());
        if self.temporary {
            format!("AI 试跑 · {label}")
        } else {
            label
        }
    }
}

fn ns_key(ns: &Namespace) -> String {
    ns.display_label()
}

/// F-84：对象在按连接状态里的稳定键（连接由 profile 区分）。
fn object_key(path: &ObjectPath) -> String {
    format!("{}|{:?}|{}", ns_key(&path.ns), path.kind, path.name)
}

/// F-84：分组展开状态的键（含连接，避免多连接互相覆盖）。
fn object_group_key(profile: &str, ns: &str, kind: ObjectKind) -> String {
    format!("{profile}|{ns}|{kind:?}")
}

/// 把任意字节范围夹到合法字符边界；诊断/高亮不能切在多字节字符中间。
fn clamp_span(text: &str, start: usize, end: usize) -> (usize, usize) {
    let mut start = start.min(text.len());
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = end.max(start).min(text.len());
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
    (start, end)
}

/// 驱动位置（1-based 字符）→ 编辑器字节范围；没有位置时覆盖整条失败语句。
fn statement_error_span(statement: &BatchStatement, position: Option<u32>) -> (usize, usize) {
    let statement_end = statement.start + statement.text.len();
    let Some(offset) = position.and_then(|position| {
        rustdb_sql_lang::position::byte_offset_of_char(&statement.text, position as usize)
    }) else {
        return (statement.start, statement_end);
    };
    let start = (statement.start + offset).min(statement_end);
    let end = statement
        .text
        .get(start - statement.start..)
        .and_then(|rest| rest.char_indices().nth(1))
        .map(|(index, _)| start + index)
        .unwrap_or(statement_end);
    (start, end.max(start))
}

fn ns_label(ns: &Namespace) -> String {
    ns.schema
        .as_ref()
        .or(ns.catalog.as_ref())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "default".into())
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// 文档导出文件名：保留字母数字与常见连接符，其余替换为 `-`。
fn doc_file_stem(label: &str) -> String {
    let mut stem = String::with_capacity(label.len());
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' {
            stem.push(ch);
        } else if ch.is_alphanumeric() {
            stem.push(ch);
        } else {
            stem.push('-');
        }
    }
    let trimmed = stem.trim_matches('-');
    if trimmed.is_empty() {
        "database".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 文件树一层的条目：子目录 + `.sql`，目录在前、忽略以 `.` 开头的目录。
///
/// 每层最多 200 条，避免打开一个大目录时把侧栏撑爆。
fn sql_dir_entries(dir: &Path) -> Vec<(PathBuf, bool)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if !hidden {
                dirs.push((path, true));
            }
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("sql"))
        {
            files.push((path, false));
        }
    }
    let key = |path: &PathBuf| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    dirs.sort_by_key(|(path, _)| key(path));
    files.sort_by_key(|(path, _)| key(path));
    dirs.into_iter().chain(files).take(200).collect()
}

/// 对象分组的中文名（树节点不显示任何数量）。
fn kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "表",
        ObjectKind::View => "视图",
        ObjectKind::MaterializedView => "物化视图",
        ObjectKind::Procedure => "存储过程",
        ObjectKind::Function => "函数",
        ObjectKind::Trigger => "触发器",
        ObjectKind::Sequence => "序列",
        ObjectKind::Type => "类型",
        ObjectKind::Index => "索引",
        ObjectKind::Synonym => "同义词",
        ObjectKind::Package => "包",
    }
}

fn kind_icon(kind: ObjectKind) -> IconName {
    match kind {
        ObjectKind::Table => IconName::Table,
        ObjectKind::View | ObjectKind::MaterializedView => IconName::Eye,
        ObjectKind::Procedure | ObjectKind::Function | ObjectKind::Package => {
            IconName::SquareFunction
        }
        ObjectKind::Trigger => IconName::Zap,
        ObjectKind::Sequence => IconName::Hash,
        ObjectKind::Type => IconName::Braces,
        ObjectKind::Index | ObjectKind::Synonym => IconName::KeyRound,
    }
}

/// 按种类分组，组内按驱动返回顺序；空组不出现。
fn group_objects(objects: &[ObjectSummary]) -> Vec<(ObjectKind, Vec<&ObjectSummary>)> {
    let mut kinds: Vec<ObjectKind> = Vec::new();
    for object in objects {
        if !kinds.contains(&object.path.kind) {
            kinds.push(object.path.kind);
        }
    }
    kinds.sort_by_key(|kind| kind.sort_rank());
    kinds
        .into_iter()
        .map(|kind| {
            let items = objects
                .iter()
                .filter(|o| o.path.kind == kind)
                .collect::<Vec<_>>();
            (kind, items)
        })
        .collect()
}

pub struct MainView {
    core: Arc<rustdb_core::Core>,
    events: EventStream,
    focus: FocusHandle,

    panel: PanelKind,
    profiles: Vec<ConnectionProfile>,
    /// 连接树里被折叠的文件夹键，默认全部展开。
    collapsed_folders: HashSet<String>,
    /// 当前连接的配置 id（用于树节点高亮）。
    active_profile: Option<String>,
    /// 正在发起的连接；用于阻止重复点击，并过滤迟到的连接事件。
    connecting: Option<String>,
    session: Option<SessionId>,
    server_label: Option<String>,
    capabilities: Option<Capabilities>,
    namespaces: Vec<Namespace>,
    current_ns: Option<Namespace>,

    /// 自动提交（默认开）。关闭后首次执行前会隐式 `BEGIN`。
    auto_commit: bool,
    /// 当前连接上还没结束的事务。
    tx: Option<TxView>,
    /// 查询历史的搜索关键字（空串表示不过滤）。
    history_filter: String,
    /// 历史面板的来源筛选（`None` = 全部）。
    history_source: Option<HistorySource>,
    /// 历史面板的时间范围。
    history_range: HistoryRange,
    /// 历史筛选下拉：当前打开的类型 + 窗口坐标。
    history_menu: Option<(Point<gpui::Pixels>, HistoryMenu)>,
    /// 历史搜索框的输入态。
    history_input: Option<gpui::Entity<gpui_component::input::InputState>>,
    /// SQL 补全候选项（元数据加载后刷新）。
    completion: std::rc::Rc<std::cell::RefCell<crate::completion::CompletionCatalog>>,
    /// 当前编辑器内容的语义诊断（含服务端错误）；`diag_dirty` 时在渲染前重算。
    diag: crate::diagnostics::Report,
    diag_dirty: bool,
    /// 分页请求序号：只接受最新一次请求的回包（快速翻页时丢弃旧页）。
    page_seq: u64,
    /// 执行前的参数面板（有待填占位符时出现）。
    params_panel: Option<ParamsPanel>,
    /// SQL 文件树根目录；`None` 时显示「选择文件夹」。
    file_root: Option<PathBuf>,
    /// 文件树里展开的子目录。
    file_expanded: HashSet<PathBuf>,
    /// 选择文件后等待表结构（表详情事件回来后打开导入面板）。
    import_pending: Option<ImportPending>,
    /// 导入面板（F-74）。
    import_panel: Option<ImportPanel>,

    /// 已加载的对象：命名空间键 → 对象列表。
    objects: HashMap<String, Vec<ObjectSummary>>,
    /// 正在加载的命名空间键。
    loading: HashSet<String>,
    /// 连接树里展开的库节点。
    expanded_ns: HashSet<String>,
    /// 对象分组的展开状态（键 `连接|库|种类`）；未记录时默认只展开「表」。
    group_state: HashMap<String, bool>,
    /// F-84：对象面板搜索关键字（名称 + 注释）。
    object_filter: String,
    /// 对象面板搜索框（按需创建）。
    object_input: Option<gpui::Entity<gpui_component::input::InputState>>,
    /// 置顶 / 最近访问 / 分组展开的按连接持久化状态。
    object_state: rustdb_platform::object_state::ObjectState,
    /// F-80：详情面板的 JSON 路径输入（按需创建）。
    detail_input: Option<gpui::Entity<gpui_component::input::InputState>>,
    /// 库下拉框打开位置（窗口坐标）；`None` 为关闭。
    db_menu: Option<Point<gpui::Pixels>>,
    /// 对象右键菜单：目标对象 + 打开位置（窗口坐标）。
    obj_menu: Option<ObjectMenu>,
    /// 库节点右键菜单。
    ns_menu: Option<NamespaceMenu>,
    /// 连接节点右键菜单（profile_id、连接名、位置）。
    conn_menu: Option<(String, String, Point<gpui::Pixels>)>,
    /// F-89：当前生产连接写解锁的剩余秒数（每秒变化一次，驱动倒计时刷新）。
    unlock_tick: Option<u64>,
    /// F-71：结果网格「行操作」菜单的打开位置。
    row_menu: Option<Point<gpui::Pixels>>,
    /// F-71：Esc 全局按键订阅（视图存活期间有效）。
    escape_key: Option<gpui::Subscription>,
    /// F-71：数据生成器面板（列、位置与两个输入框）。
    generator: Option<GeneratorPanel>,
    /// F-73：按表缓存外键（键 = 表路径展示名）。
    table_fks: HashMap<String, Vec<rustdb_db_api::ForeignKeyInfo>>,
    /// F-73：批量更新面板。
    bulk: Option<BulkPanel>,
    /// F-73：批量更新执行完，下一帧刷新当前表数据（事件阶段拿不到 window）。
    bulk_refresh: bool,
    /// F-87：数据库文档面板。
    docs: Option<DocsPanel>,
    /// F-87：文档笔记（按连接配置 id 分组持久化）。
    doc_notes: rustdb_core::DocNoteStore,
    doc_notes_path: PathBuf,
    /// F-88：字段血缘面板。
    lineage: Option<LineagePanel>,
    /// F-86：结构对比面板。
    compare: Option<ComparePanel>,
    /// F-86：对比配置落盘位置（`compare.json`）。
    compare_config_path: PathBuf,
    /// F-76：整库逻辑导出面板。
    dump: Option<DumpPanel>,
    /// F-75：跨库数据传输面板。
    transfer: Option<TransferPanel>,
    /// F-78：数据对比与同步面板。
    data_compare: Option<DataComparePanel>,
    /// F-78：对比配置落盘位置（`data-compare.json`）。
    data_compare_config_path: PathBuf,
    /// 三列布局：宽度（侧栏按面板记忆）、折叠与 AI 开关，落 `layout.json`。
    layout: rustdb_core::LayoutSettings,
    layout_path: PathBuf,
    /// 结果区「导出格式」菜单：打开位置（窗口坐标）；`None` 为关闭。
    export_menu: Option<Point<gpui::Pixels>>,
    /// 「设计表」面板：对象名 + 表详情。
    table_panel: Option<(String, rustdb_db_api::TableDetail)>,
    /// 列筛选面板：列下标 + 打开位置。
    filter_menu: Option<(usize, Point<gpui::Pixels>)>,
    /// 筛选输入框（每列共用，打开时重置内容）。
    filter_input: Option<gpui::Entity<gpui_component::input::InputState>>,
    /// 单元格详情：列名 + 类型 + 全文内容 + 渲染分类。
    cell_detail: Option<CellDetail>,
    /// 单元格编辑状态：(行, 列, 列名)。
    edit_cell: Option<(usize, usize, String)>,
    /// 单元格编辑输入框。
    edit_input: Option<gpui::Entity<gpui_component::input::InputState>>,
    /// 「打开表」后的编辑目标（主键列异步取回）。
    table_edit: Option<TableEditTarget>,
    /// 变更集 SQL 预览浮层内容。
    edit_preview: Option<String>,
    /// F-92：AI 优化 / 方言转换面板。
    optimize: Option<OptimizePanel>,
    /// 表设计器状态（打开「设计表」时建立）。
    designer: Option<DesignerState>,
    /// AI Agent 引擎（离线脚本 / OpenAI 兼容端点）。
    agent: std::sync::Arc<rustdb_core::AgentEngine>,
    /// AI 输入框。
    ai_input: Option<gpui::Entity<gpui_component::input::InputState>>,
    /// AI 对话记录：(角色, 文本)；角色 = user / assistant / tool / error。
    ai_lines: Vec<(String, String)>,
    /// 正在流式追加的助手消息下标。
    ai_stream: Option<usize>,
    /// 本次启动是否恢复了上次会话（状态栏提示一次）。
    recovered: bool,
    /// 当前 AI 会话 id（持久化用）。
    ai_session_id: String,
    /// AI 会话历史（最近 50 条，倒序）。
    ai_sessions: Vec<rustdb_platform::ai_sessions::AiSession>,
    /// 历史面板是否展开。
    ai_history_open: bool,
    /// 重命名会话：(会话 id, 输入框)。
    ai_rename: Option<(String, gpui::Entity<gpui_component::input::InputState>)>,
    /// 待确认的工具调用：(run_id, call_id, tool, summary)。
    ai_pending: Option<(String, String, String, String)>,
    ai_running: bool,
    /// F-93：待注入的上下文（表 / 已存 SQL / 选区），发送后清空。
    ai_context: Vec<rustdb_core::ContextItem>,
    /// F-93：正在取结构的表（路径展示名 + 路径）。
    ai_ctx_pending: Option<(String, ObjectPath)>,
    /// F-93：上下文选择面板。
    ai_ctx_menu: Option<AiCtxMenu>,
    /// F-93：回答里 SQL 块的后台执行记录，键 =（消息下标, 该消息内的 SQL 块序号）。
    ai_block_execs: HashMap<(usize, usize), AiBlockExec>,
    /// F-94：多模型配置（`ai_models.json`）。
    ai_settings: rustdb_core::AiSettings,
    ai_config_path: PathBuf,
    /// F-94：会话内输入的密钥（只留在内存，不落盘）。
    ai_keys: HashMap<String, String>,
    /// F-94：模型配置面板。
    ai_model_panel: Option<AiModelPanel>,
    /// 待取回表结构的目标（TableDetailLoaded 事件里拿到 ObjectPath 用）。
    design_pending: Option<rustdb_db_api::ObjectPath>,
    /// 设计器刚下发的迁移：(查询 id, 可选的外键操作)。
    designer_apply_query: Option<(QueryId, Option<DesignerFkOp>)>,
    /// F-82：例程源码面板（视图 / 过程 / 函数 / 触发器 / 程序包）。
    routine: Option<RoutinePanel>,
    /// F-82：例程保存前的确认（预览 + 警告）。
    routine_save_preview: Option<RoutineSavePreview>,
    /// F-82：脏草稿离开确认。
    routine_discard: bool,
    /// F-82：待写入例程编辑器的源码（事件里拿不到 window）。
    pending_routine_source: Option<String>,
    /// F-85：分区管理面板。
    partition: Option<PartitionPanel>,
    /// F-90：连接配置加密交换面板。
    exchange: Option<ExchangePanel>,
    /// 设置面板（低频入口收纳）。
    settings_open: bool,
    /// F-02：连接编辑器（新建 / 编辑连接配置）。
    conn_editor: Option<ConnEditor>,
    /// F-91：待确认的主机密钥。
    host_key_prompt: Option<HostKeyPrompt>,
    /// 已提交、等待结果回填的变更集。
    pending_edit: Option<rustdb_db_api::QueryId>,
    /// 当前选中的单元格（数据行下标, 列下标）；底栏「详情」按钮用。
    selected_cell: Option<(usize, usize)>,
    /// 结果网格矩形选区；切换结果/排序/筛选后失效。
    grid_selection: Option<GridSelection>,
    /// 拖拽选区起点（鼠标按下时记录，click 收尾时清空）。
    drag_select: Option<(usize, usize)>,
    /// 多格选区是否真的扩出了范围（click 收尾时用它决定是否吞掉复制）。
    drag_moved: bool,
    /// 最近一次 DML 预览的提示（结果区顶部提示条，下次执行时清空）。
    preview_note: Option<String>,
    /// 待写入编辑器的 SQL（事件里拿不到 window，下一帧统一应用）。
    pending_sql: Option<(Option<Namespace>, String)>,
    /// 「查看 DDL」请求对应的命名空间（用来决定写进哪个标签）。
    pending_ddl_ns: Option<Namespace>,
    /// 删除对象时要求输入的确认文本（表名）。
    danger_input: Option<gpui::Entity<gpui_component::input::InputState>>,
    /// 待确认的破坏性动作。
    danger: Option<DangerAction>,
    /// 表头列宽拖拽中的状态。
    col_resize: Option<ColResize>,
    /// 编辑器 / 结果区分隔条拖拽中的状态。
    editor_resize: Option<EditorResize>,
    /// 结果网格横向滚动偏移（像素，≥ 0）。
    grid_h: f32,
    /// 绑在行体容器上只用于量测视口宽度（自身不滚动）。
    grid_viewport: gpui::ScrollHandle,

    /// SQL 标签页：只能由连接树里的库节点打开。
    tabs: Vec<SqlTab>,
    active_tab: usize,
    next_tab_id: u64,

    sql_input: gpui::Entity<EditorState>,

    status: String,
    status_kind: StatusKind,
    query_count: u32,
    dark: bool,
    /// 开发/截图用：`RUSTDB_DEMO=1|pg|mysql` 时启动自动连接并执行默认 SQL。
    demo_autorun: bool,
    /// 演示时自动切换到的 schema（`RUSTDB_DEMO_SCHEMA=shop`）。
    demo_schema: Option<String>,
    demo_switched: bool,
    /// 截图钩子：对象加载完后自动弹出表菜单。
    demo_obj_menu: bool,
    /// 截图钩子：元数据加载完后自动打开指定浮层（`transfer` / `data-compare`）。
    demo_panel: Option<String>,
    /// 截图钩子：启动后自动切到指定侧栏面板（keys 见 `PanelKind::key`）。
    demo_sidebar: Option<String>,
}

impl Focusable for MainView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// 服务端版本串往往带编译信息（`PostgreSQL 13.21 (Debian …) on x86_64…`），
/// 标题栏与标签页只保留「产品 + 版本号」，其余省略。
fn short_server_label(version: &str, database: Option<&str>) -> String {
    let head = version.split('(').next().unwrap_or(version).trim();
    let short = head
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    match database.filter(|db| !db.is_empty()) {
        Some(db) => format!("{short} · {db}"),
        None => short,
    }
}

/// F-87：文档预览的轻量 Markdown 排版（标题 / 列表 / 引用 / 表格行），
/// 让预览像「文档」而不是一串带 `#` 的原文。
fn doc_table_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn doc_table_separator(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells
            .iter()
            .all(|cell| !cell.is_empty() && cell.chars().all(|ch| matches!(ch, '-' | ':')))
}

fn render_doc_table(rows: &[Vec<String>], t: &rustdb_core::theme::ThemeTokens) -> gpui::AnyElement {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut table = v_flex()
        .w_full()
        .rounded(px(4.))
        .border_1()
        .border_color(hex(t.border_subtle))
        .overflow_hidden();
    for (index, cells) in rows.iter().enumerate() {
        let header = index == 0;
        let mut row = h_flex().w_full().items_center();
        if header {
            row = row.bg(hex(t.bg_elevated));
        } else if index > 1 {
            row = row.border_t_1().border_color(hex(t.border_subtle));
        }
        for column in 0..columns {
            let text = cells.get(column).map(String::as_str).unwrap_or("");
            row = row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_ellipsis()
                    .when(header, |cell| {
                        cell.font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(hex(t.fg_primary))
                    })
                    .when(!header, |cell| cell.text_color(hex(t.fg_secondary)))
                    .child(SharedString::from(text.to_string())),
            );
        }
        table = table.child(row);
    }
    table.into_any_element()
}

fn render_doc_markdown(markdown: &str, t: &rustdb_core::theme::ThemeTokens) -> gpui::AnyElement {
    let mut body = v_flex().gap(px(1.));
    let lines: Vec<&str> = markdown.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim_end().trim_start();
        if trimmed.starts_with('|') {
            let mut rows: Vec<Vec<String>> = Vec::new();
            while index < lines.len() && lines[index].trim().starts_with('|') {
                let cells = doc_table_row(lines[index]);
                if !doc_table_separator(&cells) {
                    rows.push(cells);
                }
                index += 1;
            }
            if !rows.is_empty() {
                body = body.child(render_doc_table(&rows, t));
            }
            continue;
        }
        index += 1;
        if trimmed.is_empty() {
            body = body.child(div().h(px(4.)));
            continue;
        }
        let (text, size, weight, color) = if let Some(rest) = trimmed.strip_prefix("### ") {
            (
                rest.to_string(),
                11.,
                gpui::FontWeight::SEMIBOLD,
                t.fg_primary,
            )
        } else if let Some(rest) = trimmed.strip_prefix("## ") {
            (
                rest.to_string(),
                12.5,
                gpui::FontWeight::SEMIBOLD,
                t.fg_primary,
            )
        } else if let Some(rest) = trimmed.strip_prefix("# ") {
            (rest.to_string(), 14., gpui::FontWeight::BOLD, t.fg_primary)
        } else if let Some(rest) = trimmed.strip_prefix("> ") {
            (
                format!("│ {rest}"),
                10.,
                gpui::FontWeight::NORMAL,
                t.fg_muted,
            )
        } else if let Some(rest) = trimmed.strip_prefix("- ") {
            (
                format!("• {rest}"),
                10.5,
                gpui::FontWeight::NORMAL,
                t.fg_secondary,
            )
        } else {
            (
                trimmed.to_string(),
                10.5,
                gpui::FontWeight::NORMAL,
                t.fg_secondary,
            )
        };
        body = body.child(
            div()
                .text_size(px(size))
                .font_weight(weight)
                .text_color(hex(color))
                .whitespace_normal()
                .child(SharedString::from(text)),
        );
    }
    body.into_any_element()
}

fn card_shell(bg: u32, cx: &App) -> gpui::Div {
    let t = current(cx);
    v_flex()
        .min_h_0()
        .min_w_0()
        .rounded(px(CARD_RADIUS))
        .bg(hex(bg))
        .border_1()
        .border_color(hex(t.border_strong))
        .overflow_hidden()
}

/// 单行文本的排版宽度（用于双击自适应列宽）。空串直接返回 0，避免无意义排版。
fn text_width(window: &mut Window, text: &str, font_size: f32) -> f32 {
    if text.is_empty() {
        return 0.;
    }
    let run = gpui::TextRun {
        len: text.len(),
        font: gpui::Font::default(),
        color: gpui::Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let shaped = window.text_system().shape_line(
        SharedString::from(text.to_string()),
        px(font_size),
        &[run],
        None,
    );
    f32::from(shaped.width)
}

fn hint_text(text: &str, t: &rustdb_core::theme::ThemeTokens) -> impl IntoElement {
    div()
        .text_size(px(crate::design::TS_CAPTION))
        .text_color(hex(t.fg_muted))
        .child(SharedString::from(text.to_string()))
}

fn metric_row(label: &str, value: String, t: &rustdb_core::theme::ThemeTokens) -> impl IntoElement {
    h_flex()
        .justify_between()
        .items_center()
        .child(
            div()
                .text_size(px(crate::design::TS_CAPTION))
                .text_color(hex(t.fg_muted))
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .text_size(px(crate::design::TS_CAPTION))
                .text_color(hex(t.fg_primary))
                .child(SharedString::from(value)),
        )
}

/// 收集一个结果集；首次出现时追加结果标签。
fn ensure_result(tab: &mut SqlTab, store: &ResultStore, set_id: ResultSetId) -> bool {
    if tab.results.iter().any(|r| r.id == set_id) {
        return false;
    }
    let Some(meta) = store.meta(set_id) else {
        return false;
    };
    let rows = store.rows(set_id, 0, MAX_VIEW_ROWS);
    tab.results.push(ResultView {
        id: set_id,
        columns: meta.columns.to_vec(),
        rows,
        loaded: meta.loaded_rows,
        elapsed_ms: meta.elapsed_ms,
        truncated: meta.truncated || meta.loaded_rows > MAX_VIEW_ROWS as u64,
        affected: meta.affected,
        sort: None,
        widths: vec![None; meta.columns.len()],
        filters: Vec::new(),
        editable: None,
        changes: None,
        page: None,
    });
    true
}

/// F-92：diff 里的一行（`prefix` 为 `-` 或空；上下文行用弱化色）。
fn diff_line(line: &str, prefix: Option<&'static str>, color: u32) -> gpui::Div {
    h_flex()
        .items_start()
        .gap_1()
        .when_some(prefix, |row, prefix| {
            row.child(
                div()
                    .text_size(px(crate::design::TS_CAPTION))
                    .text_color(hex(color))
                    .child(prefix),
            )
        })
        .child(
            div()
                .text_size(px(crate::design::TS_CAPTION))
                .whitespace_normal()
                .text_color(hex(color))
                .child(SharedString::from(line.to_string())),
        )
}

/// F-94：名称 → 合法配置 id（小写字母、数字、连字符、下划线）。
fn ai_profile_id(name: &str) -> String {
    let mut id = String::new();
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | ' ' | '.') && !id.ends_with('-') && !id.is_empty() {
            id.push('-');
        }
    }
    let id = id.trim_matches('-').to_string();
    if id.is_empty() {
        "model".into()
    } else {
        id
    }
}

/// 模型面板里的一行：标签 + 输入框。
fn ai_field(
    t: &rustdb_core::theme::ThemeTokens,
    label: &str,
    input: &gpui::Entity<gpui_component::input::InputState>,
    width: gpui::Pixels,
) -> gpui::AnyElement {
    h_flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(64.))
                .text_size(px(crate::design::TS_CAPTION))
                .text_color(hex(t.fg_muted))
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .w(width)
                .child(gpui_component::input::Input::new(input).small()),
        )
        .into_any_element()
}

/// 构造 AI 引擎（F-94）：按激活配置生成；密钥优先取内存，其次读该配置的环境变量。
///
/// 没有任何配置时退回旧的环境变量直连（F-51），再退到离线脚本（明确标注）。
/// Key 从不写进配置文件（需求 12.3）。
fn build_agent(
    settings: &rustdb_core::AiSettings,
    keys: &HashMap<String, String>,
) -> std::sync::Arc<rustdb_core::AgentEngine> {
    use rustdb_core::agent::{AssistantTurn, CliProvider, ScriptedProvider, ToolCall};
    use rustdb_core::{AgentEngine, AuthKind, EngineLimits, OpenAiProvider};
    let active = settings.active_profile().cloned().or_else(|| env_profile());
    if let Some(profile) = active {
        let limits = EngineLimits {
            max_steps: profile.max_turns.max(1) as usize,
            retries: profile.retries,
        };
        let instruction = profile
            .global_instruction
            .clone()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let engine = if profile.kind == rustdb_core::AiKind::Cli {
            let command = profile.cli_command.clone().unwrap_or_default();
            AgentEngine::new(std::sync::Arc::new(CliProvider::new(
                command,
                std::time::Duration::from_secs(120),
            )))
        } else {
            let api_key = keys
                .get(&profile.id)
                .cloned()
                .or_else(|| std::env::var(profile.key_env()).ok())
                .unwrap_or_default();
            AgentEngine::new(std::sync::Arc::new(OpenAiProvider {
                base_url: profile.base_url.clone(),
                model: profile.model.clone(),
                api_key,
                temperature: profile.temperature,
                headers: profile.headers.clone(),
                auth_header: match profile.auth {
                    AuthKind::Header => profile.auth_header.clone(),
                    _ => None,
                },
                proxy: profile.proxy.clone(),
                timeout: std::time::Duration::from_secs(60),
            }))
        };
        return std::sync::Arc::new(
            engine
                .with_limits(limits)
                .with_global_instruction(instruction),
        );
    }
    std::sync::Arc::new(AgentEngine::new(std::sync::Arc::new(
        ScriptedProvider::new(vec![
            AssistantTurn::ToolCall(ToolCall::new(
                "search_objects",
                serde_json::json!({ "query": "" }),
            )),
            AssistantTurn::Answer("离线演示 · 点击配置模型".to_string()),
        ])
        .named("offline-demo"),
    )))
}

/// 旧版环境变量直连：只在没有任何命名配置时兜底（F-51 兼容）。
fn env_profile() -> Option<rustdb_core::AiProfile> {
    let base_url = std::env::var("RUSTDB_AI_BASE_URL").ok()?;
    // 这里只判断是否具备直连条件；真正取值在构建 provider 时按环境变量名读取。
    std::env::var("RUSTDB_AI_KEY").ok()?;
    let model = std::env::var("RUSTDB_AI_MODEL").ok()?;
    Some(rustdb_core::AiProfile {
        id: "env".into(),
        name: "环境变量直连".into(),
        base_url,
        model,
        api_key_ref: Some("RUSTDB_AI_KEY".into()),
        ..rustdb_core::AiProfile::default()
    })
}

/// SQL 导出用的表名：有 schema 时用 `schema.name`（导出模块会按方言引用各段）。
fn table_ref(path: &rustdb_db_api::ObjectPath) -> String {
    match path.ns.schema.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(schema) => format!("{schema}.{}", path.name),
        None => path.name.to_string(),
    }
}

/// 多格选区的结构化载荷：列/行都是选区的子集，主键下标重映射到子集列。
struct SelectionPayload {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    table: Option<String>,
    key_columns: Vec<usize>,
    missing_key_columns: bool,
    dimensions: (usize, usize),
}

/// 多格选区的子矩阵（按当前排序/筛选后的显示顺序）；单格与无选区返回 `None`。
fn tab_selection_typed(tab: &SqlTab, selection: Option<GridSelection>) -> Option<SelectionPayload> {
    let view = tab.results.get(tab.active_result)?;
    let range = selection_range(selection, view)?;
    if !range.is_multi_cell() {
        return None;
    }
    let order = view.ordered_row_indices();
    let columns: Vec<String> = view.columns[range.left..=range.right]
        .iter()
        .map(|column| column.name.to_string())
        .collect();
    let rows: Vec<Vec<Value>> = (range.top..=range.bottom)
        .filter_map(|position| {
            let row = *order.get(position)?;
            let values = view.rows.get(row)?.get(range.left..=range.right)?;
            Some(values.to_vec())
        })
        .collect();
    let (table, key_columns, missing_key_columns) = match view.editable.as_ref() {
        Some((path, keys)) => {
            let mapped: Vec<usize> = keys
                .iter()
                .filter(|key| **key >= range.left && **key <= range.right)
                .map(|key| *key - range.left)
                .collect();
            let missing_key_columns = !keys.is_empty() && mapped.len() != keys.len();
            (Some(table_ref(path)), mapped, missing_key_columns)
        }
        None => (None, Vec::new(), false),
    };
    Some(SelectionPayload {
        columns,
        rows,
        table,
        key_columns,
        missing_key_columns,
        dimensions: (range.rows(), range.cols()),
    })
}

/// 当前结果的导出负载：导出与复制共用，避免两套取数逻辑。
pub(crate) struct ExportPayload {
    dialect: rustdb_db_api::DbKind,
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    table: Option<String>,
    key_columns: Vec<usize>,
    /// 多格选区的（行, 列）数；整结果复制为 `None`。
    selection: Option<(usize, usize)>,
    /// SQL UPDATE 复制时，选区是否漏掉了主键列。
    missing_key_columns: bool,
}

impl ExportPayload {
    fn options(&self, format: ExportFormat) -> ExportOptions {
        ExportOptions {
            dialect: self.dialect,
            table: self.table.clone(),
            key_columns: self.key_columns.clone(),
            excel_bom: format == ExportFormat::Csv,
            ..ExportOptions::default()
        }
    }
}

#[cfg(test)]
mod tests_ai;
#[cfg(test)]
mod tests_exchange;
#[cfg(test)]
mod tests_grid;
#[cfg(test)]
mod tests_helpers;
#[cfg(test)]
mod tests_layout;
#[cfg(test)]
mod tests_panels;
