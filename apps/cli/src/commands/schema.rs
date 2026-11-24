//! `schema`：浏览命名空间与对象。

use rustdb_db_api::ObjectSummary;

use crate::args::{Cli, Command};
use crate::commands::{connect_timeout, find_profile, inject_secret, pick_namespace};
use crate::error::CliError;
use crate::output::grid;
use crate::runtime::Runtime;

pub async fn run(cli: &Cli, runtime: &mut Runtime) -> Result<(), CliError> {
    let options = match &cli.command {
        Command::Schema(options) => options.clone(),
        _ => return Err(CliError::internal("命令与参数不匹配")),
    };
    let profile = find_profile(runtime.core(), &options.connection)?;
    inject_secret(runtime.core(), &options.connection, &profile);
    let timeout = connect_timeout(&profile);
    let session = runtime.connect(&options.connection, timeout).await?;
    let namespaces = runtime.wait_namespaces(timeout).await?;

    let result = match options.namespace.as_deref() {
        None => list_namespaces(cli, &namespaces),
        Some(label) => {
            let namespace = pick_namespace(runtime.core(), &namespaces, Some(label))?;
            let objects = runtime
                .load_objects(&session, namespace.clone(), timeout)
                .await?;
            list_objects(
                cli,
                &namespace.display_label(),
                &objects,
                options.kind.as_deref(),
                options.filter.as_deref(),
            )
        }
    };
    runtime.core().disconnect(&session);
    result
}

fn list_namespaces(cli: &Cli, namespaces: &[rustdb_db_api::Namespace]) -> Result<(), CliError> {
    if cli.json {
        let items: Vec<serde_json::Value> = namespaces
            .iter()
            .map(|namespace| {
                serde_json::json!({
                    "name": namespace.display_label(),
                    "catalog": namespace.catalog,
                    "schema": namespace.schema,
                })
            })
            .collect();
        let payload = serde_json::json!({ "namespaces": items });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|error| CliError::internal(error.to_string()))?
        );
        return Ok(());
    }
    let headers = vec![
        "命名空间".to_string(),
        "catalog".to_string(),
        "schema".to_string(),
    ];
    let rows: Vec<Vec<String>> = namespaces
        .iter()
        .map(|namespace| {
            vec![
                namespace.display_label(),
                namespace.catalog.as_deref().unwrap_or("-").to_string(),
                namespace.schema.as_deref().unwrap_or("-").to_string(),
            ]
        })
        .collect();
    print!("{}", grid(&headers, &rows));
    Ok(())
}

fn list_objects(
    cli: &Cli,
    namespace: &str,
    objects: &[ObjectSummary],
    kind: Option<&str>,
    filter: Option<&str>,
) -> Result<(), CliError> {
    let selected: Vec<&ObjectSummary> = objects
        .iter()
        .filter(|object| {
            kind.is_none_or(|kind| object.path.kind.label().eq_ignore_ascii_case(kind))
                && filter.is_none_or(|filter| {
                    object
                        .path
                        .name
                        .to_lowercase()
                        .contains(&filter.to_lowercase())
                })
        })
        .collect();

    if cli.json {
        let items: Vec<serde_json::Value> = selected
            .iter()
            .map(|object| {
                serde_json::json!({
                    "name": object.path.name,
                    "kind": object.path.kind.label(),
                    "row_estimate": object.row_estimate,
                    "comment": object.comment,
                    "primary_key": object.has_primary_key,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "namespace": namespace,
            "count": items.len(),
            "objects": items,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|error| CliError::internal(error.to_string()))?
        );
        return Ok(());
    }

    let headers = vec![
        "对象".to_string(),
        "类型".to_string(),
        "行数估计".to_string(),
        "主键".to_string(),
        "注释".to_string(),
    ];
    let rows: Vec<Vec<String>> = selected
        .iter()
        .map(|object| {
            vec![
                object.path.name.to_string(),
                object.path.kind.label().to_string(),
                object
                    .row_estimate
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".into()),
                match object.has_primary_key {
                    Some(true) => "是".to_string(),
                    Some(false) => "否".to_string(),
                    None => "-".to_string(),
                },
                object.comment.clone().unwrap_or_else(|| "-".into()),
            ]
        })
        .collect();
    print!("{}", grid(&headers, &rows));
    Ok(())
}
