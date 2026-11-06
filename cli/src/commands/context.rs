//! `context`：导出 AI 上下文（Schema 摘要 + 列信息）。
//!
//! 按需求 19.5：上下文里**不含**主机、账号、口令与密钥引用。

use rustdb_db_api::{ObjectSummary, TableDetail};

use crate::args::{Cli, Command, DocFormat};
use crate::commands::{
    connect_timeout, emit, find_profile, inject_secret, pick_namespace, META_TIMEOUT,
};
use crate::error::CliError;
use crate::runtime::Runtime;

/// 一个对象及其列信息。
struct ContextObject {
    object: ObjectSummary,
    detail: Result<TableDetail, String>,
}

pub async fn run(cli: &Cli, runtime: &mut Runtime) -> Result<(), CliError> {
    let options = match &cli.command {
        Command::Context(options) => options.clone(),
        _ => return Err(CliError::internal("命令与参数不匹配")),
    };
    let profile = find_profile(runtime.core(), &options.connection)?;
    inject_secret(runtime.core(), &options.connection, &profile);
    let timeout = connect_timeout(&profile);
    let session = runtime.connect(&options.connection, timeout).await?;
    let namespaces = runtime.wait_namespaces(timeout).await?;
    let namespace = pick_namespace(runtime.core(), &namespaces, options.namespace.as_deref())?;
    let objects = runtime
        .load_objects(&session, namespace.clone(), timeout)
        .await?;

    let selected: Vec<&ObjectSummary> = objects.iter().take(options.max_objects).collect();
    let truncated = objects.len() > selected.len();
    let mut entries: Vec<ContextObject> = Vec::with_capacity(selected.len());
    for object in selected {
        let detail = runtime
            .table_detail(&session, object.path.clone(), META_TIMEOUT)
            .await
            .map_err(|error| error.message);
        entries.push(ContextObject {
            object: object.clone(),
            detail,
        });
    }
    runtime.core().disconnect(&session);

    let format = if cli.json {
        DocFormat::Json
    } else {
        options.format
    };
    let text = match format {
        DocFormat::Json => json_text(&profile, &namespace.display_label(), &entries, truncated)?,
        _ => markdown_text(&profile, &namespace.display_label(), &entries, truncated),
    };
    emit(&text, options.output.as_ref())
}

fn json_text(
    profile: &rustdb_core::ConnectionProfile,
    namespace: &str,
    entries: &[ContextObject],
    truncated: bool,
) -> Result<String, CliError> {
    let objects: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            let (columns, detail_error) = match &entry.detail {
                Ok(detail) => {
                    let columns = detail
                        .columns
                        .iter()
                        .map(|column| {
                            serde_json::json!({
                                "name": column.meta.name,
                                "type": column.meta.logical_type.label(),
                                "nullable": column.meta.nullable,
                                "primary_key": column.is_primary_key,
                                "auto_increment": column.is_auto_increment,
                                "default": column.default,
                                "comment": column.comment,
                            })
                        })
                        .collect::<Vec<_>>();
                    (columns, serde_json::Value::Null)
                }
                Err(message) => (Vec::new(), serde_json::json!(message)),
            };
            serde_json::json!({
                "name": entry.object.path.name,
                "kind": entry.object.path.kind.label(),
                "row_estimate": entry.object.row_estimate,
                "comment": entry.object.comment,
                "columns": columns,
                "detail_error": detail_error,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "connection": {
            "id": profile.id,
            "name": profile.name,
            "driver": profile.driver.key(),
        },
        "namespace": namespace,
        "object_count": objects.len(),
        "truncated": truncated,
        "objects": objects,
    });
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&payload)
            .map_err(|error| CliError::internal(error.to_string()))?
    ))
}

fn markdown_text(
    profile: &rustdb_core::ConnectionProfile,
    namespace: &str,
    entries: &[ContextObject],
    truncated: bool,
) -> String {
    let mut out = String::new();
    out.push_str("# 数据库结构上下文\n\n");
    out.push_str(&format!("- 连接：{}（{}）\n", profile.id, profile.name));
    out.push_str(&format!("- 方言：{}\n", profile.driver.display_name()));
    out.push_str(&format!("- 命名空间：{namespace}\n"));
    out.push_str(&format!("- 对象数：{}\n", entries.len()));
    if truncated {
        out.push_str("- 注意：对象过多，已按上限截断\n");
    }
    out.push_str("\n## 对象\n\n");
    out.push_str("| 对象 | 类型 | 行数估计 | 注释 |\n");
    out.push_str("| --- | --- | --- | --- |\n");
    for entry in entries {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            entry.object.path.name,
            entry.object.path.kind.label(),
            entry
                .object
                .row_estimate
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".into()),
            entry.object.comment.clone().unwrap_or_else(|| "-".into()),
        ));
    }

    for entry in entries {
        out.push_str(&format!("\n### {}\n\n", entry.object.path.name));
        match &entry.detail {
            Err(message) => {
                out.push_str(&format!("（列信息不可用：{message}）\n"));
            }
            Ok(detail) => {
                if detail.columns.is_empty() {
                    out.push_str("（无列信息）\n");
                    continue;
                }
                out.push_str("| 列 | 类型 | 主键 | 可空 | 默认值 | 注释 |\n");
                out.push_str("| --- | --- | --- | --- | --- | --- |\n");
                for column in &detail.columns {
                    out.push_str(&format!(
                        "| {} | {} | {} | {} | {} | {} |\n",
                        column.meta.name,
                        column.meta.logical_type.label(),
                        if column.is_primary_key { "是" } else { "否" },
                        if column.meta.nullable { "是" } else { "否" },
                        column.default.clone().unwrap_or_else(|| "-".into()),
                        column.comment.clone().unwrap_or_else(|| "-".into()),
                    ));
                }
            }
        }
    }
    out
}
