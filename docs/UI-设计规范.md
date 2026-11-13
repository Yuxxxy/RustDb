# RustDb UI 设计规范 v1.0

> **适用范围**：`ui/` 全部视图、组件与主题映射；`core/src/theme.rs` 的 Token 定义；`core/src/layout.rs` 的界面偏好持久化。
> **代码基线**：`e78ffa4`（2026-11-12 核对，与 `origin/main` 一致）。
> **与需求文档的关系**：`docs/RustDb-需求开发文档.md` 第 10 章定义视觉方向与基础 Token，本文件是它的**可执行细化与验收口径**。
> 基础 Token 的十六进制值仍以需求文档 10.3 为准（**禁止改值**）；字号、对比度、组件状态以本文件为准。

---

## 0. 优先级与验收原则

| 级别 | 含义 | 默认要求 |
| --- | --- | --- |
| P0 | 影响可读性、可访问性或状态可辨识度，用户每次操作都会遇到 | 本轮必须落地 |
| P1 | 组件状态缺失、响应式与交互一致性 | 本轮尽量落地，未完成项显式登记 |
| P2 | 信息架构、文案与长尾一致性 | 排期推进 |

三条硬规则：

1. **所有指标必须可复跑**：口径、命令、目标数字写进本文第 9 节，禁止「看起来更舒服」这类结论。
2. **设计系统先行**：先定 Token / TypeScale / 组件规格，再改界面；禁止在业务代码里写死颜色或字号字面量。
3. **改动必须回归**：`cargo fmt --all -- --check`、`cargo check -p rustdb-ui --lib`、`cargo test -p rustdb-ui --lib` 全绿才算完成。

---

## 1. 设计原则

1. **Token 唯一来源**：颜色一律经 `theme::current(cx)` → `hex(t.xxx)`；`ui/src` 内禁止十六进制字面量。
2. **基础层冻结**：`border_subtle` / `bg_*` / `fg_*` 等基础值禁止改动（与 RustShell 保持一致）。对比度不足时**新增扩展 Token**，不覆盖旧值。
3. **可访问性内建**：正文 ≥ 4.5:1，非文本 UI 边界 ≥ 3:1（WCAG 2.1 AA / 1.4.11），键盘可达、焦点可见、命中区 ≥ 24×24。
4. **8pt 栅格**：间距取 4 / 8 / 12 / 16 / 24 / 32，圆角只保留 6 / 8 / 10 三档，禁止 `px(3.)` 这类零散值用于布局。
5. **状态必须可辨**：hover / active / selected / focus / disabled 五态齐备，且相邻态之间可肉眼区分。

---

## 2. Token 体系

### 2.1 三层结构

| 层 | 位置 | 规则 |
| --- | --- | --- |
| 基础层 | `core/src/theme.rs::ThemeTokens` 现有字段 | 冻结，禁止改值；Dark / Light 必须成对 |
| 扩展层 | 同结构体新增字段（带 `#[serde(default)]` 语义） | 本轮新增，用于修复对比度与语义前景 |
| 组件层 | `ui/src/theme.rs::apply_theme_mode` | 把上游 Token 映射到 `gpui-component::ThemeColors`，业务代码不直接碰 |

### 2.2 基础层（冻结值，摘自需求文档 10.3）

| Token | Dark | Light | 用途 |
| --- | --- | --- | --- |
| `bg_base` | `#0D1117` | `#FFFFFF` | 窗口 / 工作区底色 |
| `bg_elevated` | `#161B22` | `#F6F8FA` | 侧栏 / 卡片 / 标题栏 |
| `bg_overlay` | `#1C2128` | `#FFFFFF` | 浮层 / 菜单 |
| `border_subtle` | `#30363D` | `#D0D7DE` | 装饰性 1px 分隔线（不承载结构信息） |
| `fg_primary` | `#E6EDF3` | `#1F2328` | 主文字 |
| `fg_secondary` | `#B6C2CF` | `#57606A` | 次级文字 |
| `fg_muted` | `#8B949E` | `#656D76` | 时间戳 / 计数 / 占位 |
| `accent` | `#58A6FF` | `#0969DA` | 激活 / 链接 / 主按钮 |
| `danger` | `#F85149` | `#CF222E` | 错误 / 破坏性操作 |

### 2.3 扩展层（v1.0 新增）

| Token | Dark | Light | 用途 | 实测对比度 |
| --- | --- | --- | --- | --- |
| `border_strong` | `#6E7681` | `#6E7781` | 结构性边界：卡片、工具栏、输入框、对话框 | Dark 4.12（on `bg_base`）/ 3.77（on `bg_elevated`）；Light 4.55 / 4.27 |
| `on_danger_resolved` | `#0D1117` | `#FFFFFF` | 危险按钮 / 危险徽标前景文字 | Dark 5.65；Light 5.36 |

`grid_line` 不新增：网格单元格线属于**装饰性分隔**，沿用 `border_subtle`，不承担「识别组件」职责；结构性边界才必须用 `border_strong`。
选区描边直接复用 `accent`（Dark 7.49 / Light 5.19）。

### 2.4 对比度基线（WCAG 2.1 相对亮度公式，2026-11-12 实测）

| 组合 | 现状 | 目标 | 处置 |
| --- | --- | --- | --- |
| 正文 `fg_primary` / `bg_base` | 16.02 / 15.80 | ≥ 4.5 | 已达标 |
| `fg_muted` / `bg_overlay` | 5.26 / 5.25 | ≥ 4.5 | 已达标（但小字需提升字号，见 §3） |
| 危险按钮白字 / `danger` | **3.35** / 5.36 | ≥ 4.5 | 改用 `on_danger_resolved` |
| 1px 边界 / 底色 | **1.55** / **1.45** | ≥ 3:1（结构性） | 改用 `border_strong` |
| 选区高亮 `accent@18%` / 底色 | **1.33** / 1.29 | 叠加 1px accent 锚点 | 见 §6 |
| 网格表头 / 数据行 | **1.09** / 1.06 | 行分隔线 + 整行 hover | 见 §5 网格 |
| 斑马纹 / 行底色 | **1.03** / 1.04 | 废弃斑马纹 | 见 §5 网格 |

