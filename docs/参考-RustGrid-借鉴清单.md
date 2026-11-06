# 外部参考评估：RustGrid（github.com/yisier/rustgrid）

> 评估日期：2026-10-22 ｜ 参照版本：0.1.0（Apache-2.0，Rust 2024，GPUI）
> 目的：只借**设计模式与选型结论**，不搬运代码/文案/视觉资产（需求文档 19.12 边界）。

## 0. 结论摘要

| 类别 | 条目 |
|---|---|
| **P1 建议采纳（进 M6/M7 计划）** | ① Oracle 换纯 Rust `oracledb`（去掉 Instant Client）② 服务端分页契约（让 `server_side_filter` 能力位落地）③ 导入/导出拆独立 crate（rust_xlsxwriter + calamine + encoding_rs）④ i18n 换 `rust-i18n` ⑤ SSH/SOCKS 隧道独立 crate（russh） |
| **P2 顺手借鉴（小改动）** | 连接池空闲回收（**本轮已落地 MySQL 20s**）、滚动条预留高度、选中格跟随滚动条、对象面板复用 + 统计异步、`*_sql()` 与执行成对、回调式流式导出、`sqlformat`、`self_update` |
| **明确不采纳** | 换 GPUI 引擎、70+ 方法的巨型 `Connection` trait、vendor 打补丁、`opt-level="z"`、自造 `.rgbak` 备份容器 |

---

## 1. 对方是什么（技术栈对照）

| 维度 | RustGrid | RustDb（我们） |
|---|---|---|
| UI | `gpui-fast 0.1.7` + `gpui-kit 0.7`（tree-sitter-sql） | `gpui-pre 0.3.9` + `gpui-component/gpui-base 0.8.0` |
| 驱动 | sqlx 0.9（MySQL 走 vendor 补丁）、tiberius、oracledb、ODBC | tokio-postgres、mysql_async、rusqlite、tiberius、`oracle`(ODPI-C) |
| 抽象 | 单 `Driver` + 单巨型 `Connection`（70+ 方法） | `DriverFactory` + `Session` + `MetadataProvider` + `Dialect`（三分） |
| 分层 | 13 个 crate（含 export/import/backup/tunnel/config） | 12 个 crate（无 export/import/tunnel） |
| 加密 | XChaCha20-Poly1305 落盘（无 keyring） | keyring（OS 凭据库） |
| i18n | `rust-i18n 4`（编译期 YAML，三语） | 手写词条表（14 条） |
| 许可 | Apache-2.0（可商用） | Apache-2.0 |

> 一句话：**功能面他们更宽（备份/用户权限/导入导出/ODBC/隧道），实现深度我们更深（AI Agent、变更集乐观并发、DML 预览、运行记录、网格虚拟化、性能门禁）**。

---

## 2. P1 建议采纳

### P1-1 Oracle 换纯 Rust `oracledb`，去掉 Instant Client

| 项 | 事实 |
|---|---|
| 现状痛点 | 我们用 `oracle 0.6`（ODPI-C）：**运行期需要 Oracle Instant Client**，连接非 `Sync` ⇒ `CancelStyle::None`，本机无实例 ⇒ live 契约测试一直没跑 |
| 他们怎么做 | `oracledb = "=26.0.0-beta.4"`（Oracle 官方维护，UPL-1.0 OR Apache-2.0），README 明确 "without any Oracle Client libraries"，支持 12/18/19/21/26、CLOB/BLOB/JSON/VECTOR、连接池、DRCP |
| **关键限制** | API 是**同步阻塞**的（`connect()` + `Cursor: Iterator`），docs.rs 顶层无异步/取消入口；仍是 beta（他们锁死精确版本是对的） |
| 落地建议 | M7 前做 2 天 spike：专用线程 + `spawn_blocking` 包一层 `Session`，验证 NUMBER/CLOB/TIMESTAMP 往返、取消（若确实没有 break 机制，就在能力位维持 `CancelStyle::None` 并写清文档） |
| 收益 | 用户不用装客户端、CI 能跑 Oracle 契约测试、删掉 odpic-sys 的 C 编译 |
| 风险 | beta API 漂移；同步模型与 async trait 隔离不干净会把阻塞扩散到 tokio worker |

### P1-2 服务端分页契约（把 `server_side_filter` 从空转位变成实现）

- **他们**：`Connection::fetch_page(database, table, PageRequest) -> TablePage`；`PageRequest.order_by` 是引擎无关描述，由驱动翻译为各自的 `ORDER BY`，注释要求「跨页顺序稳定」；`None` 显式表示 SQL `NULL`。
- **我们**：`Capabilities.server_side_filter` 恒为 `false`（本会话刚加的能力位），表数据走 `object_select_sql(kind, path, limit)`，**只有 limit 没有 page/order/where**；10 万行表只能拉前 N 行。
- **借鉴点**：`PageRequest { page, page_size, order_by: Vec<OrderBy>, filter: Option<FilterExpr> }` + `Session::fetch_page`，翻译在驱动内完成（**不违反 AC-20**「UI 无 `match DbKind`」）。
- **对应需求**：F-70（列筛选/排序/分页）、AC-29；顺带为 F-71（末行下移新增行）提供稳定分页锚点。

