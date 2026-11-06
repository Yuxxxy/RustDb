//! RustDb 组合根：初始化日志、组装 Core、启动窗口。
//!
//! 驱动注册与仓库组装在 `rustdb-bootstrap` 里，桌面端与 CLI 共用同一套。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    rustdb_platform::logging::init(log::LevelFilter::Info);

    let core = rustdb_bootstrap::build_core();
    log::info!(
        "已注册 {} 个驱动，载入 {} 条查询历史，{} 个连接配置",
        core.drivers().len(),
        core.history().len(),
        core.profiles().len()
    );

    rustdb_shell::app::run(core);
}
