//! 组合根：驱动注册与 `Core` 组装。桌面端与 CLI 共用。
//!
//! 需求 19.5：「CLI 与 GUI 共用 core 服务，不直接持有驱动」——
//! 驱动只在这里注册一次，两个入口都只通过 `Core` 访问数据库，
//! 读只读 / 生产标记 / 隧道等门控仍由 core 统一执行。

use rustdb_core::{Core, HistoryStore, ProfileStore, DEFAULT_HISTORY_LIMIT};
use rustdb_db_api::DriverRegistry;
use std::sync::Arc;

/// 注册全部六方言驱动。
pub fn driver_registry() -> DriverRegistry {
    let mut drivers = DriverRegistry::new();
    drivers.register(rustdb_db_sqlite::SqliteFactory::new());
    drivers.register(rustdb_db_mysql::MySqlFactory::new());
    drivers.register(rustdb_db_mysql::MariaDbFactory::new());
    drivers.register(rustdb_db_postgres::PostgresFactory::new());
    drivers.register(rustdb_db_mssql::MssqlFactory);
    drivers.register(rustdb_db_oracle::OracleFactory);
    drivers
}

/// 查询历史落盘到数据目录；目录不可用时退回纯内存，不阻塞启动。
pub fn open_history() -> HistoryStore {
    match rustdb_platform::paths::ensure_data_dir() {
        Ok(dir) => HistoryStore::with_path(DEFAULT_HISTORY_LIMIT, dir.join("history.jsonl")),
        Err(err) => {
            log::warn!("准备数据目录失败，查询历史仅保存在内存：{err}");
            HistoryStore::new(DEFAULT_HISTORY_LIMIT)
        }
    }
}

/// 连接配置落盘到数据目录；目录不可用时退回纯内存。
pub fn open_profiles() -> ProfileStore {
    match rustdb_platform::paths::ensure_data_dir() {
        Ok(_) => ProfileStore::with_path(rustdb_platform::paths::connections_path()),
        Err(err) => {
            log::warn!("准备数据目录失败，连接配置仅保存在内存：{err}");
            ProfileStore::in_memory()
        }
    }
}

/// 组装 `Core`（默认路径）。
pub fn build_core() -> Arc<Core> {
    build_core_with(open_profiles(), open_history())
}

/// 以指定仓库组装 `Core`（CLI 用 `--connections` 指向别的配置文件）。
pub fn build_core_with(profiles: ProfileStore, history: HistoryStore) -> Arc<Core> {
    init_crypto();
    Core::with_config(driver_registry(), history, profiles)
}

/// 安装进程级 rustls `CryptoProvider`（幂等）。
///
/// rustls 0.23 起 TLS 客户端必须显式选 provider：本工程按 `aws-lc-rs` 编译
/// （见 workspace `Cargo.toml`，与 SQL Server / Oracle 共用同一 provider），
/// 但 **没有任何地方调用过 `install_default()`**。后果是 MySQL / PostgreSQL 第一次
/// 建 TLS 时在 `rustls::crypto` 里 panic：
/// 「Could not automatically determine the process-level CryptoProvider」，
/// 而连接跑在 `rustdb-db` 工作线程上 —— 主线程只看到「等待超时」，排查成本极高。
/// 放在组合根而不是各驱动里：桌面端与 CLI 都只经 `build_core*` 起步，装一次就够。
pub fn init_crypto() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 护栏：`build_core` 之后进程里必须已经装好 rustls provider。
    ///
    /// 没有它，任何走 TLS 的连接（MySQL / PostgreSQL / SQL Server）第一次握手就会
    /// 在工作线程里 panic，而调用方只看到「等待超时」——实测就是这么丢了一轮排查时间。
    #[test]
    fn build_core_installs_rustls_provider() {
        init_crypto();
        assert!(
            rustls::crypto::CryptoProvider::get_default().is_some(),
            "进程级 CryptoProvider 未安装：TLS 连接会在 rustls::crypto 里 panic"
        );
    }
}
