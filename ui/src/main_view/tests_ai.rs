//! 生成器、连接编辑器、优化与 AI 能力测试。

use super::tests_helpers::*;
use super::*;

/// F-71：Esc 撤销未填写的新增行；已填写的行不会被自动丢弃。
#[gpui::test]
fn escape_undoes_unfilled_new_row(cx: &mut gpui::TestAppContext) {
    let (mut vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = editable_view(&mut vcx, &holder);
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.append_rows(1, cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    let (rows, pending, status) = vcx.update(|_, app| {
        let view = view.read(app);
        let result = &view.tabs[0].results[0];
        (
            result.rows.len(),
            result.change_count(),
            view.status.clone(),
        )
    });
    assert_eq!(rows, 2, "Esc 应撤销未填写的新增行（{status}）");
    assert_eq!(pending, 0);
    assert!(status.contains("已撤销"), "{status}");
}

/// F-71：`fk:列名` 从当前结果取候选值；空结果给出明确错误。
#[test]
fn foreign_key_generator_uses_result_values() {
    let mut rv = editable_result_view();
    let spec = resolve_generator_spec("fk:name", &rv).expect("应能解析外键引用");
    assert_eq!(
        spec,
        rustdb_core::GeneratorSpec::ForeignKey {
            values: vec![Value::Text("a".into()), Value::Text("b".into())]
        }
    );
    let values = rustdb_core::generate(&spec, 3).expect("应能生成");
    assert_eq!(values.len(), 3);
    assert_eq!(values[2], Value::Text("a".into()), "候选值按顺序取模");

    rv.rows.clear();
    assert!(
        resolve_generator_spec("fk:name", &rv).is_err(),
        "没有候选值时应报错"
    );
}

/// F-02：连接面板的「+」打开编辑器，填完保存后连接出现在列表里。
#[gpui::test]
fn conn_editor_creates_a_profile(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.panel = PanelKind::Connections;
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let plus = vcx.debug_bounds("conn-new").expect("缺少新建连接按钮");
    vcx.simulate_click(plus.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("conn-editor").is_some(),
        "应打开连接编辑器"
    );

    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let editor = view.conn_editor.as_mut().expect("编辑器未打开");
            editor
                .name
                .update(cx, |state, cx| state.set_value("内网库", window, cx));
            editor
                .host
                .update(cx, |state, cx| state.set_value("db.internal", window, cx));
            editor
                .port
                .update(cx, |state, cx| state.set_value("5432", window, cx));
        });
    });
    vcx.run_until_parked();

    let save = vcx.debug_bounds("conn-editor-save").expect("缺少保存按钮");
    vcx.simulate_click(save.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (names, editor_open) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.profiles
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>(),
            view.conn_editor.is_some(),
        )
    });
    assert!(names.iter().any(|name| name == "内网库"), "{names:?}");
    assert!(!editor_open, "保存后编辑器应关闭");
}

/// F-02：必填项缺失时留在编辑器并给出原因，不写入连接列表。
#[gpui::test]
fn conn_editor_reports_missing_name(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.open_conn_editor(None, window, cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let save = vcx.debug_bounds("conn-editor-save").expect("缺少保存按钮");
    let before = vcx.update(|_, app| view.read(app).profiles.len());
    vcx.simulate_click(save.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (error, count) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.conn_editor.as_ref().and_then(|e| e.error.clone()),
            view.profiles.len(),
        )
    });
    assert_eq!(error.as_deref(), Some("请填写连接名"));
    assert_eq!(count, before, "校验不过不应新增连接");
}