---

## 3. 字体排版 TypeScale

全局 `theme.font_size = px(15.)` 为组件库 rem 基准；业务代码只允许调用下表的常量，**禁止 `text_size(px(字面量))`**。

| 级别 | 常量 | 字号 | 字重 | 行高 | 用途 |
| --- | --- | --- | --- | --- | --- |
| Caption | `TS_CAPTION` | 12px | 400 | 16px | 徽标、页脚、时间戳、辅助说明（**最小字号**） |
| Body | `TS_BODY` | 13px | 400 | 20px | 正文、表格单元格、菜单项、按钮标签 |
| Base | `TS_BASE` | 15px | 400/500 | 22px | 面板标题、区块标题 |
| Title | `TS_TITLE` | 18px | 500 | 26px | 空态标题、关于页、对话框标题 |

旧值迁移映射（机械替换，10 种 → 3 级）：

| 旧字号 | 8.5 / 9 / 9.5 / 10 / 10.5 / 11 / 11.5 | 12 / 13 | 15 |
| --- | --- | --- | --- |
| 新级别 | `TS_CAPTION` (12) | `TS_BODY` (13) | `TS_BASE` (15) |

`TS_TITLE` (18) 预留给对话框 / 空态主标题，本轮暂无使用点；迁移采用保守映射，
原 13px / 15px 不变，只抬升 <12px 的文字，避免大范围布局抖动。

验收：字号种类 ≤ 4、最小 ≥ 12px、`text_size(px(数字))` 字面量 = 0、150% DPI 下无截断。
`CELL_FONT_SIZE` 必须与网格实际字号同步（用于自适应列宽测量），否则列宽偏窄导致截断。

---

## 4. 间距、尺寸与圆角

| 类别 | 规范 |
| --- | --- |
| 间距 | 4 / 8 / 12 / 16 / 24 / 32（8pt 栅格），统一走 `p_2` / `p_3` / `gap_2` / `gap_3` |
| 圆角 | 6（输入 / 气泡 / 小控件）、8（卡片）、10（对话框）；禁止 3、4、12 等零散值 |
| 命中区 | **≥ 24×24**（WCAG 2.2 Target Size Minimum），推荐 28×28；图标按钮必须有 tooltip |
| 标题栏 / 状态栏 | 28px / 22px（与需求文档 10.1 一致） |
| 工具栏 / 标签栏 | 34px / 30px |
| 网格 | 行高三档：紧凑 22 / 标准 25 / 宽松 30，表头 27px，列宽 72–640 |

---

## 5. 组件规范

| 组件 | 规格 | 状态要求 |
| --- | --- | --- |
| 主按钮 | accent 底 + `on_accent` 字，高 28px，圆角 6，一屏最多一个 | hover / active / focus / disabled |
| 次按钮 | 透明底 + `border_strong` 描边 | 同上 |
| 危险按钮 | `danger` 底 + `on_danger_resolved` 字，仅用于删除 / 清空 / 回滚 | 同上 |
| 输入框 | 高 28px，默认 `border_strong` 描边；聚焦时描边换 **accent 2px**，不画外环（`focus_ring = false` 保留） | focus 对比 ≥ 3:1 |
| 树节点 | 行高 26px，缩进 12px/层，展开箭头 14px；加载时骨架 3 行 | hover / selected / focus |
| 标签页 | 高 30px，选中底部 2px accent，可关闭，溢出滚动 | — |
| 对话框 | `min(90vw, 740) × min(85vh, 560)`，圆角 10，遮罩 35% 黑，**Esc 可关**，焦点约束在弹层内 | — |
| 空态 | 图标 32px（`fg_muted`）+ 13px 一句说明 + **一个主操作** + 12px 快捷键提示；不使用插画 | 主操作可键盘触发 |
| 加载态 | 骨架屏（列表 3 行 / 网格 8 行），**> 400ms 才显示**；不用转圈遮罩；保留旧内容并降透明度（`0.55`） | 400ms 内 / 旧内容 / 无旧内容，见 §11.2 |
| 错误态 | 图标 + 一句话 + 「详情」折叠 + 「复制」 | — |
| 结果网格 | 表头 27px + 列类型角标；行分隔线 `border_subtle`；**废弃斑马纹**；整行 hover `bg_overlay`；选区 `accent@18%` + accent 高对比描边 | selected / hover / dirty / NULL |
| diff 行 | `diff_add` / `diff_del` **@12% 底色**；禁止实色铺满（会压低文字对比度）。2px 语义色左条列为 P2 增强 | — |
| AI 气泡 | 角色标签 + `border_strong` 1px 边 + 12–13px 正文；用户/助手不能只靠背景色区分 | 运行中发送按钮禁用 |
| 状态栏 | 常驻连接 / 事务 / 行数 / 耗时 / 只读标记，字号 ≥ 12px，超长省略 + tooltip | — |

---

## 6. 交互状态规范

| 状态 | 视觉 | 量化要求 |
| --- | --- | --- |
| hover | `bg_overlay` 或 `accent@8%` | 与 default 可区分，但**必须弱于 selected** |
| active / pressed | `bg_elevated` / `accent@18%` | — |
| selected | `accent@18%` 填充 + accent 高对比描边 | 描边对比 ≥ 3:1 |
| focus | accent 2px 描边，`focus_ring = false` | 对比 ≥ 3:1，键盘走查 100% 可见 |
| disabled | 透明度 0.6 + `cursor-not-allowed` | 不响应点击 |

选区填充不能靠无限提高不透明度解决：`accent` 叠深底会抬亮背景、压低前景文字对比度，正确做法是**淡填充 + 高对比描边锚点**。

---

## 7. 可访问性

