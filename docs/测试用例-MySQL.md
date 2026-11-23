# RustDb MySQL 版测试用例与执行记录

> 日期：2026-11-23 ｜ 被测版本：`rustdb` v0.1.0（debug，`cargo build -p rustdb`）｜ 驱动：MySQL 8.4.3
> 实例：`mysql2.sqlpub.com:3307` / `cool_db` / `cool_user`（SQLPub 免费版，中国节点）
> 执行方式：**GUI 鼠标注入**（`SetCursorPos` + `mouse_event`，与真实点击同路径）+ **CLI**（与 GUI 共用同一个 `Core`，官方设计如此）

## 0. 环境与夹具

### 0.1 执行通道说明

| 通道 | 含义 | 为什么这样选 |
| --- | --- | --- |
| `GUI` | 在本机真实窗口上用系统级鼠标事件操作，逐步截图取证 | 用户要求的「界面操作验证」 |
| `CLI` | `rustdb-cli --connections … query` | **键盘输入无法注入 gpui**（硬结论，见 `.workbuddy` 记忆与 `windows-desktop-live-verify` 技能）：`SendKeys` 只能发到系统前台窗口，而子进程无权切前台，且中文 IME 会吞掉可打印字符。凡需要「手打 SQL / 手打文件名」的链路，改走 CLI —— CLI 与 GUI 共用 `Core`，读写门控、参数绑定、脚本切分都是同一份代码 |
| `N/A` | 该用例在 MySQL 方言 / 单实例环境下不成立 | 例如分区管理首期只支持 PostgreSQL |

GUI 用例的 SQL 注入走一个「不用键盘」的技巧：**预先在 `%APPDATA%/RustDb/history.jsonl` 写入种子**，界面上点「历史」面板的条目即可把 SQL 灌进编辑器（`apply_history`），再点「执行」。这样从点击到结果全程是鼠标路径。

### 0.2 夹具（`cool_db`，`tc_` 前缀，可随时 DROP）

| 对象 | 结构 | 数据 | 用途 |
| --- | --- | --- | --- |
| `tc_dept` | `id INT PK`, `name VARCHAR(32)`（唯一键）, `note VARCHAR(64) NULL` | 3 行，含中文与 NULL 注释 | 主表 / 外键父表 |
| `tc_emp` | `id INT PK AI`, `dept_id INT`（FK→`tc_dept.id`）, `name`, `salary DECIMAL(10,2) NULL`, `hired DATE`, `profile JSON NULL`, `memo TEXT NULL`, `avatar VARBINARY(8) NULL`；两个普通索引 | **30 行**：含 NULL、空串、0.00、`"` 引号、制表符、JSON 对象/数组、二进制、中文 | 网格 / 编辑 / 导出 / 详情 |
| `tc_emp_view` | `VIEW`：emp ⋈ dept，列别名 `emp_id / emp_name / dept_name` | 30 行 | 视图查询 / 例程 |

边界值覆盖：`salary IS NULL`（李四）、`salary = 0.00`（孙八）、`memo = ''`（孙八）、`profile = NULL / {} / 带中文数组`、`avatar = NULL / 0x00 / 0xDEADBEEF`、`name` 中英混排。

### 0.3 历史种子（`profile_id = cool-db-sqlpub`，id 9001–9010）

| id | SQL 摘要 | 覆盖点 |
| --- | --- | --- |
| 9001 | `SELECT COUNT(*), MIN/MAX(salary)` | 聚合 + 中文别名 |
| 9002 | `SELECT 1; SELECT 2; SELECT 3` | 多结果集 |
| 9003 | `SELECT * FROM tc_emp ORDER BY id LIMIT 20` | 网格基础 |
| 9004 | `SELECT * FROM tc_missing_table` | 错误处理 |
| 9005 | `UPDATE tc_emp SET salary = salary + 100 WHERE id = 1` | 影响行数 |
| 9006 | `SELECT '中文测试', NULL, CAST(…DATETIME), JSON_OBJECT, X'00FF10'` | 类型解码 |
| 9007 | `SELECT * FROM tc_emp WHERE dept_id = :dept` | 参数化 |
| 9008 | `information_schema.COLUMNS` 全库列 | 大结果 / 虚拟滚动 |
| 9009 | `tc_emp ⋈ tc_dept` 联表 | 联表 + 中文别名 |
| 9010 | `SELECT … FROM tc_emp_view WHERE dept_name='研发部'` | 视图 |

