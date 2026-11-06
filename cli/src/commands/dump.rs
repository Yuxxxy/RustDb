//! `dump`：导出结构文档（DDL）。

use rustdb_db_api::ObjectSummary;

use crate::args::{Cli, Command, DocFormat};
use crate::commands::{
    connect_timeout, emit, find_profile, inject_secret, pick_namespace, META_TIMEOUT,
};
use crate::error::CliError;
use crate::runtime::Runtime;

/// 一个对象的 DDL 结果。
struct DdlEntry {
    object: ObjectSummary,
    ddl: Result<rustdb_db_api::ObjectDdl, String>,
}

pub async fn run(cli: &Cli, runtime: &mut Runtime) -> Result<(), CliError> {
    let options = match &cli.command {
        Command::Dump(options) => options.clone(),
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

    let selected: Vec<&ObjectSummary> = objects
        .iter()
        .filter(|object| {
            options
                .object
                .as_deref()
                .is_none_or(|wanted| object.path.name.eq_ignore_ascii_case(wanted))
        })
        .collect();
    if selected.is_empty() {
        runtime.core().disconnect(&session);
        let label = namespace.display_label();
        return Err(CliError::config(match options.object.as_deref() {
            Some(wanted) => format!("在 {label} 里找不到对象 {wanted}"),
            None => format!("{label} 里没有可导出的对象"),
        }));
    }

    let mut entries: Vec<DdlEntry> = Vec::with_capacity(selected.len());
    for object in selected {
        let ddl = runtime
            .ddl(&session, object.path.clone(), META_TIMEOUT)
            .await
            .map_err(|error| error.message);
        entries.push(DdlEntry {
            object: object.clone(),
            ddl,
        });
    }
    runtime.core().disconnect(&session);

    let format = if cli.json {
        DocFormat::Json
    } else {
        options.format
    };
    let text = match format {
        DocFormat::Json => json_text(&namespace.display_label(), &entries)?,
        DocFormat::Markdown => markdown_text(&namespace.display_label(), &entries),
        DocFormat::Sql => sql_text(&namespace.display_label(), &entries),
    };
    emit(&text, options.output.as_ref())
}

fn json_text(namespace: &str, entries: &[DdlEntry]) -> Result<String, CliError> {
    let objects: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            let (ddl, source, error) = match &entry.ddl {
                Ok(ddl) => (
                    serde_json::json!(ddl.text),
                    serde_json::json!(ddl.source.label()),
                    serde_json::Value::Null,
                ),
                Err(message) => (
                    serde_json::Value::Null,
                    serde_json::Value::Null,
                    serde_json::json!(message),
                ),
            };
            serde_json::json!({
                "name": entry.object.path.name,
                "kind": entry.object.path.kind.label(),
                "source": source,
                "error": error,
                "ddl": ddl,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "namespace": namespace,
        "count": objects.len(),
        "objects": objects,
    });
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&payload)
            .map_err(|error| CliError::internal(error.to_string()))?
    ))
}

fn sql_text(namespace: &str, entries: &[DdlEntry]) -> String {
    let mut out = String::new();
    out.push_str(&format!("-- 命名空间：{namespace}\n"));
    for entry in entries {
        out.push_str(&format!("\n-- {}\n", entry.object.path.display_label()));
        match &entry.ddl {
            Ok(ddl) => {
                out.push_str(ddl.text.trim_end());
                out.push_str(";\n");
            }
            Err(message) => out.push_str(&format!("-- （该对象不支持导出 DDL：{message}）\n")),
        }
    }
    out
}

fn markdown_text(namespace: &str, entries: &[DdlEntry]) -> String {
    let mut out = String::new();
    out.push_str("# 结构文档\n\n");
    out.push_str(&format!("- 命名空间：{namespace}\n"));
    out.push_str(&format!("- 对象数：{}\n", entries.len()));
    for entry in entries {
        out.push_str(&format!("\n## {}\n\n", entry.object.path.display_label()));
        match &entry.ddl {
            Ok(ddl) => {
                out.push_str(&format!("来源：{}\n\n", ddl.source.label()));
                out.push_str("```sql\n");
                out.push_str(ddl.text.trim_end());
                out.push_str("\n```\n");
            }
            Err(message) => out.push_str(&format!("（该对象不支持导出 DDL：{message}）\n")),
        }
    }
    out
}
