//! 连接编辑器（F-02）：新建 / 编辑 / 保存连接配置。
//!
//! 从 `impl MainView` 的连接配置命令簇拆出；连接建立本身仍在 `connect`。
use super::*;
impl MainView {
    /// F-02：打开连接编辑器（`profile_id = None` 表示新建）。
    pub(crate) fn open_conn_editor(
        &mut self,
        profile_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = profile_id.and_then(|id| {
            self.profiles
                .iter()
                .find(|profile| profile.id == id)
                .cloned()
        });
        let mut hops = Vec::new();
        let mut tunnel = false;
        if let Some(profile) = &existing {
            if let Some(spec) = &profile.tunnel {
                tunnel = !spec.is_empty();
                for hop in &spec.hops {
                    let (auth, secret) = match hop.auth.methods.first() {
                        Some(rustdb_db_api::TunnelAuthMethod::KeyFile {
                            dev_passphrase, ..
                        }) => {
                            if dev_passphrase.is_some() {
                                (HopAuth::KeyPassphrase, dev_passphrase.clone())
                            } else {
                                (HopAuth::Key, None)
                            }
                        }
                        Some(rustdb_db_api::TunnelAuthMethod::Password { dev_secret, .. }) => {
                            (HopAuth::Password, dev_secret.clone())
                        }
                        _ => (HopAuth::None, None),
                    };
                    let key_path = match hop.auth.methods.first() {
                        Some(rustdb_db_api::TunnelAuthMethod::KeyFile { path, .. }) => path.clone(),
                        _ => String::new(),
                    };
                    hops.push(HopEditor {
                        host: conn_input(window, cx, "跳板主机", Some(&hop.host)),
                        port: conn_input(window, cx, "22", Some(&hop.port.to_string())),
                        user: conn_input(window, cx, "用户名", Some(&hop.user)),
                        auth,
                        key_path: conn_input(window, cx, "~/.ssh/id_ed25519", Some(&key_path)),
                        secret: conn_input(window, cx, "口令（只留在内存）", secret.as_deref()),
                    });
                }
            }
        }
        let proxy = existing.as_ref().and_then(|profile| profile.proxy.clone());
        let proxy_on = proxy.is_some();
        let proxy_kind = proxy.as_ref().map(|proxy| proxy.kind).unwrap_or_default();
        let proxy_port = proxy
            .as_ref()
            .map(|proxy| proxy.port.to_string())
            .unwrap_or_else(|| proxy_kind.default_port().to_string());
        let proxy_host = proxy
            .as_ref()
            .map(|proxy| proxy.host.clone())
            .unwrap_or_default();
        let proxy_username = proxy
            .as_ref()
            .and_then(|proxy| proxy.username.clone())
            .unwrap_or_default();
        let proxy_remote_dns = proxy.as_ref().map(|proxy| proxy.remote_dns).unwrap_or(true);
        let proxy_order = proxy.as_ref().map(|proxy| proxy.order).unwrap_or_default();
        let forward_lan = existing.as_ref().is_some_and(|profile| profile.forward_lan);
        let name = existing
            .as_ref()
            .map(|profile| profile.name.clone())
            .unwrap_or_default();
        let folder = existing
            .as_ref()
            .and_then(|profile| profile.folder.clone())
            .unwrap_or_default();
        let driver = existing
            .as_ref()
            .map(|profile| profile.driver)
            .unwrap_or(DbKind::Postgres);
        let host = existing
            .as_ref()
            .and_then(|profile| profile.host.clone())
            .unwrap_or_default();
        let port = existing
            .as_ref()
            .and_then(|profile| profile.port)
            .map(|port| port.to_string())
            .unwrap_or_default();
        let database = existing
            .as_ref()
            .and_then(|profile| profile.database.clone())
            .unwrap_or_default();
        let username = existing
            .as_ref()
            .and_then(|profile| profile.username.clone())
            .unwrap_or_default();
        let ssl_mode = existing
            .as_ref()
            .and_then(|profile| profile.ssl_mode.clone())
            .unwrap_or_default();
        self.conn_editor = Some(ConnEditor {
            id: existing.as_ref().map(|profile| profile.id.clone()),
            name: conn_input(window, cx, "连接名", Some(&name)),
            folder: conn_input(window, cx, "分组（可空）", Some(&folder)),
            driver,
            host: conn_input(window, cx, "主机", Some(&host)),
            port: conn_input(window, cx, "端口", Some(&port)),
            database: conn_input(window, cx, "数据库 / 文件", Some(&database)),
            username: conn_input(window, cx, "用户名", Some(&username)),
            // 口令不落盘：这里只放内存里的临时值。
            password: conn_input(window, cx, "口令（只留在内存）", None),
            ssl_mode: conn_input(window, cx, "SSL 模式", Some(&ssl_mode)),
            read_only: existing.as_ref().is_some_and(|profile| profile.read_only),
            production: existing.as_ref().is_some_and(|profile| profile.production),
            tunnel,
            hops,
            proxy: proxy_on,
            proxy_kind,
            proxy_host: conn_input(window, cx, "代理主机", Some(&proxy_host)),
            proxy_port: conn_input(window, cx, "1080", Some(&proxy_port)),
            proxy_username: conn_input(window, cx, "代理用户名（可空）", Some(&proxy_username)),
            proxy_password: conn_input(window, cx, "代理口令（只留在内存）", None),
            proxy_remote_dns,
            proxy_order,
            forward_lan,
            error: None,
        });
        cx.notify();
    }