- **WCAG 2.1 AA**：普通文本 ≥ 4.5:1，大文本与 UI 组件边界 ≥ 3:1；不依赖颜色单独传达状态（dirty 单元格同时有文字/图标标记）。
- **键盘**：所有交互可 Tab 到达；Esc 关闭链覆盖全部浮层（含设置）；对话框内焦点不逃逸。
- **命中区**：≥ 24×24；纯图标按钮必须有 tooltip 与可访问名称。
- **文本缩放 / DPI**：字号走 Token，容器用 `flex_1` / `min_w_0` + 省略，150% 缩放下不截断。
- **动效**：尊重 `prefers-reduced-motion`，过渡 150ms，不做装饰性循环动画。

---

## 8. 响应式与布局

| 断点 | 行为 |
| --- | --- |
| 窗口最小 1024 × 640 | 低于 1024 宽时侧栏自动让位，不改用户折叠开关 |
| 侧栏 180–520（默认 260） | 按面板分别记忆，落 `layout.json` |
| AI 栏 280–480（默认 360） | 可收起 |
| 工具栏 < 900px | 折叠为「更多 ▾」，禁止溢出裁切 |
| 对话框 | 用 `window.viewport_size()` 动态夹取，禁止无条件固定 `w(px(740))`；740 是 Settings 组件不裁剪内容的最小宽度，视口不足时才按 90% 缩小 |
| 编辑器 / 结果区 | 9px 分隔条（1px `border_strong` 线 + 上下各 4px 命中带）可拖拽；高度 96–480（默认 176）落 `layout.json`；窗口不足时优先压缩编辑器，保证结果区 ≥120px；hover 淡底、拖拽 accent 指示条；`Ctrl+Alt+↑/↓` 每档 16px、`Ctrl+Alt+0` 或双击复位 |

---

## 9. 验收清单（可复跑）

在 `D:/rust/RustDb` 下执行：

```bash
# 1) 字号与像素字面量扫描
python -c "import re,glob,collections; f=glob.glob('ui/src/**/*.rs',recursive=True); s=''.join(open(x,encoding='utf8').read() for x in f); c=collections.Counter(map(float,re.findall(r'text_size\(px\(([0-9.]+)\)\)',s))); print('kinds',len(c),'calls',sum(c.values()),'lt12',sum(v for k,v in c.items() if k<12))"

# 2) 颜色字面量扫描（应为空；主题文件除外）
python -c "import re,glob; f=glob.glob('ui/src/**/*.rs',recursive=True); print([x for x in f if re.search(r'0x[0-9A-Fa-f]{6}',open(x,encoding='utf8').read())])"

# 3) 编译、格式与 UI 用例
cargo fmt --all -- --check
cargo check -p rustdb-ui --lib
cargo test -p rustdb-ui --lib
```

| 项 | 改动前基线 | 当前 / 目标 |
| --- | --- | --- |
| `text_size` 字面量调用 | 369 处 / 10 种 / 357 处 < 12px | **0 处**（全部走 `TS_*`）/ 3 级在用 / < 12px = 0 |
| `px(...)` 硬编码常量 | 950 处 / 98 种值 | 587 处 / 89 种值（P2：布局类收敛到 8pt 栅格） |
| `ui/src` 十六进制色值 | 0 | 0 |
| 结构性边界对比度 | 1.45–1.55 | 卡片 / 工具栏 / 输入框 / 状态栏 / 对话框改 `border_strong`：3.77–4.55 |
| 危险按钮文字对比度 | 3.35（Dark） | 5.65（Dark）/ 5.36（Light） |
| ghost 悬停文字对比度 | 1.00（Dark，与卡片背景同色，悬停时标签整个消失） | 深色改 `fg_primary`：裸底 13.7–16.0 / accent@50% 复合底 5.5–5.8；浅色 `on_accent` 对实心 accent 5.19 |
| 图标按钮 tooltip | 9/9 纯图标按钮已覆盖；133 个 Button 链仅 38 个有 tooltip | 纯图标 100%；**文字按钮 tooltip 与快捷键提示仍待补** |
| 命中区 | 18×18 / 20×24 存在 | 设置关闭 / 导出下拉 / 工具栏「更多 ▾」均 ≥24px；其余零散小控件 P2 排查 |
| 工具栏溢出 | < 900px 时次要动作被裁切 | 折叠为「更多 ▾」；1024–1920 × AI 开/关 12 组配置**零裁切**（`toolbar_never_clips_in_supported_window_range`） |
| UI 用例 | 135 passed | **155 passed / 0 failed** |

对比度脚本口径：WCAG 2.1 相对亮度 `L = 0.2126R + 0.7152G + 0.0722B`（sRGB 线性化），
`CR = (L1 + 0.05) / (L2 + 0.05)`；透明度按 sRGB 通道线性混合后再计算。

---

## 10. 落地状态

> 本节在每轮改动结束时回写，未落地项必须显式登记，不得留空或写「已完成」。