/// F-91a：代理段可开关，类型 / 远程 DNS / 局域网都落到保存后的配置里。
#[gpui::test]
fn conn_editor_proxy_section_round_trips(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.open_conn_editor(None, window, cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("conn-proxy-kind-socks5").is_none(),
        "默认不显示代理表单"
    );
    let toggle = vcx.debug_bounds("conn-toggle-proxy").expect("缺少代理开关");
    vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("conn-proxy-kind-socks5").is_some(),
        "开启后应能选代理类型"
    );
    assert!(
        vcx.debug_bounds("conn-proxy-order-before").is_none(),
        "没有隧道时不显示代理顺序"
    );

    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let editor = view.conn_editor.as_mut().expect("编辑器未打开");
            editor
                .name
                .update(cx, |state, cx| state.set_value("代理库", window, cx));
            editor
                .host
                .update(cx, |state, cx| state.set_value("db.internal", window, cx));
            editor
                .port
                .update(cx, |state, cx| state.set_value("5432", window, cx));
            editor
                .proxy_host
                .update(cx, |state, cx| state.set_value("10.0.0.2", window, cx));
            editor
                .proxy_port
                .update(cx, |state, cx| state.set_value("1080", window, cx));
            editor
                .proxy_username
                .update(cx, |state, cx| state.set_value("ops", window, cx));
            editor
                .proxy_password
                .update(cx, |state, cx| state.set_value("pw", window, cx));
        });
    });
    vcx.run_until_parked();

    let dns = vcx
        .debug_bounds("conn-toggle-remote-dns")
        .expect("缺少远程 DNS 开关");
    vcx.simulate_click(dns.center(), gpui::Modifiers::default());
    let http = vcx
        .debug_bounds("conn-proxy-kind-http")
        .expect("缺少 HTTP 类型");
    vcx.simulate_click(http.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("conn-toggle-remote-dns").is_none(),
        "HTTP CONNECT 不显示远程 DNS 开关"
    );

    let lan = vcx.debug_bounds("conn-toggle-lan").expect("缺少局域网开关");
    vcx.simulate_click(lan.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let save = vcx.debug_bounds("conn-editor-save").expect("缺少保存按钮");
    vcx.simulate_click(save.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (profile, editor_open) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.profiles.iter().find(|p| p.name == "代理库").cloned(),
            view.conn_editor.is_some(),
        )
    });
    assert!(!editor_open, "保存后编辑器应关闭");
    let profile = profile.expect("应保存连接");
    assert!(profile.forward_lan, "局域网开关应保存");
    let proxy = profile.proxy.expect("应保存代理");
    assert_eq!(proxy.kind, rustdb_db_api::ProxyKind::HttpConnect);
    assert_eq!(proxy.host, "10.0.0.2");
    assert_eq!(proxy.port, 1080);
    assert_eq!(proxy.username.as_deref(), Some("ops"));
    assert_eq!(proxy.dev_secret.as_deref(), Some("pw"));
    assert!(!proxy.remote_dns);
    assert_eq!(proxy.order, rustdb_db_api::ProxyOrder::BeforeTunnel);
}

/// F-91：隧道段可开可关，跳板能加能删，顺序即建链顺序。
#[gpui::test]
fn conn_editor_tunnel_hops_add_and_remove(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.open_conn_editor(None, window, cx);
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let toggle = vcx
        .debug_bounds("conn-toggle-tunnel")
        .expect("缺少隧道开关");
    vcx.simulate_click(toggle.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("conn-hop-add").is_some(),
        "开启隧道后应有「添加跳板」"
    );

    let add = vcx.debug_bounds("conn-hop-add").expect("缺少添加跳板");
    vcx.simulate_click(add.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("conn-hop-0").is_some(), "应有第 1 层跳板");

    let add = vcx.debug_bounds("conn-hop-add").expect("缺少添加跳板");
    vcx.simulate_click(add.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("conn-hop-1").is_some(), "应有第 2 层跳板");

    let remove = vcx.debug_bounds("conn-hop-remove-0").expect("缺少移除跳板");
    vcx.simulate_click(remove.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("conn-hop-1").is_none(),
        "移除后不应剩第 2 层"
    );
}