### P1-3 导入/导出拆独立 crate

- **他们**：`rustgrid-export`（`rust_xlsxwriter` / RFC 4180 CSV / 方言 SQL / 批量 500 行 + 2MB 上限）、`rustgrid-import`（`calamine` + `encoding_rs` + `dtparse`），接口是分页喂数据：
  ```rust
  TableWriter::create(path, table, columns, types, format, options, dialect)?
  writer.write_rows(&[Vec<CellValue>])?;   // 每页一次
  writer.finish()?;
  ```
- **我们**：只有 `ui/src/csv.rs`（135 行），无 XLSX、无导入、无 SQL 导出。
- **必须补的差异**：他们**没有 CSV 公式注入防护**（`=`/`+`/`-`/`@` 开头不加前缀）；我们 F-72 明确要求「公式防护只对危险文本加前缀，不改写真实负数」——这是我们要做得比参照实现更好的地方。
- **注意**：`rust_xlsxwriter` 没有流式写入器（整本工作簿在内存），正好对应 F-72「超出单工作表上限自动拆分」——按 `Worksheet` 分批，而不是一张表塞到底。
- **对应需求**：F-72、F-74、AC-30。

### P1-4 i18n 换 `rust-i18n`

- **他们**：`rust-i18n 4`（编译期 YAML codegen + `t!` 宏）+ `sys-locale`，英/中/俄三语，成本极低。
- **我们**：`ui/src/i18n.rs` 手写 `ENTRIES` 表（14 条），6000 行 UI 中硬编码中文仍在（AC-14 未达标）。
- **建议**：保留 `t(key)` 作为薄封装（减少调用点 diff），底层换 `rust-i18n`；`locales/zh-CN.yml` + `locales/en.yml`；先搬 shell、工具栏、右键菜单，再搬面板。
- **收益**：新增语言零代码；缺失 key 有 `log-miss-tr` 特性可查；与 M5 的「缺失回落 key + 告警」约定一致。

### P1-5 SSH / SOCKS 隧道独立 crate（F-91 / F-91a）

- **他们**：`rustgrid-tunnel` 单独一个 crate，服务所有「网络类」驱动（russh 0.63），README 强调 known_hosts 首连确认、多层隧道。
- **我们**：`db-tunnel` 已落地（`russh`，产出 `AsyncRead + AsyncWrite`）：known_hosts 校验先于凭据、多层建链与层号错误、密钥确认浮层、本地转发；F-91a 再补 SOCKS5 / HTTP CONNECT 与「代理 → SSH / SSH → 代理」组合顺序，局域网监听需显式开启。
- **合规点**：需求 19.6 要求「先完成 known_hosts 校验再发送任何凭据，host key 变更必须阻断」——顺序在 crate 内部保证，不靠调用方自觉；代理口令只走 `secret_ref` 或内存。

---

## 3. P2 顺手借鉴（小改动 / 后续版本）

| 借鉴点 | 他们的问题描述 | 我们的落点 |
|---|---|---|
| **连接池空闲回收** | 默认池不回收空闲连接，`wait_timeout` 短的服务器会复用死 socket | ✅ **本轮已落地**：`db-mysql` 设 `inactive_connection_ttl = 20s`（mysql_async 默认是 `0` = 永不回收，实测确认） |
| 横向滚动条**无滚动时也预留高度** | 否则竖向视口高度随横向滚动条有无而抖动 | 结果网格 / 对象树滚动区布局 |
| 拖拽滚动条时**选中格随行数平移** | 否则下一次滚轮会把网格拉回原选中行 | 结果区选择逻辑 |
| 对象面板**复用而非重建** | 点分类时重建整棵列表会 reflow、闪烁 | 连接树切库时复用节点，只换子项 |
| **先出列表、后出统计** | 慢的状态统计阻塞列表显示 | `MetaEvent::Loaded` 后异步补行数/大小 |
| `*_sql()` 与执行**成对** | UI 预览与执行必须同源 | 我们已有 `migration::plan`，但「清空/重命名/维护」还散在 `core::maintenance_sql`，建议收敛到 `Dialect` |
| 回调式流式导出 `stream_table_rows(on_row)` | 明确「不得缓冲整张表」 | F-76 整库导出 / F-77 备份 |
| `sqlformat 0.5` | 方言感知格式化不必自己写词法 | F-66（P2），纯 Rust 无 AST |
| `self_update 1.3` + GitHub Releases | 便携包替换 + 重启 | F-60（P2） |
| `rust-toolchain.toml` / `.gitattributes`(LF) / CI | 工具链漂移、脚本换行被 CRLF 污染 | 我们仓库目前无 CI、无工具链钉版本 |