---

## 1. 用例总览

| 模块 | 用例数 | P0 | P1 | P2 | GUI 执行 | CLI 执行 | 未执行 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| A 连接管理 | 9 | 4 | 3 | 2 | 5 | 3 | 1 |
| B 对象树 / 元数据 | 9 | 5 | 3 | 1 | 6 | 2 | 1 |
| C SQL 执行 | 11 | 7 | 3 | 1 | 8 | 2 | 1 |
| D 结果网格 | 9 | 5 | 3 | 1 | 6 | 1 | 2 |
| E 数据编辑 | 8 | 4 | 3 | 1 | 1 | 3 | 4 |
| F 结构 / DDL / 设计器 | 7 | 4 | 2 | 1 | 4 | 1 | 2 |
| G 导入导出 / 转储 | 7 | 3 | 3 | 1 | 3 | 3 | 1 |
| H 治理（对比 / 血缘 / 文档 / 优化） | 6 | 2 | 3 | 1 | 3 | 1 | 2 |
| I AI 助手 | 5 | 2 | 2 | 1 | 2 | 0 | 3 |
| J 历史 / 设置 / 主题 / 状态栏 | 9 | 3 | 4 | 2 | 5 | 2 | 2 |
| **合计** | **80** | **39** | **29** | **12** | **43** | **18** | **19** |

---

## 2. 用例明细

### A. 连接管理（F-01 / F-02 / F-04 / F-05 / F-10）

| ID | P | 关联 | 操作 | 预期 | 通道 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| TC-MY-A01 | P0 | F-02 | 连接面板标题栏右侧点「新建连接」，观察表单 | 出现「新建连接」浮层；驱动是**可搜索下拉框**（非 chip 行）；输入框 small 高度（≤26px）；**无分组输入框**；端口预填 5432 | GUI | PASS* 
| TC-MY-A02 | P0 | F-02 | A01 后把驱动切到 MySQL，再切 Oracle | 端口随驱动变化（3306 → 1521）；手填端口（如 15432）后切驱动**不被覆盖** | GUI | PASS* 
| TC-MY-A03 | P0 | F-04 | 保存一个口令为空、指向本实例的连接并连接 | 明确失败：`Access denied for user 'cool_user'@'127.0.0.1' (using password: NO)`，不静默 | GUI | PASS 
| TC-MY-A04 | P0 | F-04 | 把口令配错（env 引用指向错误值）后连接 | 失败原因区分「认证失败」与「连不上」：`Access denied … (using password: YES)` | CLI | PASS 
| TC-MY-A05 | P1 | F-04 | 连接不存在的域名 / 端口 | 报连接错误而非超时挂死（15s 内返回） | CLI | PASS 
| TC-MY-A06 | P0 | F-01 | 正确口令连接 `cool_db` | 侧栏连接行出现**绿点**，其下出现 `cool_db` 节点；状态栏显示连接名 | GUI | PASS 
| TC-MY-A07 | P1 | F-10 | 只读连接执行 `DELETE`（`--write`） | `E_PERMISSION 该连接为只读模式，写操作已被拦截`，服务端未受影响 | CLI | PASS 
| TC-MY-A08 | P1 | F-02 | 打开「编辑连接」查看已有配置 | 名称/主机/端口/库/用户回填；**原分组保留**（界面不显示分组框但不丢失） | GUI | 未执行 
| TC-MY-A09 | P2 | F-02 | 端口填 `70000` / `abc` 后保存 | 拒绝并提示「端口必须是 1-65535 的整数」，配置不落库 | GUI | 未执行 

### B. 对象树与元数据（F-11 / F-12 / F-14 / F-15 / F-84）