/// F-91：主机密钥待确认时弹窗；拒绝后弹窗消失且不重连。
#[gpui::test]
fn host_key_prompt_requires_explicit_decision(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.apply_event(&rustdb_core::EventEnvelope {
                seq: 1,
                session_seq: None,
                ts_ms: 0,
                session_id: None,
                event: rustdb_core::AppEvent::Conn(rustdb_core::ConnEvent::TunnelHostKey {
                    profile_id: "demo-sqlite".into(),
                    layer: 1,
                    host: Some("bastion.example.com".into()),
                    port: Some(22),
                    fingerprint: "SHA256:abc".into(),
                    public_key: "ssh-ed25519 AAAA".into(),
                    changed: true,
                    recorded: Some("SHA256:old".into()),
                }),
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("host-key-prompt").is_some(),
        "应弹出主机密钥确认"
    );
    let reject = vcx.debug_bounds("host-key-reject").expect("缺少拒绝按钮");
    vcx.simulate_click(reject.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (prompt, status) = vcx.update(|_, app| {
        let view = view.read(app);
        (view.host_key_prompt.is_some(), view.status.clone())
    });
    assert!(!prompt, "拒绝后弹窗应关闭");
    assert!(status.contains("已放弃"), "{status}");
}

/// F-92：工具栏入口打开面板；转换才显示目标方言；Esc 关闭。
#[gpui::test]
fn optimize_panel_opens_from_toolbar(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            push_result(view, 1, 1);
            view.current_ns = Some(ns("main"));
            view.session = Some(SessionId::new());
            view.settings_open = true;
            view.sql_input.update(cx, |state, cx| {
                state.set_value("SELECT a\nFROM t".to_string(), window, cx)
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let button = vcx
        .debug_bounds("settings-tool-optimize")
        .expect("缺少优化入口");
    vcx.simulate_click(button.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("ai-optimize-panel").is_some(),
        "应打开优化面板"
    );
    let (kind, original) = vcx.update(|_, app| {
        let panel = view.read(app).optimize.as_ref().expect("面板状态应存在");
        (panel.kind, panel.original.clone())
    });
    assert_eq!(kind, OptimizeKind::Optimize);
    assert_eq!(original, "SELECT a\nFROM t");
    assert!(
        vcx.debug_bounds("ai-optimize-target-postgres").is_none(),
        "非转换不显示目标方言"
    );

    let convert = vcx
        .debug_bounds("ai-optimize-kind-convert")
        .expect("缺少转换选项");
    vcx.simulate_click(convert.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("ai-optimize-target-postgres").is_some(),
        "转换应显示目标方言"
    );

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("ai-optimize-panel").is_none(),
        "Esc 应关闭面板"
    );
}

/// F-92：逐段采纳/拒绝；插入编辑器只写文本、不执行。
#[gpui::test]
fn optimize_hunks_can_be_skipped_before_insert(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let original = "SELECT a\nFROM t\nWHERE id = 1\nORDER BY id\nLIMIT 10";
    let improved = "SELECT a, b\nFROM t\nWHERE id = 1\nORDER BY id DESC\nLIMIT 10";
    let hunks = rustdb_core::diff_hunks(original, improved);
    assert_eq!(hunks.len(), 2, "应切成两段改动");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            push_result(view, 1, 1);
            view.current_ns = Some(ns("main"));
            view.sql_input.update(cx, |state, cx| {
                state.set_value(original.to_string(), window, cx)
            });
            view.optimize = Some(OptimizePanel {
                kind: OptimizeKind::Optimize,
                source: DbKind::Sqlite,
                target: DbKind::Postgres,
                instruction: None,
                original: original.to_string(),
                running: false,
                error: None,
                accepted: vec![true; hunks.len()],
                result: Some(rustdb_core::OptimizeResult {
                    explanation: "补一列并改排序".into(),
                    sql: Some(improved.to_string()),
                    notes: Vec::new(),
                    dialect_notes: Vec::new(),
                    hunks,
                }),
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let second = vcx
        .debug_bounds("ai-optimize-hunk-1")
        .expect("缺少第二段改动");
    vcx.simulate_click(second.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let insert = vcx
        .debug_bounds("ai-optimize-insert")
        .expect("缺少插入按钮");
    vcx.simulate_click(insert.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (closed, sql, status) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.optimize.is_none(),
            view.tabs[0].sql.clone(),
            view.status.clone(),
        )
    });
    assert!(closed, "插入后面板应关闭");
    assert_eq!(
        sql, "SELECT a, b\nFROM t\nWHERE id = 1\nORDER BY id\nLIMIT 10",
        "被跳过的第二段应保持原文"
    );
    assert!(status.contains("未执行"), "{status}");
}

/// F-92：「新标签」不覆盖当前页，也不触发执行。
#[gpui::test]
fn optimize_can_open_a_new_tab(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let original = "SELECT 1";
    let improved = "SELECT 1 AS one";
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            push_result(view, 1, 1);
            view.current_ns = Some(ns("main"));
            view.sql_input.update(cx, |state, cx| {
                state.set_value(original.to_string(), window, cx)
            });
            let hunks = rustdb_core::diff_hunks(original, improved);
            view.optimize = Some(OptimizePanel {
                kind: OptimizeKind::Optimize,
                source: DbKind::Sqlite,
                target: DbKind::Postgres,
                instruction: None,
                original: original.to_string(),
                running: false,
                error: None,
                accepted: vec![true; hunks.len()],
                result: Some(rustdb_core::OptimizeResult {
                    explanation: String::new(),
                    sql: Some(improved.to_string()),
                    notes: Vec::new(),
                    dialect_notes: Vec::new(),
                    hunks,
                }),
            });
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let new_tab = vcx
        .debug_bounds("ai-optimize-new-tab")
        .expect("缺少新标签按钮");
    vcx.simulate_click(new_tab.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (tabs, sql, running, status) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.tabs.len(),
            view.tabs
                .last()
                .map(|tab| tab.sql.clone())
                .unwrap_or_default(),
            view.tabs
                .last()
                .and_then(|tab| tab.running.clone())
                .is_some(),
            view.status.clone(),
        )
    });
    assert_eq!(tabs, 2, "应新增一个标签页");
    assert_eq!(sql, improved);
    assert!(!running, "插入结果不应自动执行");
    assert!(status.contains("未执行"), "{status}");
}

/// F-92：生成走同一个 provider，一次调用即回填说明与 diff。
#[gpui::test]
fn optimize_generation_fills_result(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            push_result(view, 1, 1);
            view.current_ns = Some(ns("main"));
            view.session = Some(SessionId::new());
            view.settings_open = true;
            view.sql_input.update(cx, |state, cx| {
                state.set_value("SELECT 1".to_string(), window, cx)
            });
            view.agent = Arc::new(rustdb_core::AgentEngine::new(Arc::new(
                rustdb_core::ScriptedProvider::new(vec![
                    rustdb_core::agent::AssistantTurn::Answer(
                        "补一个别名。\n```sql\nSELECT 1 AS one\n```".into(),
                    ),
                ]),
            )));
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let button = vcx
        .debug_bounds("settings-tool-optimize")
        .expect("缺少优化入口");
    vcx.simulate_click(button.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let run = vcx.debug_bounds("ai-optimize-run").expect("缺少生成按钮");
    vcx.simulate_click(run.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (explanation, hunks, error) = vcx.update(|_, app| {
        let panel = view.read(app).optimize.as_ref().expect("面板应保持打开");
        let result = panel.result.as_ref().expect("应拿到结果");
        (
            result.explanation.clone(),
            result.hunks.len(),
            panel.error.clone(),
        )
    });
    assert_eq!(error, None);
    assert_eq!(explanation, "补一个别名。");
    assert_eq!(hunks, 1);
}

/// F-93：错误栏的「AI 修复」把出错语句和报错预填进「改写」，且不执行。
#[gpui::test]
fn error_offers_ai_fix_and_prefills_rewrite(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let sql = "SELECT 1;\nSELECT 中 FROM nope;";
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let mut tab = SqlTab::new(1, Some(ns("main")));
            tab.sql = sql.into();
            tab.sql_error = Some(SqlErrorMark {
                message: "syntax error at or near \"中\"".into(),
                start: 10,
                end: 30,
            });
            view.tabs.push(tab);
            view.active_tab = 0;
            view.sql_input
                .update(cx, |state, cx| state.set_value(sql, window, cx));
            cx.notify();
        });
    });
    vcx.run_until_parked();

    let fix = vcx.debug_bounds("ai-fix-error").expect("缺少 AI 修复入口");
    vcx.simulate_click(fix.center(), gpui::Modifiers::default());
    vcx.run_until_parked();

    let (kind, original, instruction, running) = vcx.update(|_, app| {
        let view = view.read(app);
        let panel = view.optimize.as_ref().expect("应打开改写面板");
        (
            panel.kind,
            panel.original.clone(),
            panel
                .instruction
                .as_ref()
                .map(|input| input.read(app).value().to_string())
                .unwrap_or_default(),
            view.tabs.iter().any(|tab| tab.running.is_some()),
        )
    });
    assert_eq!(kind, OptimizeKind::Rewrite);
    assert_eq!(original, "SELECT 中 FROM nope");
    assert!(instruction.contains("syntax error"), "{instruction}");
    assert!(instruction.contains("修好"), "{instruction}");
    assert!(!running, "修复入口不应执行任何语句");
}

