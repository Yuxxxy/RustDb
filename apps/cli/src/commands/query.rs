//! `query`：执行 SQL 并输出结果。

use std::io::{IsTerminal, Read, Write};

use rustdb_core::export::{write_rows, ExportFormat, ExportOptions};
use rustdb_core::{Core, ResultSummary};

use crate::args::{Cli, Command, OutputFormat, QueryOptions};
use crate::commands::{connect_timeout, find_profile, inject_secret};
use crate::error::CliError;
use crate::gate::{self, WriteGate};
use crate::output::{cells, grid, rows_json};
use crate::runtime::{QueryOutcome, Runtime};

pub async fn run(cli: &Cli, runtime: &mut Runtime) -> Result<(), CliError> {
    let options = match &cli.command {
        Command::Query(options) => options.clone(),
        _ => return Err(CliError::internal("命令与参数不匹配")),
    };
    let profile = find_profile(runtime.core(), &options.connection)?;
    let sql = read_sql(&options)?;

    // 门控在连库之前：只读 / 缺开关的请求不碰数据库。
    gate::check(
        &sql,
        WriteGate {
            write: options.write,
            danger: options.danger,
        },
        &profile,
    )?;

    inject_secret(runtime.core(), &options.connection, &profile);
    let session = runtime
        .connect(&options.connection, connect_timeout(&profile))
        .await?;
    let outcome = runtime.execute(&session, &sql, options.timeout).await?;
    render(cli, &options, profile.driver, &outcome, runtime.core())?;
    runtime.core().disconnect(&session);
    Ok(())
}

/// SQL 来源：位置参数 / `--file` / 标准输入。
fn read_sql(options: &QueryOptions) -> Result<String, CliError> {
    if let Some(path) = &options.file {
        return std::fs::read_to_string(path)
            .map_err(|error| CliError::io(format!("读取 {} 失败：{error}", path.display())));
    }
    if let Some(sql) = &options.sql {
        return Ok(sql.clone());
    }
    if std::io::stdin().is_terminal() {
        return Err(CliError::usage(
            "缺少 SQL：位置参数 / --file / 标准输入三选一",
        ));
    }
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|error| CliError::io(format!("读取标准输入失败：{error}")))?;
    Ok(text)
}

/// 结果集的展示数据。
struct SetView {
    columns: Vec<String>,
    rows: Vec<rustdb_db_api::Row>,
    summary: ResultSummary,
}

fn collect(core: &Core, outcome: &QueryOutcome, limit: usize) -> Vec<SetView> {
    let store = core.results();
    outcome
        .results
        .iter()
        .map(|summary| {
            let columns = store
                .meta(summary.set_id)
                .map(|meta| meta.columns.iter().map(|c| c.name.to_string()).collect())
                .unwrap_or_default();
            let take = if limit == 0 {
                summary.rows as usize
            } else {
                limit
            };
            let rows = store.rows(summary.set_id, 0, take);
            SetView {
                columns,
                rows,
                summary: summary.clone(),
            }
        })
        .collect()
}

fn render(
    cli: &Cli,
    options: &QueryOptions,
    dialect: rustdb_db_api::DbKind,
    outcome: &QueryOutcome,
    core: &Core,
) -> Result<(), CliError> {
    let format = if cli.json {
        OutputFormat::Json
    } else {
        options.format
    };
    let sets = collect(core, outcome, options.limit);

    match format {
        OutputFormat::Json => {
            let mut results = Vec::with_capacity(sets.len());
            for set in &sets {
                let rows = rows_json(&set.columns, &set.rows, &export_options(dialect, "NULL"));
                results.push(serde_json::json!({
                    "columns": set.columns,
                    "row_count": set.summary.rows,
                    "shown": set.rows.len(),
                    "truncated": set.summary.truncated || (set.rows.len() as u64) < set.summary.rows,
                    "affected": set.summary.affected,
                    "rows": rows,
                }));
            }
            let payload = serde_json::json!({
                "elapsed_ms": outcome.elapsed_ms,
                "warnings": outcome.warnings,
                "notices": outcome.notices,
                "results": results,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&payload)
                    .map_err(|error| CliError::internal(error.to_string()))?
            );
        }
        OutputFormat::Table => {
            let options = export_options(dialect, "NULL");
            for (index, set) in sets.iter().enumerate() {
                if sets.len() > 1 {
                    println!("── 结果集 {}/{} ──", index + 1, sets.len());
                }
                if set.columns.is_empty() {
                    match set.summary.affected {
                        Some(affected) => println!("影响行数：{affected}"),
                        None => println!("（无结果集）"),
                    }
                    continue;
                }
                let headers = set.columns.clone();
                let rows: Vec<Vec<String>> =
                    set.rows.iter().map(|row| cells(row, &options)).collect();
                print!("{}", grid(&headers, &rows));
                if set.summary.truncated || (set.rows.len() as u64) < set.summary.rows {
                    println!(
                        "（已截断：共 {} 行，仅显示前 {} 行）",
                        set.summary.rows,
                        set.rows.len()
                    );
                }
            }
        }
        OutputFormat::Csv | OutputFormat::Tsv | OutputFormat::Markdown => {
            let export_format = match format {
                OutputFormat::Csv => ExportFormat::Csv,
                OutputFormat::Tsv => ExportFormat::Tsv,
                _ => ExportFormat::Markdown,
            };
            let options = export_options(dialect, "");
            let stdout = std::io::stdout();
            let mut writer = stdout.lock();
            for (index, set) in sets.iter().enumerate() {
                if index > 0 {
                    writeln!(writer).map_err(CliError::from)?;
                }
                if set.columns.is_empty() {
                    continue;
                }
                write_rows(
                    &mut writer,
                    export_format,
                    &set.columns,
                    set.rows.clone(),
                    &options,
                )
                .map_err(|error| CliError::io(error.to_string()))?;
            }
            writer.flush().map_err(CliError::from)?;
        }
    }
    Ok(())
}

/// 导出选项：NULL 在 CSV / TSV 里是空字段，在表格里显示 `NULL`。
fn export_options(dialect: rustdb_db_api::DbKind, null_text: &str) -> ExportOptions {
    ExportOptions {
        dialect,
        include_header: true,
        null_text: null_text.to_string(),
        formula_guard: true,
        line_ending: "\n".to_string(),
        ..ExportOptions::default()
    }
}