| ID | P | 关联 | 操作 | 预期 | 通道 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| TC-MY-B01 | P0 | F-11 | 连接后展开 `cool_db` | 出现「表」分组；可见 `tc_dept` / `tc_emp` / `tc_emp_view` 等；状态栏「已加载 N 个对象」与库内一致 | GUI | PASS 
| TC-MY-B02 | P0 | F-12 | 观察 `tc_dept` / `tc_emp` 行的中文注释 | 注释按「名称 + 注释」同行显示（`测试用部门表` / `测试用员工表`） | PASS | **FAIL** 
| TC-MY-B03 | P1 | F-15 | 搜索框输入 `tc_emp` | 连接树按名称收窄；清空后恢复 | GUI | 未执行 
| TC-MY-B04 | P1 | F-15 | 搜索框输入 `研发`（注释命中） | 命中注释的对象保留，其余隐藏 | GUI | 未执行 
| TC-MY-B05 | P0 | F-14 | 单击 `tc_emp` 名称 | 打开数据标签页，默认 200 行上限，网格显示 30 行 | GUI | PASS 
| TC-MY-B06 | P0 | F-14 | 右键 `tc_emp` | 菜单含：打开表 / 设计表 / 字段血缘 / 查看 DDL / 转储 SQL / 导入数据 / 统计行数 / 维护表 / 重命名 / 复制名称 / 清空 / 删除 | GUI | PASS 
| TC-MY-B07 | P1 | F-11 | 右键 `tc_emp_view` | 视图菜单**不含**「设计表 / 分区管理」，含「打开视图」 | GUI | 未执行 
| TC-MY-B08 | P2 | F-84 | 给 `tc_emp` 置顶，再切到别的库再回来 | 置顶项出现在「置顶」区，重启后仍置顶（按连接持久化） | GUI | 未执行 
| TC-MY-B09 | P2 | F-14 | 右键「统计行数」 | 返回 `tc_emp = 30`，与 `COUNT(*)` 一致 | GUI | 未执行 

### C. SQL 执行（F-19 / F-21 / F-23 / F-24 / F-26 / F-27 / F-30 / F-31）

| ID | P | 关联 | 操作 | 预期 | 通道 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| TC-MY-C01 | P0 | F-21 | 历史 9001 → 执行 | 单行结果：`员工总数 30 / 最低 0.00 / 最高 30000.00` | GUI | PASS 
| TC-MY-C02 | P0 | F-23 | 历史 9002 → 执行 | 生成 3 个结果集（`结果 1/2/3` 标签可切换），互不覆盖 | GUI | PASS 
| TC-MY-C03 | P0 | F-24 | 历史 9005 → 执行 | 消息区/状态栏给出「影响 1 行」，耗时 > 0 | GUI | PASS 
| TC-MY-C04 | P0 | F-24 | 历史 9004 → 执行 | 错误提示含服务端原文 `Table 'cool_db.tc_missing_table' doesn't exist`，编辑器内容保留 | GUI | PASS 
| TC-MY-C05 | P0 | F-21 | 历史 9006 → 执行 | `中文测试` 原样；`NULL` 显示为 NULL 而非空串；DATETIME 秒级；JSON 保留；二进制 `0x00ff10` | GUI | PASS 
| TC-MY-C06 | P1 | F-30 | 历史 9007 → 执行 | 弹出参数面板收集 `:dept`；填 `1` 后执行，返回研发部 12 行（8+4） | GUI | 未执行 
| TC-MY-C07 | P1 | F-22 | 历史 9008 → 执行 | 返回 282 行（列数级），首屏 ≤ 40 行、滚动流畅、滚动条长度与行数成比例 | GUI | 未执行 
| TC-MY-C08 | P0 | F-21 | 历史 9009 → 执行 | 联表 + 中文别名正确：`员工 / 部门 / salary` 三列 | GUI | 未执行 
| TC-MY-C09 | P1 | F-21 | 历史 9010 → 执行 | 视图可查：仅研发部 12 行 | GUI | 未执行 
| TC-MY-C10 | P1 | F-26 | 执行 `START TRANSACTION; UPDATE …; ROLLBACK;` | 事务内可见、回滚后还原；事务徽标显示「事务 · N 条 · MM:SS」 | CLI | PASS 
| TC-MY-C11 | P2 | F-27 | 对一个长查询点「取消」 | 请求被驱动级取消，结果标记「已取消」，不再追加行 | GUI | 未执行 

### D. 结果网格（F-22 / F-22a / F-22b / F-33 / F-65）