| 编号 | 项 | 状态 | 落地证据 / 未完成原因 |
| --- | --- | --- | --- |
| P0-2 | 扩展 Token | ✅ 已落地 | `core/theme.rs` 新增 `border_strong` / `on_danger_resolved`；`ui/theme.rs` 映射；卡片、工具栏、状态栏、对话框接线 |
| P0-1 | TypeScale + 护栏 | ✅ 已落地 | `ui/design.rs` 4 级 Token；369 处字面量全替换；`no_hardcoded_text_size` 扫描用例 |
| P0-3 | 危险按钮文字 | ✅ 已落地 | Dark 5.65:1 / Light 5.36:1，生产徽标同步 |
| P0-4 | 焦点态 | ✅ 已落地 | 保留 `focus_ring = false`；核对 gpui-component 0.8 `focus_style`：聚焦时 `border_color` 自动换 `theme.ring`(accent)。本轮把**默认**描边由 1.5:1 提到 ≥3.77:1，两态均可辨 |
| P0-5 | 网格 | ✅ 已落地 | `GridDensity` 三档（22/25/30）落 `layout.json`；diff @12%；废弃斑马纹 + 整行 hover；选区 18% + accent 描边；列头类型角标 |
| P1 | 统一空态 | ✅ 已落地 | `empty_state` 复用 4 处：SQL 工作区 / 无结果 / DDL 成功 / AI 空会话 |
| P1 | 加载骨架 | ✅ 已落地 | `LoadingPhase` 统一 400ms 阈值：连接树 3 行骨架、结果区 8 行骨架、例程依赖 96×12 骨架条全部延迟显示；长查询保留旧结果并降透明度 `0.55`；复用 80ms 轮询，不新建定时器 |
| P1 | 设置响应式 + Esc | ✅ 已落地 | `min(90vw, 740) × min(85vh, 560)`、圆角 10、内容 `flex_1`、边框 `border_strong`、接入 Esc 链 |
| P1 | 命中区 | ✅ 已落地 | 设置关闭 18×18→28×28；导出下拉换 `Button` 并显式 `with_size(px(24.))`（`Size::Small` 实测 22.5px，低于 WCAG 2.2 的 24px 下限）；「更多 ▾」用 `.small().h(px(24.))`（带 label 时 `with_size` 只改内边距） |
| P1 | 工具栏 < 900px 折叠 | ✅ 已落地 | `TOOLBAR_COMPACT_W = 900`；判定用 `on_prepaint` **实测**工作区宽（侧栏被 flex 压缩且不回写 layout，视口估算偏小 79.9px，会误折叠 vw≈1130–1209）；只读预览 / 打开 / 保存收进「更多 ▾」，复用 `MenuEntry` + 全局按键观察者；库选择器省略号吸收余量、折叠态去掉两条装饰分隔线。**< 1024px 仍会裁切**，见 §11.4 |
| P1 | 导出触发器 | ✅ 已落地 | 手写 `div + on_mouse_down` 换 `Button`：Idle / Hover / Focus / Open 四态由组件提供，`tooltip` 说明可用格式，`track_focus` + 全局按键观察者打通 Enter / ↑ / ↓ / Home / End / Esc；菜单高亮项与 hover 同底色 |
| P1 | AI 气泡 | 🟡 部分 | 角色标签 + `border_strong` 描边 + 运行中禁用发送；**长会话虚拟化未做** |
| P1 | 编辑器可拖拽分隔 | ✅ 已落地 | `core/layout.rs` 新增 `editor_height` + 6 个常量与 `clamp_editor_height`（`#[serde(default)]` 兼容旧配置）；`workspace.rs` 单一 setter 接线拖拽 / 键盘 / 双击 / 设置页复位；拖拽期遮罩防误选字；`layout.json` 只在松手时写一次 |
| P2 | 命令面板 / 状态栏常驻字段 / 设置搜索 / i18n 全量 | ❌ 未做 | 仅把标题栏品牌名回 15px；其余排期 |

**本轮改动文件**：`core/{theme,layout,lib}.rs`、`ui/{design,theme,lib}.rs`、
`ui/main_view.rs` 及 29 个 `ui/src/**` 视图文件（字号收敛）、
`ui/src/main_view/{result_view,settings_panel,workspace,status_bar,sidebar,ai_column,routine_panel}.rs`、
`docs/UI-设计规范.md`。
**P1-① 追加**：`ui/src/main_view/{workspace,render,lifecycle,settings_panel,tests_layout}.rs`、
`ui/src/main_view.rs`、`core/src/{layout,lib}.rs`（单文件多处改动走 `.tmp/p1_editor_split.py` 字节级精确补丁）。
**P1-② 追加**：`ui/src/{design,main_view}.rs`、
`ui/src/main_view/{lifecycle,sidebar,result_view,result_tabs,routine_panel,events,tests_layout}.rs`
（走 `.tmp/p1_loading.py`）。
**P1-③ 追加**：`ui/src/main_view.rs`（`MenuEntry.highlight` + `export_focus` / `export_hover`）、
`ui/src/main_view/{workspace,grid,lifecycle,tests_layout}.rs`（走 `.tmp/p1_export_button.py`、
`.tmp/p1_export_key2.py`）。
**P1-④ 追加**：`core/src/{layout,lib}.rs`（`TOOLBAR_COMPACT_W`）、
`ui/src/main_view.rs`（`toolbar_menu` / `toolbar_focus` / `toolbar_hover` / `measured_workspace_w`、
3 个 `MenuAction`、共用 `menu_hover_move`）、
`ui/src/main_view/{workspace,render,lifecycle,tests_layout}.rs`、
`ui/src/theme.rs`（ghost 悬停文字色深色分支 + 对比度护栏，见 §11.4 回归记录五）。

**提交边界**：P0 全项 + P1-① = `c206c1a`；P1-② = `6c6e52c`；P1-③ + P1-④ + ghost 悬停修复仍在工作区、未提交。

---

**文档维护**：UI Designer（像素君）· 2026-11-12 · 每次视觉改动后同步更新 §9 基线与 §10 状态。

> **实测修正（2026-11-12）**：初版把宽度夹到 640px，导致 4 个设置面板用例失败
> （`settings-exchange` 等入口被裁剪）。`gpui-component` 0.8 的 `Settings` 在 640px 下
> 内容区不足以容纳工具项，因此把上限定回 740px，仅按 `90vw / 85vh` 在视口不足时缩小。
> 回归命令见 §9；这一约束优先于「越窄越好」的直觉。

---

## 11. 本轮验证结果（2026-11-12）