/// F-93：上下文芯片可追加、去重、移除。
#[gpui::test]
fn ai_context_chips_add_and_remove(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.layout.ai_open = true;
            view.ai_ctx_push(
                rustdb_core::ContextItem::new(
                    rustdb_core::ContextKind::Table,
                    "main.users",
                    "表：main.users\n- id INT NOT NULL PK",
                ),
                cx,
            );
            view.ai_ctx_push(
                rustdb_core::ContextItem::new(
                    rustdb_core::ContextKind::Table,
                    "main.users",
                    "重复项",
                ),
                cx,
            );
            view.ai_ctx_push(
                rustdb_core::ContextItem::new(
                    rustdb_core::ContextKind::Selection,
                    "SELECT 1",
                    "SELECT 1",
                ),
                cx,
            );
        });
    });
    vcx.run_until_parked();

    assert_eq!(
        vcx.update(|_, app| view.read(app).ai_context.len()),
        2,
        "同标签的重复上下文应被忽略"
    );
    assert!(vcx.debug_bounds("ai-ctx-chip-0").is_some());
    assert!(vcx.debug_bounds("ai-ctx-chip-1").is_some());

    let chip = vcx.debug_bounds("ai-ctx-chip-0").expect("缺少芯片");
    vcx.simulate_click(chip.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let chips = vcx.update(|_, app| {
        view.read(app)
            .ai_context
            .iter()
            .map(|item| item.label.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(chips, vec!["SELECT 1".to_string()], "点击芯片应移除对应项");
    assert!(vcx.debug_bounds("ai-ctx-chip-1").is_none());
}

/// F-93：「应用」只写编辑器，不执行；块状态随标签状态变化。
#[gpui::test]
fn ai_sql_block_applies_without_running(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    let line = vcx.update(|_, app| {
        view.update(app, |view, cx| {
            view.current_ns = Some(ns("main"));
            view.ai_lines.clear();
            view.ai_lines.push((
                "assistant".into(),
                "试试：\n```sql\nSELECT 42 AS answer\n```\n".into(),
            ));
            cx.notify();
            view.ai_lines.len() - 1
        })
    });
    vcx.run_until_parked();

    vcx.update(|window, app| {
        view.update(app, |view, cx| view.ai_block_apply(line, 0, window, cx));
    });
    vcx.run_until_parked();

    let (text, running) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.sql_input.read(app).value().to_string(),
            view.tabs.iter().any(|tab| tab.running.is_some()),
        )
    });
    assert!(text.contains("SELECT 42 AS answer"), "{text:?}");
    assert!(!running, "应用不应自动执行");

    let tokens = rustdb_core::theme::ThemeTokens::dark();
    let statuses = vcx.update(|_, app| {
        view.update(app, |view, _| {
            let note = AiBlockExec {
                tab_id: None,
                query_id: None,
                note: Some("临时运行只执行只读语句；写入请用「正式执行」".into()),
                temporary: true,
            };
            view.ai_block_execs.insert((line, 0), note);
            let blocked = view.ai_block_status(line, 0, &tokens).map(|(text, _)| text);

            let query_id = QueryId::new();
            let mut tab = SqlTab::new(7, Some(ns("main")));
            tab.running = Some(query_id.clone());
            view.tabs.push(tab);
            view.ai_block_execs.insert(
                (line, 0),
                AiBlockExec {
                    tab_id: Some(7),
                    query_id: Some(query_id),
                    note: None,
                    temporary: true,
                },
            );
            let running = view.ai_block_status(line, 0, &tokens).map(|(text, _)| text);

            view.tabs.clear();
            let closed = view.ai_block_status(line, 0, &tokens).map(|(text, _)| text);
            (blocked, running, closed)
        })
    });
    assert!(
        statuses.0.as_deref().unwrap_or_default().contains("只读"),
        "{statuses:?}"
    );
    assert_eq!(statuses.1.as_deref(), Some("试跑中…"));
    assert_eq!(statuses.2.as_deref(), Some("标签已关闭"));
}