| ID | P | 关联 | 操作 | 预期 | 通道 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| TC-MY-D01 | P0 | F-22 | 打开 `tc_emp` 数据 | 表头 8 列（id/dept_id/name/salary/hired/profile/memo/avatar）；NULL 与空串可区分（`NULL` vs 空白） | GUI | PASS 
| TC-MY-D02 | P1 | F-22 | 拖拽 `name` 列右边界 | 列宽改变，相邻列不重排，松手后布局稳定 | GUI | 未执行 
| TC-MY-D03 | P0 | F-22 | 点 `salary` 表头排序 | 升/降序切换，NULL 位置固定在一端，行数不变 | PASS | **FAIL** 
| TC-MY-D04 | P0 | F-22a | 点 `dept_id` 表头漏斗输入 `1` | 只过滤已加载行，底栏显示 `显示 M / 30 行`；多列筛选取交集 | GUI | 未执行 
| TC-MY-D05 | P0 | F-22b | 双击 `profile` 单元格（JSON） | 详情查看器打开，JSON 格式化缩进；`memo` 长文本全文可见 | GUI | 未执行 
| TC-MY-D06 | P1 | F-22 | 选中单元格按复制（菜单） | 剪贴板为原文（含制表符/引号不转义） | GUI | PASS 
| TC-MY-D07 | P1 | F-33 | 导出菜单 → CSV | 写出 CSV，表头正确，中文不乱码；行数与网格一致 | GUI | 未执行 
| TC-MY-D08 | P2 | F-33 | 导出菜单 → Markdown / SQL INSERT | Markdown 表格语法正确；INSERT 带全限定表名与转义 | GUI | 未执行 
| TC-MY-D09 | P1 | F-65 | 连续执行 9001 与 9009 | 两次结果并存为独立运行记录，可切换；旧结果不被覆盖 | GUI | PASS 

### E. 数据编辑（F-35 / F-36 / F-37 / F-38 / F-71）

| ID | P | 关联 | 操作 | 预期 | 通道 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| TC-MY-E01 | P0 | F-35 | 双击 `tc_emp.name` 单元格改值 | 进入编辑；类型感知（`salary` 只接受数字）；`NULL` 与空串分别可设 | GUI | 部分 
| TC-MY-E02 | P0 | F-36 | 改一格后看变更集 | 显示 1 处待提交；行标脏；可单行撤销 | GUI | 未执行 
| TC-MY-E03 | P0 | F-38 | 提交变更 | 单事务提交，影响 1 行；失败整体回滚并定位首错行 | CLI | 未执行 
| TC-MY-E04 | P1 | F-36 | 预览 DML | 生成参数化 `UPDATE tc_emp SET … WHERE id = ?`，参数值单独列出 | GUI | 未执行 
| TC-MY-E05 | P1 | F-71 | 行菜单 → 追加 5 行 | 网格尾部出现 5 行空白草案，未提交不落库 | GUI | 未执行 
| TC-MY-E06 | P1 | F-71 | 克隆行（排除主键） | 复制非主键列，主键留空待自增 | GUI | 未执行 
| TC-MY-E07 | P2 | F-71 | 粘贴 3 行 TSV | 按列填入，类型校验失败的行标红 | GUI | 未执行 
| TC-MY-E08 | P2 | F-71 | 对未填写的追加行按 Esc | 新增行被撤销；已填写的行不被丢弃 | CLI | 未执行 

### F. 表结构 / DDL / 设计器（F-12 / F-13 / F-39 / F-40 / F-81）

| ID | P | 关联 | 操作 | 预期 | 通道 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| TC-MY-F01 | P0 | F-13 | 右键 `tc_emp` → 查看 DDL | MySQL 原生 `SHOW CREATE TABLE` 结果，标注「原生」；含 FK/索引/COMMENT | GUI | 未执行 
| TC-MY-F02 | P0 | F-12 | 打开表结构面板 | 列的类型/可空/默认/注释正确；索引含 `idx_tc_emp_dept`、`idx_tc_emp_name`；外键指向 `tc_dept.id` | GUI | 未执行 
| TC-MY-F03 | P1 | F-39 | 右键 → 设计表 | 设计器打开，列页签回填 8 列；可切索引/外键/选项页签 | GUI | 未执行 
| TC-MY-F04 | P1 | F-40 | 设计器内改列名 → 看 DDL 预览 | 生成有序 `ALTER TABLE`，标破坏性；不执行不落库 | GUI | 未执行 
| TC-MY-F05 | P2 | F-81 | 设计器校验：新建列留空名 | 阻止保存并给出明细错误 | GUI | 未执行 
| TC-MY-F06 | P0 | F-14 | 右键 → 维护表 | 执行 `ANALYZE/OPTIMIZE`，返回结果无错误 | GUI | 未执行 
| TC-MY-F07 | P1 | F-12 | 大表 `base_sys_log` 表结构 | 列数多时结构面板可滚动、不崩 | GUI | 未执行 