| 验证项 | 命令 | 结果 |
| --- | --- | --- |
| 格式 | `cargo fmt --all -- --check` | 通过 |
| 单 crate 编译 | `cargo check -p rustdb-ui --lib` | 通过 |
| 全工作区编译 | `cargo check --workspace --all-targets` | 通过（11 个 crate） |
| UI 单元测试 | `cargo test -p rustdb-ui --lib` | **155 passed / 0 failed**（148 + P1-④ 6 条折叠 / 阈值 / 键盘 / 裁切用例 + 1 条 ghost 悬停对比度护栏） |
| core 单元测试 | `cargo test -p rustdb-core --lib` | **334 passed / 0 failed**（330 + P1-① 4 条高度夹取用例） |
| 设计系统扫描 | `python .tmp/ui_verify.py` | 字号字面量 0；`ui/src` 十六进制色值 0；6 组对比度全部 PASS |
| 真机视觉 | PrintWindow 截图 + 像素直方图（`.tmp/p4_diag.ps1` / `p4_final.ps1`） | 悬停标签可见：idle / 悬停亮像素 6373 / 6373（修复前 6373 / 6122）；恢复真实用户配置后 1200×820 启动、折叠态无裁切 |

**实测验收数字**：`text_size` 字面量 0，Token 调用 367 处
（`TS_CAPTION` 355 / `TS_BODY` 10 / `TS_BASE` 2）；`border_strong` 接线 8 处、
`on_danger_resolved` 4 处；`empty_state` 4 处、`skeleton` 调用点 2 处；`px()` 数字常量 587 处 / 89 种值
（较基线 950 / 98 下降，剩余为尺寸与图标，P2 继续收敛）。
修复后对比度：Dark 边界 4.12 / 3.77，Light 4.55 / 4.27，危险按钮 5.65 / 5.36，全部达标。

**回归记录**：首次把设置面板宽度夹到 640px 后，4 个用例失败
（`optimize_panel_opens_from_toolbar`、`optimize_generation_fills_result`、
`exchange_panel_exports_and_imports_with_dedupe`、`exchange_panel_guards_plaintext_export_and_escape`）。
根因是 `gpui-component` 0.8 的 `Settings` 在 640px 下内容区不足、工具项被裁剪，
`debug_bounds("settings-exchange")` 返回 `None`；改为 740px 上限后 137/137 全绿。

**P1-① 回归（2026-11-12）**：把拖拽用例从「按下 → 移动一帧 → 松手」补强成
「移动两帧，第二帧在遮罩出现之后」，结果 `139 passed / 1 failed`（176 → 240，期望 256）。
根因：拖拽遮罩 `.absolute().inset_0().occlude()` 是最上层 hitbox，会截断 body 的
`on_mouse_move`，真实拖拽在第一帧后卡住。修复：遮罩自身转发同一套
`on_mouse_move` / `on_mouse_up` / `on_mouse_up_out`，复跑 140/140 全绿。
**护栏**：凡「拖拽中覆盖全屏遮罩」的交互，遮罩必须自带事件转发，并至少测两帧移动。

**下一轮起点**（按优先级）：~~① 编辑器 / 结果区拖拽分隔 + 高度持久化~~（已落地，见 §11.1，含遮罩事件转发护栏）；
~~② 加载骨架的 400ms 延迟与旧内容保留~~（已落地，见 §11.2）；~~③ 导出下拉换 `Button`~~（已落地，见 §11.3）；
~~④ 工具栏 < 900px 折叠~~（已落地，见 §11.4，含「折叠判定必须用实测宽度」护栏）；
⑤ AI 长会话虚拟化；⑥ P2 命令面板 / 状态栏常驻字段 / 设置搜索 / i18n 全量。
**当前起点 = ⑤**（AI 长会话虚拟化）。

### §11.1 P1-① 编辑器分隔条（本轮追加）

| 口径 | 值 |
| --- | --- |
| 默认 / 最小 / 最大编辑器高度 | 176 / 96 / 480 px |
| 分隔条命中带 | 9px（1px 线 + 上下各 4px），与 `gpui-base` `ResizeHandle`（1+4×2）同口径 |
| 结果区保底 | 120px；`max = min(480, available_h − 9 − 120)`，`available_h = viewport_h − 129 − 可选面板估算` |
| 落盘时机 | 拖拽**只在 mouse up 写一次**；键盘微调 / 双击复位 / 设置页复位各写一次，避免逐帧 I/O |
| 状态 | `LayoutSettings::editor_height` 为唯一数据源，所有入口走 `set_editor_height` |
| 键盘 | 全局 `observe_keystrokes`：`Ctrl+Alt+↑/↓` ±16px、`Ctrl+Alt+0` 复位；不要求分隔条获得焦点 |
| 三态 | Idle 1px `border_strong`；Hover 9px 带 `bg_overlay`；Dragging 线转 `accent` + 44×3 指示条 |
| 遮罩 | 仅 `active`（真的拖动后）才盖透明 `occlude()` 层；单击/双击时不得遮挡第二次 mouse_down；**遮罩自身必须转发 `on_mouse_move` / `on_mouse_up(_out)`**，否则只能收到第一帧（实测回归，见上） |

**实测约束**：`available_h` 用视口高度扣固定外壳的保守估算（129px + 参数面板 34px + 导入面板 40px），
不依赖组件内部测量；夹取只影响编辑器上限，结果区不足时由 `RESULT_HEIGHT_MIN` 兜底。
未做：分隔条自身 tooltip（`div` 无 `Button::tooltip`），快捷键提示改由「设置 → 外观 → 工作区」承载，P2 补统一 tooltip 组件。

**验收用例**（`ui/src/main_view/tests_layout.rs`）：
- `editor_splitter_drag_persists_height`：两帧拖拽 + 遮罩出现/消失 + `layout.json` 持久化；
- `editor_splitter_double_click_resets`：双击复位且不残留遮罩；
- `editor_height_keyboard_nudge_and_reset`：`Ctrl+Alt+↑/↓/0` 夹取与复位。

core 侧 4 条：默认值 / 夹取与结果区保底 / 序列化与旧配置回落 / 键盘步长边界。
本轮全量：`cargo fmt --all -- --check`、`cargo check --workspace --all-targets`、
`cargo test -p rustdb-ui --lib`（当时 140，P1-② 后 147）、`cargo test -p rustdb-core --lib`（334）、
`python .tmp/ui_verify.py`（6 组对比度全 PASS）均通过。

### §11.2 P1-② 加载骨架延迟与旧内容保留（本轮追加）