#[gpui::test]
fn ai_model_panel_saves_profile_without_key(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");

    // 空配置时面板载入第一个预设（OpenAI）。
    vcx.update(|window, app| {
        view.update(app, |view, cx| view.ai_models_open(window, cx));
    });
    vcx.run_until_parked();
    let (kind, name, editing) = vcx.update(|_, app| {
        let view = view.read(app);
        let panel = view.ai_model_panel.as_ref().expect("面板未打开");
        (
            panel.kind,
            panel.name.read(app).value().to_string(),
            panel.editing_id.clone(),
        )
    });
    assert_eq!(kind, rustdb_core::AiKind::Cloud);
    assert_eq!(name, "OpenAI");
    assert!(editing.is_none(), "预设尚未保存");
    assert!(
        vcx.debug_bounds("ai-model-panel").is_some(),
        "模型面板应已绘制"
    );

    let path = ai_test_path("save");
    let _ = std::fs::remove_file(&path);
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            view.ai_config_path = path.clone();
            let panel = view.ai_model_panel.as_mut().expect("面板未打开");
            panel.kind = rustdb_core::AiKind::Local;
            panel.auth = rustdb_core::AuthKind::None;
            panel
                .name
                .update(cx, |state, cx| state.set_value("本地 Ollama", window, cx));
            panel.base_url.update(cx, |state, cx| {
                state.set_value("http://127.0.0.1:11434/v1", window, cx)
            });
            panel
                .model
                .update(cx, |state, cx| state.set_value("qwen2.5", window, cx));
            panel
                .key
                .update(cx, |state, cx| state.set_value("sk-in-memory", window, cx));
            view.ai_panel_save(cx);
        });
    });
    vcx.run_until_parked();

    let (active, provider, in_memory, panel_open, profiles) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.ai_settings.active.clone(),
            view.agent.provider_name(),
            view.ai_keys.get("ollama").cloned(),
            view.ai_model_panel.is_some(),
            view.ai_settings.profiles.len(),
        )
    });
    assert_eq!(active.as_deref(), Some("ollama"));
    assert!(provider.contains("qwen2.5"), "{provider}");
    assert_eq!(in_memory.as_deref(), Some("sk-in-memory"));
    assert!(!panel_open, "保存后面板应关闭");
    assert_eq!(profiles, 1);

    let saved = std::fs::read_to_string(&path).expect("配置文件未写入");
    assert!(!saved.contains("sk-in-memory"), "密钥不能落盘：{saved}");
    assert!(saved.contains("qwen2.5"));
    let _ = std::fs::remove_file(&path);

    // 再次打开：应载入已保存配置（可删除、可再次保存）。
    vcx.update(|window, app| {
        view.update(app, |view, cx| view.ai_models_open(window, cx));
    });
    vcx.run_until_parked();
    let editing = vcx.update(|_, app| {
        view.read(app)
            .ai_model_panel
            .as_ref()
            .and_then(|panel| panel.editing_id.clone())
    });
    assert_eq!(editing.as_deref(), Some("ollama"));
}