    /// 从编辑器读出一份连接配置；校验不通过时返回原因。
    pub(crate) fn conn_editor_profile(
        &self,
        cx: &App,
    ) -> Result<rustdb_core::ConnectionProfile, String> {
        let editor = self
            .conn_editor
            .as_ref()
            .ok_or_else(|| "连接编辑器未打开".to_string())?;
        let name = editor.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return Err("请填写连接名".into());
        }
        let host = editor.host.read(cx).value().trim().to_string();
        let port = editor.port.read(cx).value().trim().to_string();
        let port = if port.is_empty() {
            None
        } else {
            match port.parse::<u16>() {
                Ok(port) if port > 0 => Some(port),
                _ => return Err("端口必须是 1-65535 的整数".into()),
            }
        };
        if editor.driver != DbKind::Sqlite && host.is_empty() {
            return Err("请填写主机地址".into());
        }
        let mut tunnel = None;
        if editor.tunnel {
            // 隧道转发的是 TCP 目标，SQLite 文件路径走不了隧道。
            if host.is_empty() {
                return Err("走 SSH 隧道时必须填写目标主机".into());
            }
            let mut hops = Vec::with_capacity(editor.hops.len());
            for (index, hop) in editor.hops.iter().enumerate() {
                let layer = index + 1;
                let hop_host = hop.host.read(cx).value().trim().to_string();
                let hop_port = hop.port.read(cx).value().trim().to_string();
                let hop_user = hop.user.read(cx).value().trim().to_string();
                if hop_host.is_empty() || hop_user.is_empty() {
                    return Err(format!("第 {layer} 层跳板要填主机与用户名"));
                }
                let hop_port = match hop_port.parse::<u16>() {
                    Ok(port) if port > 0 => port,
                    _ => return Err(format!("第 {layer} 层端口无效")),
                };
                let secret = hop.secret.read(cx).value().to_string();
                let key_path = hop.key_path.read(cx).value().trim().to_string();
                let method = match hop.auth {
                    HopAuth::Key => rustdb_db_api::TunnelAuthMethod::KeyFile {
                        path: key_path,
                        passphrase_ref: None,
                        dev_passphrase: None,
                    },
                    HopAuth::KeyPassphrase => rustdb_db_api::TunnelAuthMethod::KeyFile {
                        path: key_path,
                        passphrase_ref: None,
                        dev_passphrase: (!secret.is_empty()).then_some(secret),
                    },
                    HopAuth::Password => rustdb_db_api::TunnelAuthMethod::Password {
                        secret_ref: None,
                        dev_secret: (!secret.is_empty()).then_some(secret),
                    },
                    HopAuth::None => rustdb_db_api::TunnelAuthMethod::None,
                };
                hops.push(rustdb_db_api::TunnelHopSpec {
                    host: hop_host,
                    port: hop_port,
                    user: hop_user,
                    auth: rustdb_db_api::TunnelAuthSpec::single(method),
                    host_keys: rustdb_db_api::HostKeySpec::KnownHostsDefault,
                });
            }
            if hops.is_empty() {
                return Err("已开启 SSH 隧道，但还没有配置跳板".into());
            }
            let spec = rustdb_db_api::TunnelSpec { hops };
            spec.validate()?;
            tunnel = Some(spec);
        }
        let mut proxy = None;
        if editor.proxy {
            let proxy_host = editor.proxy_host.read(cx).value().trim().to_string();
            let proxy_port = editor.proxy_port.read(cx).value().trim().to_string();
            let proxy_port = if proxy_port.is_empty() {
                editor.proxy_kind.default_port()
            } else {
                match proxy_port.parse::<u16>() {
                    Ok(port) if port > 0 => port,
                    _ => return Err("代理端口必须是 1-65535 的整数".into()),
                }
            };
            let proxy_username = Some(editor.proxy_username.read(cx).value().trim().to_string())
                .filter(|value| !value.is_empty());
            // 代理口令同样不回显：留空沿用当前内存里的口令。
            let typed = editor.proxy_password.read(cx).value().to_string();
            let dev_secret = if !typed.is_empty() {
                Some(typed)
            } else {
                editor.id.as_ref().and_then(|id| {
                    self.profiles
                        .iter()
                        .find(|profile| &profile.id == id)
                        .and_then(|profile| profile.proxy.as_ref())
                        .and_then(|proxy| proxy.dev_secret.clone())
                })
            };
            let spec = rustdb_db_api::ProxySpec {
                kind: editor.proxy_kind,
                host: proxy_host,
                port: proxy_port,
                username: proxy_username,
                secret_ref: None,
                dev_secret,
                remote_dns: editor.proxy_remote_dns,
                // 顺序只对「代理 + 隧道」有意义；没有隧道时界面也不显示它。
                order: if tunnel.is_some() {
                    editor.proxy_order
                } else {
                    rustdb_db_api::ProxyOrder::BeforeTunnel
                },
            };
            spec.validate()?;
            proxy = Some(spec);
        }
        // 没有隧道也没有代理时不存在本地转发，局域网开关无意义。
        let forward_lan = (tunnel.is_some() || proxy.is_some()) && editor.forward_lan;
        let id = editor.id.clone().unwrap_or_else(|| {
            // 新建：按名称生成稳定 id（同名不重复）。
            let base = name
                .chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                        ch
                    } else {
                        '-'
                    }
                })
                .collect::<String>();
            let base = if base.is_empty() {
                "conn".to_string()
            } else {
                base
            };
            let mut id = base.clone();
            let mut index = 2;
            while self.profiles.iter().any(|profile| profile.id == id) {
                id = format!("{base}-{index}");
                index += 1;
            }
            id
        });
        // 口令不回显：留空表示「沿用当前内存里的口令」，而不是清空它。
        let password = editor.password.read(cx).value().to_string();
        let dev_secret = if !password.is_empty() {
            Some(password)
        } else {
            editor.id.as_ref().and_then(|id| {
                self.profiles
                    .iter()
                    .find(|profile| &profile.id == id)
                    .and_then(|profile| profile.dev_secret.clone())
            })
        };
        Ok(rustdb_core::ConnectionProfile {
            id,
            name,
            folder: Some(editor.folder.read(cx).value().trim().to_string())
                .filter(|folder| !folder.is_empty()),
            driver: editor.driver,
            host: (!host.is_empty()).then_some(host),
            port,
            database: Some(editor.database.read(cx).value().trim().to_string())
                .filter(|value| !value.is_empty()),
            username: Some(editor.username.read(cx).value().trim().to_string())
                .filter(|value| !value.is_empty()),
            secret_ref: None,
            // 口令只留在内存（`#[serde(skip)]`，不落盘）。
            dev_secret,
            ssl_mode: Some(editor.ssl_mode.read(cx).value().trim().to_string())
                .filter(|value| !value.is_empty()),
            params: Default::default(),
            read_only: editor.read_only,
            production: editor.production,
            tunnel,
            proxy,
            forward_lan,
            connect_timeout_ms: 10_000,
            color: None,
            last_used_at: None,
        })
    }

    /// F-02：保存连接配置。
    pub(crate) fn save_conn_editor(&mut self, cx: &mut Context<Self>) {
        let profile = match self.conn_editor_profile(cx) {
            Ok(profile) => profile,
            Err(error) => {
                if let Some(editor) = self.conn_editor.as_mut() {
                    editor.error = Some(error);
                }
                cx.notify();
                return;
            }
        };
        let is_new = self
            .conn_editor
            .as_ref()
            .is_some_and(|editor| editor.id.is_none());
        let result = if is_new {
            self.core.add_profile(profile)
        } else {
            self.core.update_profile(profile)
        };
        match result {
            Ok(()) => {
                self.profiles = self.core.profiles();
                self.conn_editor = None;
                self.status = if is_new {
                    "已新建连接".into()
                } else {
                    "已保存连接配置".into()
                };
                self.status_kind = StatusKind::Ok;
            }
            Err(error) => {
                if let Some(editor) = self.conn_editor.as_mut() {
                    editor.error = Some(error);
                }
            }
        }
        cx.notify();
    }

    /// F-02：连接编辑器浮层（新建 / 编辑 + SSH 隧道跳板）。
    pub(crate) fn render_conn_editor(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = current(cx);
        let Some(editor) = self.conn_editor.as_ref() else {
            return div().into_any_element();
        };
        let title = if editor.id.is_some() {
            "编辑连接"
        } else {
            "新建连接"
        };
        let error = editor.error.clone();
        let driver = editor.driver;
        let read_only = editor.read_only;
        let production = editor.production;
        let tunnel_on = editor.tunnel;
        let proxy_on = editor.proxy;
        let proxy_kind = editor.proxy_kind;
        let proxy_order = editor.proxy_order;
        let proxy_remote_dns = editor.proxy_remote_dns;
        let lan_on = editor.forward_lan;
        // 驱动选择条。
        let mut driver_row = h_flex().gap_1().items_center().flex_wrap();
        for kind in DbKind::ALL {
            let active = kind == driver;
            let selector = format!("conn-driver-{}", kind.key());
            driver_row = driver_row.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .h(px(20.))
                    .px_2()
                    .rounded(px(4.))
                    .text_size(px(10.5))
                    .cursor_pointer()
                    .when(active, |d| d.bg(hex(t.accent)).text_color(hex(t.on_accent)))
                    .when(!active, |d| {
                        d.text_color(hex(t.fg_secondary))
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(editor) = view.conn_editor.as_mut() {
                            editor.driver = kind;
                        }
                        cx.notify();
                    }))
                    .child(SharedString::from(kind.display_name())),
            );
        }
        // 跳板编辑行。
        let mut hop_rows = v_flex().gap_2();
        for (index, hop) in editor.hops.iter().enumerate() {
            let auth = hop.auth;
            let mut auth_row = h_flex().gap_1().items_center();
            for option in HopAuth::ALL {
                let active = option == auth;
                let selector = format!("conn-hop-auth-{index}-{}", option.label());
                auth_row = auth_row.child(
                    div()
                        .id(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .h(px(18.))
                        .px_2()
                        .rounded(px(4.))
                        .text_size(px(10.))
                        .cursor_pointer()
                        .when(active, |d| d.bg(hex(t.accent)).text_color(hex(t.on_accent)))
                        .when(!active, |d| {
                            d.text_color(hex(t.fg_secondary))
                                .hover(|s| s.bg(hex(t.bg_overlay)))
                        })
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if let Some(hop) = view
                                .conn_editor
                                .as_mut()
                                .and_then(|editor| editor.hops.get_mut(index))
                            {
                                hop.auth = option;
                            }
                            cx.notify();
                        }))
                        .child(SharedString::from(option.label())),
                );
            }
            let hop_index = index;
            hop_rows = hop_rows.child(
                v_flex()
                    .id(SharedString::from(format!("conn-hop-{index}")))
                    .debug_selector(move || format!("conn-hop-{index}"))
                    .p_2()
                    .gap_1()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(hex(t.border_subtle))
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(hex(t.fg_secondary))
                                    .child(SharedString::from(format!("第 {} 层", index + 1))),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("conn-hop-remove-{index}")))
                                    .debug_selector(move || format!("conn-hop-remove-{index}"))
                                    .text_size(px(10.5))
                                    .text_color(hex(t.danger))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        if let Some(editor) = view.conn_editor.as_mut() {
                                            if hop_index < editor.hops.len() {
                                                editor.hops.remove(hop_index);
                                            }
                                        }
                                        cx.notify();
                                    }))
                                    .child("移除"),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(conn_field(&t, "主机", &hop.host, px(150.)))
                            .child(conn_field(&t, "端口", &hop.port, px(60.)))
                            .child(conn_field(&t, "用户", &hop.user, px(90.))),
                    )
                    .child(auth_row)
                    .when(
                        matches!(auth, HopAuth::Key | HopAuth::KeyPassphrase),
                        |row| row.child(conn_field(&t, "私钥", &hop.key_path, px(220.))),
                    )
                    .when(
                        matches!(auth, HopAuth::KeyPassphrase | HopAuth::Password),
                        |row| row.child(conn_field(&t, "口令", &hop.secret, px(220.))),
                    ),
            );
        }
        // F-91a：代理类型与相对隧道的位置。
        let mut proxy_kind_row = h_flex().gap_1().items_center();
        for option in rustdb_db_api::ProxyKind::ALL {
            let active = option == proxy_kind;
            let selector = format!("conn-proxy-kind-{}", option.scheme());
            proxy_kind_row = proxy_kind_row.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .h(px(18.))
                    .px_2()
                    .rounded(px(4.))
                    .text_size(px(10.))
                    .cursor_pointer()
                    .when(active, |d| d.bg(hex(t.accent)).text_color(hex(t.on_accent)))
                    .when(!active, |d| {
                        d.text_color(hex(t.fg_secondary))
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(editor) = view.conn_editor.as_mut() {
                            editor.proxy_kind = option;
                        }
                        cx.notify();
                    }))
                    .child(SharedString::from(option.label())),
            );
        }
        let mut proxy_order_row = h_flex().gap_1().items_center();
        for option in rustdb_db_api::ProxyOrder::ALL {
            let active = option == proxy_order;
            let suffix = match option {
                rustdb_db_api::ProxyOrder::BeforeTunnel => "before",
                rustdb_db_api::ProxyOrder::AfterTunnel => "after",
            };
            let selector = format!("conn-proxy-order-{suffix}");
            proxy_order_row = proxy_order_row.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .h(px(18.))
                    .px_2()
                    .rounded(px(4.))
                    .text_size(px(10.))
                    .cursor_pointer()
                    .when(active, |d| d.bg(hex(t.accent)).text_color(hex(t.on_accent)))
                    .when(!active, |d| {
                        d.text_color(hex(t.fg_secondary))
                            .hover(|s| s.bg(hex(t.bg_overlay)))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(editor) = view.conn_editor.as_mut() {
                            editor.proxy_order = option;
                        }
                        cx.notify();
                    }))
                    .child(SharedString::from(option.label())),
            );
        }
        let body = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(conn_field(&t, "名称", &editor.name, px(180.)))
                    .child(conn_field(&t, "分组", &editor.folder, px(120.))),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(hex(t.fg_muted))
                            .child("驱动"),
                    )
                    .child(driver_row),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(conn_field(&t, "主机", &editor.host, px(180.)))
                    .child(conn_field(&t, "端口", &editor.port, px(70.))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(conn_field(&t, "数据库", &editor.database, px(180.)))
                    .child(conn_field(&t, "用户", &editor.username, px(120.))),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(conn_field(&t, "口令", &editor.password, px(180.)))
                    .child(conn_field(&t, "SSL", &editor.ssl_mode, px(100.))),
            )
            .child(
                h_flex()
                    .gap_4()
                    .items_center()
                    .child(
                        h_flex()
                            .id(SharedString::from("conn-toggle-readonly"))
                            .debug_selector(|| "conn-toggle-readonly".to_string())
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|view, _, _, cx| {
                                if let Some(editor) = view.conn_editor.as_mut() {
                                    editor.read_only = !editor.read_only;
                                }
                                cx.notify();
                            }))
                            .child(toggle_box(&t, read_only))
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(hex(t.fg_secondary))
                                    .child("只读连接"),
                            ),
                    )
                    .child(
                        h_flex()
                            .id(SharedString::from("conn-toggle-production"))
                            .debug_selector(|| "conn-toggle-production".to_string())
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|view, _, _, cx| {
                                if let Some(editor) = view.conn_editor.as_mut() {
                                    editor.production = !editor.production;
                                }
                                cx.notify();
                            }))
                            .child(toggle_box(&t, production))
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(hex(t.fg_secondary))
                                    .child("生产连接（写操作需临时解锁）"),
                            ),
                    ),
            )
            // F-91：SSH 隧道段。
            .child(
                v_flex()
                    .gap_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(hex(t.border_subtle))
                    .child(
                        h_flex()
                            .id(SharedString::from("conn-toggle-tunnel"))
                            .debug_selector(|| "conn-toggle-tunnel".to_string())
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|view, _, _, cx| {
                                if let Some(editor) = view.conn_editor.as_mut() {
                                    editor.tunnel = !editor.tunnel;
                                }
                                cx.notify();
                            }))
                            .child(toggle_box(&t, tunnel_on))
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(hex(t.fg_secondary))
                                    .child("走 SSH 隧道"),
                            ),
                    )
                    .when(tunnel_on, |section| {
                        section
                            .child(
                                div()
                                    .text_size(px(9.5))
                                    .text_color(hex(t.fg_muted))
                                    .child("跳板按从上到下顺序建链"),
                            )
                            .child(hop_rows)
                            .child(
                                div()
                                    .id("conn-hop-add")
                                    .debug_selector(|| "conn-hop-add".to_string())
                                    .text_size(px(10.5))
                                    .text_color(hex(t.accent))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        if let Some(editor) = view.conn_editor.as_mut() {
                                            editor.hops.push(HopEditor {
                                                host: conn_input(window, cx, "跳板主机", None),
                                                port: conn_input(window, cx, "22", Some("22")),
                                                user: conn_input(window, cx, "用户名", None),
                                                auth: HopAuth::Key,
                                                key_path: conn_input(
                                                    window,
                                                    cx,
                                                    "~/.ssh/id_ed25519",
                                                    None,
                                                ),
                                                secret: conn_input(
                                                    window,
                                                    cx,
                                                    "口令（只留在内存）",
                                                    None,
                                                ),
                                            });
                                        }
                                        cx.notify();
                                    }))
                                    .child("+ 添加跳板"),
                            )
                    }),
            )
            // F-91a：代理段。
            .child(
                v_flex()
                    .gap_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(hex(t.border_subtle))
                    .child(
                        h_flex()
                            .id(SharedString::from("conn-toggle-proxy"))
                            .debug_selector(|| "conn-toggle-proxy".to_string())
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|view, _, _, cx| {
                                if let Some(editor) = view.conn_editor.as_mut() {
                                    editor.proxy = !editor.proxy;
                                }
                                cx.notify();
                            }))
                            .child(toggle_box(&t, proxy_on))
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(hex(t.fg_secondary))
                                    .child("走代理（SOCKS5 / HTTP CONNECT）"),
                            ),
                    )
                    .when(proxy_on, |section| {
                        section
                            .child(proxy_kind_row)
                            .when(tunnel_on, |section| {
                                section.child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            div()
                                                .text_size(px(10.))
                                                .text_color(hex(t.fg_muted))
                                                .child("顺序"),
                                        )
                                        .child(proxy_order_row),
                                )
                            })
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(conn_field(&t, "主机", &editor.proxy_host, px(180.)))
                                    .child(conn_field(&t, "端口", &editor.proxy_port, px(70.))),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(conn_field(&t, "用户", &editor.proxy_username, px(120.)))
                                    .child(conn_field(
                                        &t,
                                        "口令",
                                        &editor.proxy_password,
                                        px(180.),
                                    )),
                            )
                            .when(proxy_kind == rustdb_db_api::ProxyKind::Socks5, |section| {
                                section.child(
                                    h_flex()
                                        .id(SharedString::from("conn-toggle-remote-dns"))
                                        .debug_selector(|| "conn-toggle-remote-dns".to_string())
                                        .gap_2()
                                        .items_center()
                                        .cursor_pointer()
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            if let Some(editor) = view.conn_editor.as_mut() {
                                                editor.proxy_remote_dns = !editor.proxy_remote_dns;
                                            }
                                            cx.notify();
                                        }))
                                        .child(toggle_box(&t, proxy_remote_dns))
                                        .child(
                                            div()
                                                .text_size(px(10.))
                                                .text_color(hex(t.fg_secondary))
                                                .child("域名交给代理解析"),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .text_size(px(9.5))
                                    .text_color(hex(t.fg_muted))
                                    .child("驱动仍连本地转发端口"),
                            )
                    }),
            )
            // F-91a：局域网监听开关（隧道 / 代理都适用，默认关闭）。
            .when(tunnel_on || proxy_on, |body| {
                body.child(
                    v_flex()
                        .gap_1()
                        .pt_2()
                        .border_t_1()
                        .border_color(hex(t.border_subtle))
                        .child(
                            h_flex()
                                .id(SharedString::from("conn-toggle-lan"))
                                .debug_selector(|| "conn-toggle-lan".to_string())
                                .gap_2()
                                .items_center()
                                .cursor_pointer()
                                .on_click(cx.listener(|view, _, _, cx| {
                                    if let Some(editor) = view.conn_editor.as_mut() {
                                        editor.forward_lan = !editor.forward_lan;
                                    }
                                    cx.notify();
                                }))
                                .child(toggle_box(&t, lan_on))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(hex(t.fg_secondary))
                                        .child("本地转发对局域网开放（监听 0.0.0.0）"),
                                ),
                        )
                        .when(lan_on, |section| {
                            section.child(
                                div().text_size(px(9.5)).text_color(hex(t.danger)).child(
                                    "同网段任何人都能借这条链路访问目标库，确需共享才打开。",
                                ),
                            )
                        }),
                )
            });
        v_flex()
            .id("conn-editor")
            .debug_selector(|| "conn-editor".to_string())
            .w(px(560.))
            .max_h(px(640.))
            .p_3()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(hex(t.border_subtle))
            .bg(hex(t.bg_elevated))
            // 卡片吃掉鼠标事件：否则「保存」校验失败时浮层会先被遮罩关掉。
            .occlude()
            .child(
                div()
                    .text_size(px(11.5))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hex(t.fg_primary))
                    .child(title),
            )
            .child(
                // 卡片高度随内容自适应，正文超过上限时自己滚动。
                div()
                    .max_h(px(470.))
                    .overflow_y_scrollbar()
                    .scroll_bounce(false)
                    .child(body),
            )
            .when(error.is_some(), |card| {
                card.child(
                    div()
                        .text_size(px(10.))
                        .text_color(hex(t.danger))
                        .child(SharedString::from(error.unwrap_or_default())),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        div()
                            .debug_selector(|| "conn-editor-cancel".to_string())
                            .child(
                                Button::new("conn-editor-cancel-btn")
                                    .small()
                                    .ghost()
                                    .label("取消")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.conn_editor = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "conn-editor-save".to_string())
                            .child(
                                Button::new("conn-editor-save-btn")
                                    .small()
                                    .primary()
                                    .label("保存")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.save_conn_editor(cx);
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}