| 口径 | 值 |
| --- | --- |
| 阈值 | `LOADING_DELAY = 400ms`；短于阈值的请求**完全不产生视觉变化**，避免快响应闪骨架 |
| 三态 | `Normal`（<400ms，同尺寸透明占位）/ `Stale`（超时 + 有旧内容）/ `Skeleton`（超时 + 无旧内容） |
| 旧内容 | 保留并降到 `STALE_CONTENT_OPACITY = 0.55`；不清空、不盖转圈遮罩、不禁用交互 |
| 骨架规格 | 连接树 3×18px；结果网格 8 行、行高取 `grid_density.row_height()`；例程依赖 96×12px 条 |
| 驱动 | `LoadingPhase` 由既有 80ms `poll()` 推进，跨阈值时返回 `true` 补一帧；**不新建定时器 / 不逐帧起异步任务** |
| 状态源 | `object_loading_phase: HashMap<String, LoadingPhase>`、`query_loading_phase: HashMap<u64, LoadingPhase>`、`RoutineDeps.phase`，均由 `loading` / `tab.running` 派生，结束即回收 |
| 可测性 | `begin_at(now, has_stale_content)` 可注入时钟，精确验证 399ms / 400ms 边界；`result_opacity()` 抽成可测方法 |

**设计取舍**：连接树在刷新期间仍允许点开别的表，所以旧对象行保持全不透明（降透明度会误示为禁用）；
「保留旧内容 + 降透明度」落在**结果区**——那才是内容会被整体替换的区域。
骨架在 400ms 内用同尺寸 `opacity(0)` 占位，保证阈值前后布局零跳动。

**验收用例**：`design.rs` 4 条（400ms 边界 / Stale 不铺骨架 / finish 复位 / 常量口径）；
`tests_layout.rs` 3 条（`loading_phase_sync_follows_loading_state`、`result_skeleton_waits_for_400ms`、
`result_opacity_dims_stale_content`）。
调试选择器：`object-loading-skeleton` / `result-loading-skeleton` / `routine-deps-skeleton`，
仅在真正可见时挂载，便于回归断言。

**回归记录**：新增 `RoutineDeps.phase` 后 `cargo check -p rustdb-ui --lib` 通过，但
`cargo test -p rustdb-ui --lib` 报 `E0063 missing field phase`（测试模块里的结构体字面量）。
再次印证 `cargo check` 不编译 `#[cfg(test)]`，新增结构体字段必须跑 test 复核。
### §11.3 P1-③ 导出下拉换 Button + 键盘可达（本轮追加）

| 口径 | 值 |
| --- | --- |
| 触发器 | `Button::new("export-format-btn").with_size(px(24.)).ghost().icon(ChevronDown)`，外层 `div` 仍挂 `export-format` 调试选择器供回归定位 |
| 命中区 | **24×24**。`Size::Small` 实测只有 22.5px（1.5rem × 15px 根字号），低于 WCAG 2.2 SC 2.5.8；28px 又比旁边的「导出」高出一截，故取 24 |
| 四态 | Idle（ghost 透明）/ Hover（`bg_overlay`）/ Focus（`theme.ring = accent` 描边）/ Open（`Selectable::open(true)` 保持选中底色）—— 全部由组件提供，不再自绘 |
| tooltip | 「更多导出格式：CSV / TSV / JSON / Markdown / Excel / SQL」，把菜单内容前置到悬停提示里 |
| 打开 | 鼠标：`on_click` → `export_menu = Some(event.position())`；键盘：焦点在触发器上按 Enter / Space / ↓ |
| 导航 | ↑ / ↓ 循环、Home / End 跳首尾；高亮项 `MenuEntry.highlight` 置位后与 hover 同底色（`bg_overlay`），序号按「跨分组展平」与 `export_menu_actions()` 一一对应 |
| 执行 | Enter / Space → `commit_export_hover` → `dispatch_menu`，与鼠标点击走同一条导出链路 |
| 收起 | Esc（`handle_escape` 新分支）、点遮罩、执行动作（`dispatch_menu` 统一清 `export_menu` + `export_hover`） |
| 锚点 | 键盘打开时无指针坐标，锚到 `(viewport_w − 24, WORKSPACE_CHROME_H)`，配合菜单自身 `offset(-190, 6)` 落在右上角工具栏下方 |

**设计取舍：键盘不用元素级 `on_key_down`。** 触发器嵌在 `h_resizable` 面板里，元素级按键依赖渲染帧的
焦点节点映射（`window.focus(handle)` 只写焦点 id，不保证节点已注册），鼠标点开浮层时这条映射并不成立，
方向键会静默丢失。改由既有的全局 `observe_keystrokes`（Esc 早就走这条路）统一处理，判定条件为
「菜单开着」或「焦点 == `export_focus`」，因此不会抢走编辑器的 Enter。

**回归记录一（键盘激活不生效）**：首次把键盘逻辑挂在 `Button` 的 `on_click`（`ClickEvent::Keyboard`）上，
用例失败在「Enter 应打开导出格式菜单」。定位过程：焦点断言为 `true`，但 `on_key_down` 一次都没被调用；
换成 `window.focus_next` 做 Tab 遍历 80 次也到不了触发器。根因是 `div.rs` 的 Enter→Click 合成依赖
「key down 记录的 focus 代际 == key up 时的代际」，而这两次事件之间的重绘会重建 `pending_keyboard_down`，
断言条件失效。结论：**浮层的键盘闭环不要依赖组件的键盘 ClickEvent 合成，走全局观察者更可靠。**

**回归记录二（命中区反而变小）**：换 `Button` 时沿用 `.small()`，实测 `debug_bounds("export-format")`
为 **22.5×22.5**——`size_6()` 是 `1.5rem`，根字号 15px ⇒ 22.5。断言「≥24」当场失败，暴露的是真实
可访问性回退而非断言过严。改为 `with_size(px(24.))` 后 24×24 通过。
**护栏**：该断言（正方形 + 24~34 高度 + 宽度 ≥24）留在 `export_format_menu_lists_all_formats` 里，
以后任何人调小触发器都会被测试拦住。