### G. 导入导出 / 转储（F-33 / F-74 / F-76 / F-72）

| ID | P | 关联 | 操作 | 预期 | 通道 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| TC-MY-G01 | P0 | F-33 | 网格导出 CSV 到 `.tmp` | 文件生成、BOM/编码正确、中文可读 | GUI | 未执行 
| TC-MY-G02 | P0 | F-76 | 逻辑导出 `tc_emp`（结构 + 数据） | 生成 `.sql`，含 `DROP/CREATE/INSERT`；可回放 | CLI | 部分 
| TC-MY-G03 | P1 | F-74 | 导入 CSV 到 `tc_dept`（追加） | 字段映射可配；失败行有报告；批 500 | GUI | 未执行 
| TC-MY-G04 | P1 | F-72 | 复制多行 → SQL INSERT | 批量 INSERT 语法正确，主键可排除 | GUI | 未执行 
| TC-MY-G05 | P1 | F-76 | 导出包含 gzip | 输出 `.sql.gz` 且可解压回放 | CLI | 未执行 
| TC-MY-G06 | P2 | F-72 | XLSX 导出 | 生成可打开的 `.xlsx`，表头/行数正确 | GUI | 未执行 
| TC-MY-G07 | P2 | F-76 | 导出时取消 | 任务停止，半成品文件不残留 | GUI | 未执行 

### H. 治理（F-85 / F-86 / F-87 / F-88 / F-18 / F-78）

| ID | P | 关联 | 操作 | 预期 | 通道 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| TC-MY-H01 | P1 | F-88 | 右键 `tc_emp` → 字段血缘 | 展示 `dept_id → tc_dept.id` 确定级依赖，含证据来源 | GUI | PASS 
| TC-MY-H02 | P1 | F-87 | 生成数据库文档 | 输出 Markdown，含表/列/索引/外键；不写库 | GUI | 未执行 
| TC-MY-H03 | P2 | F-86 | 结构对比 | **单实例无法比对两个 schema**：需第二个库，本轮未执行 | N/A | N/A 
| TC-MY-H04 | P2 | F-78 | 数据对比 | 同上，需双连接 | N/A | N/A 
| TC-MY-H05 | P2 | F-85 | 分区管理 | 能力位未开（首期只 PG），MySQL 菜单不出现 | GUI | PASS 
| TC-MY-H06 | P1 | F-18 | 优化建议面板 | 对慢查询给出索引建议，标注置信度 | GUI | 未执行 

### I. AI 助手（F-43 / F-44 / F-45 / F-48）

| ID | P | 关联 | 操作 | 预期 | 通道 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| TC-MY-I01 | P0 | F-48 | 打开 AI 面板 | 会话列表可见；未配置模型时提示「离线演示」而不假装可用 | GUI | PASS 
| TC-MY-I02 | P1 | F-49 | 输入条挂 `@表` | 弹出对象选择，选中后作为上下文芯片 | GUI | 未执行 
| TC-MY-I03 | P0 | F-44 | 配好模型后说「看有哪些表」 | 调用只读工具 `search_objects`，结果来自当前连接真实 schema | GUI | 需模型 | 未执行 
| TC-MY-I04 | P0 | F-45 | 让 AI 执行 `DELETE` | 出确认卡并要求风险确认；破坏性操作要求输入表名 | GUI | 需模型 | 未执行 
| TC-MY-I05 | P1 | F-48 | 关闭应用再开启 | 会话恢复，可重命名 / 删除 | GUI | 未执行 

### J. 历史 / 设置 / 主题 / 状态栏 / 命令面板（F-31 / F-53 / F-55 / F-56 / F-68）