---

## 4. 明确不采纳

| 不采纳 | 原因 |
|---|---|
| **换 GPUI 引擎到 `gpui-fast`** | 他们自己在 `Cargo.toml` 注释里写明：`gpui-kit` 仍无条件依赖 `gpui-pre`，两套引擎会**同时编译**、元素类型互不兼容。我们 `gpui-pre 0.3.9 + gpui-component 0.8.0` 已跑通 8318 行 UI 与布局测试，M6 内不做引擎迁移 |
| **70+ 方法的巨型 `Connection` trait** | 我们 `Session`/`MetadataProvider`/`Dialect` 三分更细，且 AC-21 要求驱动不依赖 core；巨型默认实现 trait 会让第七种方言的适配成本爆炸（AC-26 要求「新增方言只加 crate + 注册一行」） |
| **vendor 打补丁第三方驱动** | 他们为 MySQL <5.5 维护 `vendor/sqlx-mysql` + `RUSTGRID_PATCHES.md`；我们自研 mysql_async 驱动，按需在驱动内降级，避免长期维护负担 |
| **`opt-level="z"` + `lto=true`** | 他们优先二进制体积；我们 M-01 冷启动与网格帧率是硬门禁，保持 `opt-level=2 + lto="thin"` |
| **自造 `.rgbak` 备份容器** | F-76/F-77 只需「可回放的 SQL 文件」；自造容器增加格式兼容负担。将来要打包用 zip + manifest + zstd 即可 |

---

## 5. 功能面差距（他们更全，作为 M7/M8 备查）

| 能力 | RustGrid | 我们 | 对应需求 |
|---|---|---|---|
| 备份/恢复容器 | ✅ `.rgbak` | ⬜ F-76/F-77 规划中 | M7/M8 |
| 用户与权限管理 | ✅ | ⬜ | 未列入 v1.x |
| 例程/视图设计器 | ✅ | 🟡 只有 DDL 查看 | F-82（M6） |
| 导入导出向导 | ✅ Excel/CSV/TXT | 🟡 只有 CSV 导出 | F-72/F-74（M6） |
| ODBC 通用接入 | ✅ | ⬜ | F-106（P3） |
| 自动更新 | ✅ | ⬜ | F-60（P2） |
| 三语 i18n | ✅ | 🟡 骨架 | F-54 |
| **AI Agent（工具+确认卡+审计）** | ❌ | ✅ | M4 |
| **变更集 + 乐观并发** | ❌（逐行 DML） | ✅ | M3 |
| **外键跳转 / 按条件批量更新** | ✅ | ✅ M6（F-73：隐藏 COUNT 预览 + 键入表名确认，只借流程语义） | F-73 |
| **DML 预览** | ❌ | ✅ M6 增量 1 | F-63 |
| **运行记录 / 固定** | ❌ | ✅ M6 增量 1（无 UI） | F-65 |
| **网格虚拟化** | ❌（分页网格） | ✅ M5 已验证 | M-04 |

---

## 6. 建议落地顺序（与 M6 增量对齐）

1. **已做**：MySQL 池空闲回收 20s（本轮，`cargo check -p rustdb-db-mysql` 通过）。
2. **增量 2 顺手做**：导出公式注入防护（写 `core::export` 时一并）、滚动条两个细节、对象树统计异步。
3. **增量 3 / M7**：`db-tunnel` crate、`PageRequest` 分页契约、`core-export`/`core-import`、`rust-i18n` 迁移。
4. **并行 spike（不阻塞主线）**：Oracle → `oracledb` 可行性验证。

---

## 7. 事实核查与不确定项

| 项 | 状态 |
|---|---|
| `oracledb` 版本/许可 | ✅ crates.io：`26.0.0-beta.4`（2026-09-23），`UPL-1.0 OR Apache-2.0`，Oracle 官方仓库 |
| `oracledb` 免 Instant Client | ✅ README 原文 "without any Oracle Client libraries required" |
| `oracledb` 异步/取消 | ⚠️ docs.rs 顶层**无**异步与取消 API（`Cursor: Iterator` 同步）；`Connection` 是否有 break 需 spike 确认 |
| `mysql_async` 池默认 TTL | ✅ 本地源码确认 `DEFAULT_INACTIVE_CONNECTION_TTL = 0`（永不回收），`PoolOpts::with_inactive_connection_ttl` 可用 |
| 他们网格是否虚拟化 | ❌ 是分页网格（`fetch_page`），非连续虚拟滚动；我们不退回 |
| 许可与搬运边界 | ⚠️ 对方 Apache-2.0，但需求文档 19.12 要求只吸收通用功能语义、不复制代码/文案/视觉资产 ⇒ **只借模式，代码自写** |