**验收用例**（`ui/src/main_view/tests_layout.rs`）：
- `export_format_menu_lists_all_formats`：鼠标点击开菜单、11 个格式项齐全、命中区尺寸护栏、执行后关闭；
- `export_format_menu_is_keyboard_reachable`：Enter 打开（默认高亮第 1 项）→ ↓ 两次到 JSON → Enter 执行
  → 再开 → Esc 收起。

### §11.4 P1-④ 工具栏 < 900px 折叠为「更多 ▾」（本轮追加）

| 口径 | 值 |
| --- | --- |
| 阈值 | `core/layout.rs::TOOLBAR_COMPACT_W = 900`，判定对象是**工具栏可用宽**，不是窗口宽 |
| 宽度来源 | `on_prepaint` 实测工作区根容器宽 → `measured_workspace_w`；视口估算仅用于首帧兜底 |
| 测量层 | 工作区根 `v_flex().size_full()`——宽度由卡片决定、不随工具栏内容变化，因此测量与折叠判定不会互相振荡 |
| 补帧 | 只在折叠判定**真的翻转**时 `cx.notify()`；宽度变化 < 0.5px 直接忽略，避免逐帧重绘 |
| 折叠项 | 只读预览 / 打开 SQL / 保存 SQL 三项进「更多 ▾」；运行 / 取消 / 库选择器 / 事务 / 诊断徽标 / 导出**两种模式都常驻** |
| 触发器 | `Button.small().h(px(24.)).ghost().label("更多").dropdown_caret(true)`，四态与命中区口径同导出触发器 |
| 菜单 | 复用 `MenuEntry` / `render_menu` / `dispatch_menu`，3 项带快捷键提示（改写为 SELECT / Ctrl+O / Ctrl+S） |
| 键盘 | 全局 `observe_keystrokes`：Enter / Space 开、↑↓ 循环、Home / End 跳首尾、Enter 执行、Esc 收起；导航逻辑与导出菜单共用 `menu_hover_move` |
| 锚点 | 左缘对齐触发器（`RAIL_W + CARD_GAP + 1 + 8`，侧栏可见时再加 `sidebar_width − CARD_GAP/2`），y = 100（标题栏 28 + 留白 6 + 边框 1 + 标签栏 31 + 工具栏 34）；菜单宽 204px，左对齐必然覆盖触发器，故不估算前导按钮宽度 |
| 溢出策略 | 折叠态去掉两条装饰性分隔线（§2.3 判定其不承载结构信息，省 26px）；库选择器是唯一可压缩项（`min_w_0` + 标签 `flex_shrink_1` + 省略号，图标 `flex_none`） |

**设计取舍一：折叠判定必须用实测宽度，视口估算不可用。** `h_resizable` 的定宽侧栏在空间不足时
会被 flex 压向 `SIDEBAR_WIDTH_MIN`(180)，但**不会把压缩后的宽度回写 `LayoutSettings`**，
于是 `layout.sidebar_width()` 仍报标称 260，估算比真实可用宽小 **79.9px**（= 260 − 180）。
这不是缩放历史造成的，首帧就会发生。后果是**一段视口区间被误折叠**（AI 关，vw ≈ 1130–1209）：

| 视口（AI 关） | 实测宽 / 判定 | 估算宽 / 判定 |
| --- | --- | --- |
| 1120 | 890.5 / 折叠 | 810.6 / 折叠 |
| 1140 | **910.5 / 平铺** | 830.6 / **误折叠** |
| 1200 | **970.5 / 平铺** | 890.6 / **误折叠** |
| 1220 | 990.5 / 平铺 | 910.6 / 平铺 |

AI 开时两者相差 ≤0.1px（AI 列没被压缩），所以问题只在侧栏被压缩的分支出现——这正是「看起来对」
最容易骗过抽查的地方。护栏：`toolbar_collapses_secondary_actions_below_900` 断言判定宽度与
`debug_bounds("workspace-toolbar")` 实测宽相差 < 1px。

> **更正（2026-11-12）**：本轮早期记录曾写「vw=1500 真实 971.5 / 估算 833.6，估算最多偏小 160px」。
> 该数字取自**共用一个 view 连续 resize** 的探测，含面板压缩历史，不可复现。
> 每配置新建 view 重测后：vw=1500 AI 开时实测 833.5 / 估算 833.6（无偏差），
> 真实最大偏差是 79.9px，出现在 AI **关**的分支。以本表为准。

**设计取舍二：真正修掉裁切的是库选择器可压缩，不是去掉分隔线。** 变异测试确证：只把两条分隔线
加回去，用例仍全绿（选择器把 26px 吸收掉了）；把选择器的 `min_w_0` / `flex_shrink_1` 去掉，
用例立刻失败在 `ai=true vw=1024 时 export-slot 被裁切（右缘 662 > 卡片 661）`。
去掉分隔线是**额外的可读性余量**（让库名少省略 26px），不是正确性必需项。

**回归记录一（`resize` 不改视口）**：`window.resize(size)` 之后所有宽度读数不变。根因是
`Window::viewport_size` 只在 `Window::bounds_changed` 里刷新（gpui-pre 0.3.9 `window.rs`，
注释写明「exposed publicly for test infrastructure」）。护栏：测试辅助
`resize_toolbar_view` 里 `resize` 必须紧跟 `bounds_changed`，并跑两轮 `run_until_parked`
让 prepaint 测量收敛。

**回归记录二（带 label 的 Button 高度不受 `with_size` 控制）**：`.with_size(px(24.))` 对
**纯图标** Button 才设 `size(px)`；带 label 时走的是 `px(size * 0.2)` 内边距分支，高度由内容决定，
实测只有 **18.5px**，断言「24~34」当场失败。改 `.small().h(px(24.))` 通过——`refine_style`
在 size 分支之后执行，调用方的 `.h()` 会覆盖组件高度。