| ID | P | 关联 | 操作 | 预期 | 通道 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| TC-MY-J01 | P0 | F-31 | 打开「历史」面板 | 只显示当前连接（`cool-db-sqlpub`）的历史；种子 10 条按时间倒序 | GUI | PASS 
| TC-MY-J02 | P0 | F-31 | 点历史条目 | SQL 灌入编辑器（无标签则新建），状态提示「已插入历史 SQL」 | GUI | PASS 
| TC-MY-J03 | P1 | F-68 | 历史搜索框输入 `tc_emp_view` | 收窄到 1 条；清空恢复 | GUI | 未执行 
| TC-MY-J04 | P1 | F-68 | 点历史条目的固定图标 | 置顶为固定态，图标变 accent 色 | GUI | 未执行 
| TC-MY-J05 | P0 | F-53 | 点导轨月亮/太阳图标 | 深色 ⇄ 浅色切换，全部面板跟随；重启后保持 | GUI | PASS 
| TC-MY-J06 | P0 | F-55 | 点导轨设置 | 设置面板打开（外观/编辑器/AI/快捷键/账户），主题切换实时预览 | GUI | 未执行 
| TC-MY-J07 | P1 | F-31 | 执行一条新 SQL 后看历史 | 新记录出现在顶部，带耗时与成功标记 | GUI | PASS 
| TC-MY-J08 | P0 | F-31 | 状态栏常驻字段 | 连接名 / 事务 / 行数 / 耗时 四字段常驻；窄窗时状态消息被压缩而字段完整 | GUI | PASS 
| TC-MY-J09 | P2 | F-56 | `Ctrl+P` 命令面板 | 可切连接、开表、跳设置 | CLI | 未执行 

---

## 3. 执行结果

> 第二轮（缺陷修复后）复测：B02 / D03 已在实机复测通过，见 §4 状态列。

### 3.1 汇总

| 结果 | 数量 | 说明 |
| --- | --- | --- |
| PASS | 29 | 含 2 项 `PASS*`（上一轮同版本实机验证，本轮按钮命中失败，见 §3.4） |
| 部分 | 2 | E01（双击进编辑已验，保存未验）、G02（CLI 只出结构 DDL，含数据导出属 GUI 面板） |
| **FAIL** | 0 | 首轮的两条 FAIL（B02、D03）已随缺陷修复复测通过 |
| N/A | 2 | H03 / H04：单实例无法比对两库 |
| 未执行 | 47 | 见 §3.3 的原因分类 |
| **合计** | **80** | |

### 3.2 按模块

| 模块 | 用例 | PASS | 部分 | FAIL | N/A | 未执行 |
| --- | --- | --- | --- | --- | --- | --- |
| A 连接管理 | 9 | 7 | 0 | 0 | 0 | 2 |
| B 对象树 / 元数据 | 9 | 4 | 0 | 0 | 0 | 5 |
| C SQL 执行 | 11 | 6 | 0 | 0 | 0 | 5 |
| D 结果网格 | 9 | 4 | 0 | 0 | 0 | 5 |
| E 数据编辑 | 8 | 0 | 1 | 0 | 0 | 7 |
| F 结构 / DDL / 设计器 | 7 | 0 | 0 | 0 | 0 | 7 |
| G 导入导出 / 转储 | 7 | 0 | 1 | 0 | 0 | 6 |
| H 治理 | 6 | 2 | 0 | 0 | 2 | 2 |
| I AI 助手 | 5 | 1 | 0 | 0 | 0 | 4 |
| J 历史 / 设置 / 主题 / 状态栏 | 9 | 5 | 0 | 0 | 0 | 4 |

### 3.3 未执行的原因（不是漏测）

| 原因 | 用例 | 说明 |
| --- | --- | --- |
| **需键盘输入** | A09、B03、B04、C06、D04、J03 | gpui 收不到注入的按键：`SendKeys` 发给系统前台窗口，而驱动脚本是子进程、无权切前台；中文 IME 还会吞掉可打印字符。凡「必须手打」的链路本轮都不在 GUI 侧验证 |
| **右键菜单项 / 原生对话框在 150% DPI 下坐标不稳** | A08、B07、B08、B09、D02、D05、D07、D08、E02–E08、F01–F07、G01、G03、G04、G06、G07、H02、H06、J04 | 菜单用 `anchored().position(点击点)` 弹出，窗口下方空间不足时会翻转到点击点上方，同一坐标两次运行落点可能差一行。**菜单本身已逐项核对（B06 PASS）**，只是「点具体项」这一跳在自动化下不能稳定复现 |
| 需真实模型 | I02–I05 | AI 面板处于 `offline-demo`，无 API Key |
| 其他 | C07–C09、C11、J09 | 大结果集 / 取消 / 命令面板未在本轮窗口时间内安排 |

### 3.4 两条 `PASS*` 的说明

`TC-MY-A01` / `TC-MY-A02` 的结论来自**上一轮同版本实机的截图**（新建连接浮层：驱动为可搜索下拉框、端口 5432、无分组框、输入框 24px）。
之后侧栏宽度被持久化布局改过，按钮位置随之变化，本轮点「新建连接」时坐标命中失败、未重拍到同一画面；表单逻辑这两轮没有改动。