#[gpui::test]
fn ai_model_panel_rejects_bad_config(cx: &mut gpui::TestAppContext) {
    let (vcx, holder) = test_view(cx);
    vcx.run_until_parked();
    let view = holder.borrow().clone().expect("view 未创建");
    vcx.update(|window, app| {
        view.update(app, |view, cx| view.ai_models_open(window, cx));
    });
    vcx.run_until_parked();

    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let panel = view.ai_model_panel.as_mut().expect("面板未打开");
            panel.kind = rustdb_core::AiKind::Cli;
            panel
                .cli_command
                .update(cx, |state, cx| state.set_value("", window, cx));
            view.ai_panel_save(cx);
        });
    });
    vcx.run_until_parked();
    let (errors, profiles) = vcx.update(|_, app| {
        let view = view.read(app);
        (
            view.ai_model_panel
                .as_ref()
                .map(|panel| panel.errors.clone())
                .unwrap_or_default(),
            view.ai_settings.profiles.len(),
        )
    });
    assert!(
        errors.iter().any(|error| error.contains("命令")),
        "{errors:?}"
    );
    assert_eq!(profiles, 0, "校验失败不能写入配置");

    // 本地 CLI 不支持发现模型。
    vcx.update(|window, app| {
        view.update(app, |view, cx| {
            let panel = view.ai_model_panel.as_mut().expect("面板未打开");
            panel.cli_command.update(cx, |state, cx| {
                state.set_value("ollama run llama3.1", window, cx)
            });
            view.ai_panel_discover(window, cx);
        });
    });
    vcx.run_until_parked();
    let errors = vcx.update(|_, app| {
        view.read(app)
            .ai_model_panel
            .as_ref()
            .map(|panel| panel.errors.clone())
            .unwrap_or_default()
    });
    assert!(
        errors.iter().any(|error| error.contains("不支持发现")),
        "{errors:?}"
    );
}