**回归记录三（首帧判定不补帧）**：`observe_workspace_width` 最初用
`measured_workspace_w.unwrap_or(width) < 900` 取 `before`，首帧时它恒等于新值，
即使估算驱动的首帧渲染错了折叠态也不会补帧。改为先按 `window` 算出上一帧真正用过的判定
再赋值。此缺陷由自查发现，不是用例发现——**护栏**：判定翻转补帧的逻辑要连首帧一起推演。

**回归记录四（共用 view 的 resize 历史会掩盖裁切）**：一个 view 连续 resize 时，定宽面板保留上一轮
被压缩后的宽度，可用宽偏大。实测 vw=700 共用 view 报 370.5px、新建 view 报 290.5px。
`toolbar_never_clips_in_supported_window_range` 因此改为**每个配置新建 view**；
宽度 ≥ `NARROW_WINDOW`(1024) 时可用宽随视口单调递增，故 1024 是最紧约束点，
1280 / 1920 覆盖折叠态与平铺态。

**实测数字（每配置新建 view，确证）**：

| 视口 | AI 关：工具栏宽 / 裁切 | AI 开：工具栏宽 / 裁切 |
| --- | --- | --- |
| 1024 | 794.5 / 0 | **357.5 / 0**（库选择器压到 62.5px） |
| 1280 | 1050.5 / 0 | 613.5 / 0 |
| 1920 | 1610.5 / 0 | 1253.5 / 0 |

1024–1920 × AI 开/关共 12 组配置全部零裁切（含 1100 / 1440 / 1600，逐一实测）。

**未落地 / 已知限制（显式登记）**：
- **低于规范窗口最小宽 1024 时仍会裁切**：AI 开 + vw=640 溢出 56px、vw=560 溢出 136px（均为导出按钮）；
  vw ≤ 700 时库选择器被压到 0px 即完全不可见。AI 关时到 560px 仍无裁切。
  未做第二级折叠——再折就要收掉「取消」，而它是查询运行中的必要控制，代价高于收益。
  规范 §8 已把 1024×640 定为窗口下限，故按「支持范围外」登记，不当作已解决。
- 最紧配置下库名只剩约 62.5px（含图标与内边距），**只能靠省略号辨认，且目前没有 tooltip**
  （库选择器是 `div`，用不了 `Button::tooltip`）；统一 tooltip 组件仍是 P2 缺口（同 §11.1）。

**验收用例**（`ui/src/main_view/tests_layout.rs`，本轮 +6）：
- `toolbar_collapses_secondary_actions_below_900`：宽/窄两态的平铺与折叠、判定宽 == 实测宽、
  「更多 ▾」命中区 24~34 高且宽 ≥24、常驻项不消失；
- `toolbar_compact_threshold_is_900`：把 `measured_workspace_w` 钉在 899.5 / 900 / 900.5，
  **在同一次 `update` 内**读判定（中间不能重绘，否则 prepaint 会覆盖注入值）；
- `toolbar_more_menu_executes_and_closes`：点击开菜单 → 3 项齐全 → 执行后关闭 → 再开 → 点遮罩关闭；
- `toolbar_more_menu_is_keyboard_reachable`：Enter 开（高亮首项）→ ↓↓ 到末项 → Home → ↑ 回绕 →
  Enter 执行并关闭 → 再开 → Esc 收起且清空高亮；
- `toolbar_never_clips_in_supported_window_range`：见上，每配置新建 view；
- `menu_hover_move_wraps_and_handles_empty`：纯函数口径（循环 / 首尾 / 未高亮起步 / 非导航键 / 空菜单 / 单项）。

调试选择器：`workspace-toolbar` / `toolbar-more` / `toolbar-more-scrim` /
`menu-more-preview-dml` / `menu-more-open-sql` / `menu-more-save-sql`
（菜单项由 `render_menu` 统一加 `menu-` 前缀）。

**回归记录五（ghost 悬停文字与背景同色，标签整个消失）**：P1-④ 真机视觉验收时发现，
用例没抓到。Esc 收起「更多 ▾」后指针仍停在按钮上，标签文字被画成与卡片背景同色：
标签区（60×32 物理像素）修复前 `m3-after-esc` 为 1917/1920 px 纯背景 `#0D1117`，
修复后 `d1-after-esc` 恢复 114 px 字形像素；整行亮像素计数修复前 idle 6373 → 悬停 6122，
修复后 idle / 悬停均 6373（二次 Esc 6375）。根因：gpui-component 的 ghost 悬停文字色固定取
`accent_foreground`（button.rs hovered 分支），主题此前把它映射成 `on_accent`(#0D1117)，
恰等于深色卡片背景；caret 走 `normal_style.fg.opacity(0.75)` 所以单独存活，
造成「只剩箭头」的观感。「取消」「只读预览」共用同一 `.ghost() + .label()` 模式，
属主题既有缺陷，非 P1-④ 引入。修复：`ui/src/theme.rs` 深色分支把 `accent_foreground`
改取 `fg_primary`，浅色保留 `on_accent`（浅色悬停底是实心 accent，白字正确）。
应用未使用 gpui-component 的 MenuItem / Toggle / Calendar 等组件，重映射无副作用。

**护栏**：`theme::tests::ghost_hover_label_keeps_aa_contrast` 读回真实
`Theme::global` 的 `accent_foreground`：深色对三种裸底与 accent@50% 复合底逐一断言
对比度 ≥4.5，浅色对实心 accent 断言 ≥4.5。变异验证：改回旧映射用例立即失败
（`Dark 悬停文字 #0D1117 对悬停底 #335C8B 对比度 2.74 < 4.5`）。
教训：对比度验收必须覆盖**悬停态**文字色，只查静态态会漏掉同色缺陷。

**验收用例**（+1）：`ui/src/theme.rs` 上述护栏；真机截图对照
`.tmp/p4shots/` 的 m1 / m3（修复前）与 d1 / d2 / d5（修复后）。