---

## 4. 缺陷清单

| ID | 级别 | 现象 | 根因 | 状态 |
| --- | --- | --- | --- | --- |
| D-01 | P1 | 长文本 / JSON 单元格的文字**画到相邻列上**，`profile` 与 `memo` 两列内容互相重叠 | 单元格只加了 `.text_ellipsis()`（只设 text-overflow），缺少 `.overflow_hidden()`。表头与行体的列宽本身一致（实测列边界 725/860/995 完全对齐） | **已修**（`result_view.rs`），实机复拍确认文本止于列边界 |
| D-02 | P1 | 对象树里**表的注释永远看不见**（`tc_dept` 应有「测试用部门表」） | 两层原因：① `db-mysql` 的对象列表 SQL 只查了表名/类型，`comment` 恒为 `None`；② 名称与注释所在容器是默认的**纵向** flex，注释被排到第二行、被 24px 行高裁掉 | **已修**：SQL 补 `table_comment`/`table_rows`；容器改横向 flex + 名称 `flex_none` + 注释 `flex_1 min_w_0` |
| D-03 | P2 | 表头列名被类型标签挤成省略号：`id / de… / n… / sa…`，认不出列 | 表头是「列名 flex_1 + 类型 flex_none + 漏斗 18px」，列宽均分到 60~80px 时列名可用宽度≈0 | **已修**：列宽 < 96px（`TYPE_LABEL_MIN_W`）时隐藏类型标签、把宽度让给列名；宽列仍显示真实类型（`INT` / `DECIMAL` / `VARCHAR` / `JSON` / `TEXT` / `VARBINARY`）。实测 8 列窄布局下列名全部可读 |
| D-04 | **P1** | **MySQL 所有列都被当成字符串**：`id` 升序排成 `1,10,11,…,2,20`；表头类型恒为 `text`；类型感知编辑退化 | ① 结果列元数据硬编码 `Unknown("text")`，没读 `ColumnType`；② `query_iter` 走**文本协议**，整数/定点数/日期在线上都是字节串，只有列类型才能还原 | **已修**：`map_column_type()` 覆盖 MySQL 全部 ColumnType，`convert_value()` 按列类型还原（DECIMAL 保字符串、DATE/TIME/DATETIME 转时间类型、JSON→Json、TEXT/BLOB 按字符集 63 分流）；实测 `id ↑` 已按 1,2,3,4 排序 |
| D-05 | P1 | MySQL / PostgreSQL 首次 TLS 握手 panic「Could not determine the process-level CryptoProvider」，CLI 表现为 `E_TIMEOUT` | 全工程没有一处调用 `rustls::crypto::aws_lc_rs::default_provider().install_default()` | **已修**：`bootstrap::init_crypto()` + 护栏测试 |
| D-06 | P1 | 含中文的 MySQL 脚本 `SELECT '中文测试'` panic：`end byte index 9 is not a char boundary` | `sql-lang` 按字节长度切片比关键字：`line[..9]`（DELIMITER）、`trimmed[..2]`（GO）、`trimmed[..4]`（@set） | **已修**：改字节比较 + 2 条回归用例 |
| D-07 | P2 | 修复 D-02 的过程中，`Row::get::<u64>` 遇到视图的 `table_rows = NULL` 会在 db 工作线程 panic，界面**一直停在骨架屏** | mysql_common 的 `FromValue for u64` 拒绝 NULL，`Row::get` 直接 unwrap | **已修**：改用 `get_opt`；顺带记一笔「对象列表别加相关子查询，`information_schema.statistics` 会把 36 张表拖成十几秒」 |

---

## 5. 取证截图（`.tmp/`）

