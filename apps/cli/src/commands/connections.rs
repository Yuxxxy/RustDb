//! `connections`：列出连接配置。

use rustdb_core::Core;

use crate::args::Cli;
use crate::error::CliError;
use crate::output::grid;

/// 连接配置的对外视图（**不含**主机、口令、密钥引用）。
fn item(profile: &rustdb_core::ConnectionProfile) -> serde_json::Value {
    serde_json::json!({
        "id": profile.id,
        "name": profile.name,
        "driver": profile.driver.key(),
        "driver_name": profile.driver.display_name(),
        "endpoint": profile.endpoint_label(),
        "folder": profile.folder_name(),
        "read_only": profile.read_only,
        "production": profile.production,
        "tunnel": profile.tunnel.as_ref().is_some_and(|spec| !spec.hops.is_empty()),
    })
}

pub fn run(cli: &Cli, core: &Core) -> Result<(), CliError> {
    let profiles = core.profiles();
    if cli.json {
        let items: Vec<serde_json::Value> = profiles.iter().map(item).collect();
        let payload = serde_json::json!({ "connections": items });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|error| CliError::internal(error.to_string()))?
        );
        return Ok(());
    }

    let headers = vec![
        "id".to_string(),
        "名称".to_string(),
        "方言".to_string(),
        "地址".to_string(),
        "分组".to_string(),
        "只读".to_string(),
        "生产".to_string(),
        "隧道".to_string(),
    ];
    let rows: Vec<Vec<String>> = profiles
        .iter()
        .map(|profile| {
            vec![
                profile.id.clone(),
                profile.name.clone(),
                profile.driver.display_name().to_string(),
                profile.endpoint_label(),
                profile.folder_name().unwrap_or("-").to_string(),
                flag(profile.read_only),
                flag(profile.production),
                flag(
                    profile
                        .tunnel
                        .as_ref()
                        .is_some_and(|spec| !spec.hops.is_empty()),
                ),
            ]
        })
        .collect();
    print!("{}", grid(&headers, &rows));
    Ok(())
}

fn flag(value: bool) -> String {
    if value {
        "是".to_string()
    } else {
        "-".to_string()
    }
}
