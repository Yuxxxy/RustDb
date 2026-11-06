//! 设置面板：外观、低频工具、AI 与关于。
//!
//! 用 gpui-component 的 `Settings`（页 / 组 / 项）搭骨架，
//! 只有工具入口用 `SettingField::render` 放一个「打开」按钮。

use super::*;
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings};

impl MainView {
    pub(crate) fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = true;
        cx.notify();
    }

    fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = false;
        cx.notify();
    }

    pub(crate) fn set_theme_mode(&mut self, mode: ThemeMode, cx: &mut Context<Self>) {
        if self.layout.theme == mode {
            return;
        }
        self.layout.theme = mode;
        crate::theme::apply_theme_mode(cx, mode);
        self.dark = crate::theme::is_dark(cx);
        self.persist_layout();
        cx.notify();
    }

    fn set_ai_default(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.layout.ai_open == open {
            return;
        }
        self.layout.ai_open = open;
        self.persist_layout();
        cx.notify();
    }

    fn run_settings_tool(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            "settings-tool-docs" => self.open_docs_panel(window, cx),
            "settings-tool-compare" => self.open_compare_panel(window, cx),
            "settings-tool-data-compare" => self.open_data_compare_panel(window, cx),
            "settings-tool-dump" => self.open_dump_panel(window, cx),
            "settings-tool-transfer" => self.open_transfer_panel(window, cx),
            "settings-tool-optimize" => self.open_optimize_panel(cx),
            "settings-exchange" => self.open_exchange_panel(window, cx),
            "settings-ai-models" => {
                self.layout.ai_open = true;
                self.persist_layout();
                self.ai_models_open(window, cx);
            }
            _ => {}
        }
    }

    pub(crate) fn render_settings(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let t = current(cx);
        let me = cx.entity();
        let connected = self.session.is_some();

        let tool_item = |title: &'static str,
                         hint: &'static str,
                         selector: &'static str,
                         enabled: bool,
                         key: &'static str| {
            let me_click = me.clone();
            SettingItem::new(
                title,
                SettingField::render(move |options, _, _| {
                    let me = me_click.clone();
                    h_flex()
                        .debug_selector(move || selector.to_string())
                        .child(
                            Button::new(SharedString::from(format!("{selector}-open")))
                                .small()
                                .outline()
                                .with_size(options.size())
                                .label("打开")
                                .disabled(!enabled)
                                .on_click(move |_, window, cx| {
                                    me.update(cx, |view, cx| {
                                        view.close_settings(cx);
                                        view.run_settings_tool(key, window, cx);
                                    });
                                }),
                        )
                        .into_any_element()
                }),
            )
            .description(hint)
            .disabled(!enabled)
        };

        let tools = SettingGroup::new().title("工具").items([
            tool_item(
                "数据库文档",
                "导出 Markdown / DBML",
                "settings-tool-docs",
                connected,
                "settings-tool-docs",
            ),
            tool_item(
                "结构对比",
                "快照应用到目标库",
                "settings-tool-compare",
                connected,
                "settings-tool-compare",
            ),
            tool_item(
                "数据对比",
                "按关键字段同步数据",
                "settings-tool-data-compare",
                connected,
                "settings-tool-data-compare",
            ),
            tool_item(
                "逻辑导出",
                "结构 + 数据的 SQL",
                "settings-tool-dump",
                connected,
                "settings-tool-dump",
            ),
            tool_item(
                "跨库传输",
                "源表复制到目标连接",
                "settings-tool-transfer",
                connected,
                "settings-tool-transfer",
            ),
            tool_item(
                "SQL 优化",
                "解释 / 改写 / 转方言",
                "settings-tool-optimize",
                connected,
                "settings-tool-optimize",
            ),
        ]);

        let config = SettingGroup::new().title("配置").item(tool_item(
            "导入 / 导出连接",
            "用加密文件交换连接配置",
            "settings-exchange",
            true,
            "settings-exchange",
        ));

        let tools_page = SettingPage::new("工具")
            .icon(IconName::Wrench)
            .group(tools)
            .group(config);

        let me_theme = me.clone();
        let theme_page = SettingPage::new("外观").icon(IconName::Sun).group(
            SettingGroup::new().item(
                SettingItem::new(
                    "主题",
                    SettingField::dropdown(
                        vec![
                            (SharedString::from("dark"), SharedString::from("深色")),
                            (SharedString::from("light"), SharedString::from("浅色")),
                            (SharedString::from("system"), SharedString::from("跟随系统")),
                        ],
                        {
                            let me = me_theme.clone();
                            move |cx: &App| SharedString::from(theme_key(me.read(cx).layout.theme))
                        },
                        {
                            let me = me_theme.clone();
                            move |value: SharedString, cx: &mut App| {
                                let mode = theme_from_key(&value);
                                me.update(cx, |view, cx| view.set_theme_mode(mode, cx));
                            }
                        },
                    )
                    .default_value("dark"),
                )
                .description("深色 / 浅色，或跟随系统"),
            ),
        );

        let me_ai = me.clone();
        let ai_page = SettingPage::new("AI 助手").icon(IconName::Bot).group(
            SettingGroup::new()
                .item(
                    SettingItem::new(
                        "显示 AI 面板",
                        SettingField::switch(
                            {
                                let me = me_ai.clone();
                                move |cx: &App| me.read(cx).layout.ai_open
                            },
                            {
                                let me = me_ai.clone();
                                move |value: bool, cx: &mut App| {
                                    me.update(cx, |view, cx| view.set_ai_default(value, cx));
                                }
                            },
                        )
                        .default_value(false),
                    )
                    .description("右侧对话列，关掉可腾出查询空间"),
                )
                .item(tool_item(
                    "模型配置",
                    "预设 / 连通性测试",
                    "settings-ai-models",
                    true,
                    "settings-ai-models",
                )),
        );

        let about_page =
            SettingPage::new("关于")
                .icon(IconName::Info)
                .group(
                    SettingGroup::new().item(SettingItem::render(move |_, _, _| {
                        v_flex()
                            .gap_1()
                            .text_size(px(11.5))
                            .text_color(hex(t.fg_secondary))
                            .child(SharedString::from(format!(
                                "RustDb v{}",
                                rustdb_core::APP_VERSION
                            )))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(hex(t.fg_muted))
                                    .child("Rust · GPUI"),
                            )
                    })),
                );

        let header = h_flex()
            .h(px(34.))
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(hex(t.border_subtle))
            .child(
                Icon::new(IconName::Settings)
                    .small()
                    .text_color(hex(t.accent)),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hex(t.fg_primary))
                    .child("设置"),
            )
            .child(
                div()
                    .id("settings-close")
                    .debug_selector(|| "settings-close".to_string())
                    .w(px(18.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|style| style.bg(hex(t.bg_overlay)))
                    .on_click(cx.listener(|view, _, _, cx| view.close_settings(cx)))
                    .child(
                        Icon::new(IconName::Close)
                            .small()
                            .text_color(hex(t.fg_muted)),
                    ),
            );

        let settings = Settings::new("settings-panel")
            .sidebar_width(px(160.))
            .sidebar_size_range(px(140.)..px(240.))
            .pages(vec![tools_page, theme_page, ai_page, about_page]);

        v_flex()
            .id("settings-card")
            .debug_selector(|| "settings-card".to_string())
            .occlude()
            .w(px(740.))
            .h(px(560.))
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            .child(header)
            .child(div().w_full().h(px(525.)).child(settings))
            .into_any_element()
    }
}

fn theme_key(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::Dark => "dark",
        ThemeMode::Light => "light",
        ThemeMode::System => "system",
    }
}

fn theme_from_key(key: &str) -> ThemeMode {
    match key {
        "light" => ThemeMode::Light,
        "system" => ThemeMode::System,
        _ => ThemeMode::Dark,
    }
}