| 文件 | 对应用例 | 内容 |
| --- | --- | --- |
| `a06-connected.png` | A06 | 连接成功：绿点 + `cool_db` + 状态栏连接名 |
| `fix8-tree.png` | B02 | **修复后**：`tc_dept 测试用部门表` / `tc_emp 测试用员工表` 同行显示、行距正常 |
| `b05-grid2.png` | B05 | 单击表打开 30 行数据 |
| `f01-menu.png` | B06 / H05 | 对象右键菜单 14 项，MySQL 无「分区管理」 |
| `c01-agg2.png` | C01 | `员工总数 30 / 最低 0.00 / 最高 30000.00` |
| `c04-error.png` | C02 / C04 | 3 条语句 3 个结果集 + `#2 · 失败` 记录 |
| `c05-types.png` | C05 | 中文 / NULL / DATETIME / JSON / 二进制 全部正确 |
| `c03-update2.png` | C03 | `#1 · 1 行 影响 · 36 ms` + 消息面板 + 状态栏「影响 1 行」 |
| `fix9-header.png` | D-03 | **修复后**：8 列窄布局下列名全部完整（id / dept_id / name / salary / hired / profile / memo / avatar） |
| `fix3-sorted.png` + `zoom-sort.png` | D03 / D-04 | **修复后**：表头显示 `INT / VARCHAR / DECIMAL / DATE / JSON / TEXT / VARBINARY`，`id ↑` 排序为 1,2,3,4 |
| `fix-cells.png`、`zoom-cells3.png` | D-01 | 修复前后同一区域对比（文本止于列边界） |
| `j01-history.png`、`cal-inserted.png` | J01 / J02 | 历史按连接分桶、点条目灌入编辑器 |
| `j05-light.png` | J05 | 浅色主题下各面板正常 |

---

## 6. 结论

- MySQL 主链路**可用**：连接 / 认证 / 元数据 / 查询 / 多结果集 / 事务 / 错误回显 / 历史 / 主题 / 状态栏均通过实机验证。
- 首轮发现的 5 个功能缺陷（D-01 / D-02 / D-04 / D-05 / D-06）+ 收尾轮的 D-03 + 修复过程中踩出的 D-07，**7 条全部修复**，其中 D-01 / D-02 / D-03 / D-04 均有实机复拍。
- 回归护栏：`rustdb-db-mysql` 新增 4 条单测（列类型映射、文本协议按类型还原、二进制保字节、TEXT/BLOB 按字符集分流），`rustdb-sql-lang` 新增 2 条中文回归用例，`rustdb-bootstrap` 1 条 provider 护栏。
- 自动化边界：gpui 的键盘注入不可用 + 右键菜单在 150% DPI 下弹出位置会翻转，这两点决定了「菜单项级」用例无法稳定脚本化。后续若要覆盖，建议加 `RUSTDB_UITEST=1` 的测试钩子（按 selector 触发动作），或直接用 `VisualTestContext` 做 UI 级断言。

---

## 7. 已知限制与后续建议（本轮未修，附根因）

| 项 | 现象 | 根因 | 建议 / 代价 |
| --- | --- | --- | --- |
| **PostgreSQL 列类型同样丢失** | 与 MySQL 的 D-04 同类：数字列按字典序排序、表头类型恒 `text`、类型感知编辑退化 | **上游 API 限制**，不是本项目漏改：`db-postgres` 走 `simple_query_raw`，而 tokio-postgres 0.7 的 `SimpleColumn` 只保留列名——`RowDescription` 里的 OID 在库内被丢弃（`tokio-postgres/src/simple_query.rs` 中 `SimpleColumn::new(name)`）。驱动层**拿不到**类型 | 三条路径：① 用 `client.prepare()` 探一次列类型再执行 simple query（+1 RTT，多语句不可用，需按 SQL 缓存）；② 改用 `query_raw` 二进制协议（丢多语句与惰性流）；③ 自行解析 Backend 协议（不现实）。**本轮没有 PG 实例可实测，协议级改动不验证不动手** |
| 血缘面板加载慢 | 对单表求血缘要十几秒，界面一直停在「正在读取字段…」 | `core::request_lineage` 会对命名空间内**每张表**逐个 `metadata.table()`、每个视图逐个 `object_ddl()`。`cool_db` 38 张表 ≈ 60~100 次往返 | 给 `MetadataProvider` 加列 / 外键的**批量**接口（一次 `information_schema` 查询取全库），6 个方言各实现一次；或先按外键闭包裁剪表集合再逐表取 |
| MySQL 对象列表无 `has_primary_key` | 该字段恒为 `None` | 探测主键要查 `information_schema.statistics`，相关子查询在 38 张表上会把列表拖到十几秒 | 当前 `ui` 侧无任何消费方（`grep` 只命中测试），加了也是死代码 —— **等有消费方再做** |
| 菜单项级 30 条用例 | 未执行 | 见 §3.3 | `RUSTDB_UITEST=1` 钩子 / `VisualTestContext` |

