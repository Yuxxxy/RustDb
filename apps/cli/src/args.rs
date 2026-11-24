//! 命令行参数解析（F-96）。
//!
//! 手写解析：参数面很小，但错误码必须稳定可预期，不引入额外依赖。

use std::path::PathBuf;
use std::time::Duration;

use crate::error::CliError;

pub const USAGE: &str = "\
RustDb 命令行工具（F-96）

用法：
  rustdb-cli <命令> [选项]

命令：
  connections            列出连接配置
  schema                 浏览 Schema（命名空间与对象）
  query                  执行 SQL 并输出结果
  context                导出 AI 上下文（Schema 摘要 + 列信息）
  dump                   导出结构文档（DDL）
  version                版本信息

全局选项：
  --json                 以 JSON 输出（脚本友好）
  --connections <路径>   连接配置文件（默认：数据目录下的 connections.json）
  -h, --help             显示本帮助
  -V, --version          显示版本

query 选项：
  -c, --connection <id>  连接配置 id（必填）
  -f, --file <路径>      从文件读取 SQL（不传则用位置参数或标准输入）
  --format <格式>        table（默认） / csv / tsv / json / markdown
  --limit <行数>         最多输出多少行（0 = 全部，默认 1000）
  --timeout <秒>         等待结果的上限（0 = 不限，默认 0）
  --write                允许写语句（默认只读）
  --danger               允许危险语句（DELETE / DROP / TRUNCATE / 无 WHERE 的改删）

schema / context / dump 选项：
  -c, --connection <id>  连接配置 id（必填）
  --namespace <名称>     指定命名空间（默认：当前库）
  --output <路径>        写入文件（默认：标准输出）

退出码：
  0 成功   2 用法错误   3 配置错误   4 连接失败   5 权限拒绝
  6 查询失败   7 超时   8 文件读写失败   1 内部错误
";

/// 结果输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Csv,
    Tsv,
    Json,
    Markdown,
}

impl OutputFormat {
    pub fn parse(text: &str) -> Result<Self, CliError> {
        match text.to_ascii_lowercase().as_str() {
            "table" => Ok(Self::Table),
            "csv" => Ok(Self::Csv),
            "tsv" => Ok(Self::Tsv),
            "json" => Ok(Self::Json),
            "markdown" | "md" => Ok(Self::Markdown),
            other => Err(CliError::usage(format!(
                "未知的输出格式 {other}（可选 table / csv / tsv / json / markdown）"
            ))),
        }
    }
}

/// 文档输出格式（上下文 / 结构文档）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFormat {
    Markdown,
    Json,
    Sql,
}

impl DocFormat {
    pub fn parse(text: &str, allowed: &[&str]) -> Result<Self, CliError> {
        match text.to_ascii_lowercase().as_str() {
            "markdown" | "md" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            "sql" if allowed.contains(&"sql") => Ok(Self::Sql),
            other => Err(CliError::usage(format!(
                "未知的文档格式 {other}（可选 {}）",
                allowed.join(" / ")
            ))),
        }
    }
}

/// 解析结果。
#[derive(Debug, Clone)]
pub struct Cli {
    pub command: Command,
    /// 全局 `--json`。
    pub json: bool,
    /// 连接配置文件路径（`None` = 数据目录默认位置）。
    pub connections: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum Command {
    Connections,
    Schema(SchemaOptions),
    Query(QueryOptions),
    Context(ContextOptions),
    Dump(DumpOptions),
    Version,
    Help,
}

#[derive(Debug, Clone, Default)]
pub struct SchemaOptions {
    pub connection: String,
    pub namespace: Option<String>,
    pub kind: Option<String>,
    pub filter: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub connection: String,
    /// 位置参数里的 SQL。
    pub sql: Option<String>,
    pub file: Option<PathBuf>,
    pub format: OutputFormat,
    pub limit: usize,
    pub write: bool,
    pub danger: bool,
    pub timeout: Duration,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            connection: String::new(),
            sql: None,
            file: None,
            format: OutputFormat::Table,
            limit: 1000,
            write: false,
            danger: false,
            timeout: Duration::ZERO,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContextOptions {
    pub connection: String,
    pub namespace: Option<String>,
    pub format: DocFormat,
    pub output: Option<PathBuf>,
    pub max_objects: usize,
}

#[derive(Debug, Clone)]
pub struct DumpOptions {
    pub connection: String,
    pub namespace: Option<String>,
    pub object: Option<String>,
    pub format: DocFormat,
    pub output: Option<PathBuf>,
}

/// 解析入口。
pub fn parse(argv: &[String]) -> Result<Cli, CliError> {
    let (json, connections, rest) = split_globals(argv)?;

    let Some((name, tail)) = rest.split_first() else {
        return Err(CliError::usage("缺少命令"));
    };

    let command = match name.as_str() {
        "connections" => Command::Connections,
        "schema" => Command::Schema(parse_schema(tail)?),
        "query" => Command::Query(parse_query(tail)?),
        "context" => Command::Context(parse_context(tail)?),
        "dump" => Command::Dump(parse_dump(tail)?),
        "version" => Command::Version,
        "help" => Command::Help,
        "--help" | "-h" => Command::Help,
        "--version" | "-V" => Command::Version,
        other => {
            return Err(CliError::usage(format!(
                "未知命令 {other}（可用：connections / schema / query / context / dump）"
            )))
        }
    };

    Ok(Cli {
        command,
        json,
        connections,
    })
}

/// 拆出全局选项，剩下的交给子命令解析。
fn split_globals(argv: &[String]) -> Result<(bool, Option<PathBuf>, Vec<String>), CliError> {
    let mut json = false;
    let mut connections = None;
    let mut rest = Vec::with_capacity(argv.len());
    let mut index = 0;
    while index < argv.len() {
        let arg = argv[index].as_str();
        match arg {
            "--json" => json = true,
            "--connections" => {
                let value = argv
                    .get(index + 1)
                    .ok_or_else(|| CliError::usage("--connections 需要一个路径"))?;
                connections = Some(PathBuf::from(value));
                index += 1;
            }
            _ => {
                if let Some(value) = arg.strip_prefix("--connections=") {
                    connections = Some(PathBuf::from(value));
                } else {
                    rest.push(argv[index].clone());
                }
            }
        }
        index += 1;
    }
    Ok((json, connections, rest))
}

/// 取 `--flag value` 或 `--flag=value` 的值；没有值时吃掉下一个参数。
fn take_value(
    args: &[String],
    index: &mut usize,
    flag: &str,
    inline: Option<&str>,
) -> Result<String, CliError> {
    if let Some(value) = inline {
        return Ok(value.to_string());
    }
    let value = args
        .get(*index + 1)
        .ok_or_else(|| CliError::usage(format!("{flag} 需要一个值")))?;
    *index += 1;
    Ok(value.clone())
}

/// 拆 `--flag=value`：长选项返回裸名（`write`），短选项原样返回（`-c`）。
fn split_inline(arg: &str) -> (&str, Option<&str>) {
    match arg.strip_prefix("--") {
        Some(rest) => match rest.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (rest, None),
        },
        None => (arg, None),
    }
}

fn parse_schema(args: &[String]) -> Result<SchemaOptions, CliError> {
    let mut options = SchemaOptions::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].clone();
        let (name, inline) = split_inline(&arg);
        match name {
            "connection" | "-c" => {
                options.connection = take_value(args, &mut index, "--connection", inline)?;
            }
            "namespace" | "-n" => {
                options.namespace = Some(take_value(args, &mut index, "--namespace", inline)?);
            }
            "kind" | "-k" => {
                options.kind = Some(take_value(args, &mut index, "--kind", inline)?);
            }
            "filter" => {
                options.filter = Some(take_value(args, &mut index, "--filter", inline)?);
            }
            _other => {
                return Err(CliError::usage(format!(
                    "schema 不支持选项 {arg}（可用 --connection / --namespace / --kind / --filter）"
                )))
            }
        }
        index += 1;
    }
    if options.connection.trim().is_empty() {
        return Err(CliError::usage("缺少 --connection"));
    }
    Ok(options)
}

fn parse_query(args: &[String]) -> Result<QueryOptions, CliError> {
    let mut options = QueryOptions::default();
    let mut sql: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].clone();
        if !arg.starts_with('-') {
            if sql.is_some() {
                return Err(CliError::usage("只接受一条 SQL（多条请用分号分隔）"));
            }
            sql = Some(arg);
            index += 1;
            continue;
        }
        let (name, inline) = split_inline(&arg);
        match name {
            "connection" | "-c" => {
                options.connection = take_value(args, &mut index, "--connection", inline)?;
            }
            "file" | "-f" => {
                options.file = Some(PathBuf::from(take_value(args, &mut index, "--file", inline)?));
            }
            "format" => {
                options.format = OutputFormat::parse(&take_value(args, &mut index, "--format", inline)?)?;
            }
            "limit" => {
                let raw = take_value(args, &mut index, "--limit", inline)?;
                options.limit = raw
                    .parse::<usize>()
                    .map_err(|_| CliError::usage(format!("--limit 需要一个非负整数，收到 {raw}")))?;
            }
            "timeout" => {
                let raw = take_value(args, &mut index, "--timeout", inline)?;
                let seconds = raw
                    .parse::<u64>()
                    .map_err(|_| CliError::usage(format!("--timeout 需要秒数，收到 {raw}")))?;
                options.timeout = Duration::from_secs(seconds);
            }
            "write" => options.write = true,
            "danger" => options.danger = true,
            _other => {
                return Err(CliError::usage(format!(
                    "query 不支持选项 {arg}（可用 --connection / --file / --format / --limit / --timeout / --write / --danger）"
                )))
            }
        }
        index += 1;
    }
    if options.connection.trim().is_empty() {
        return Err(CliError::usage("缺少 --connection"));
    }
    options.sql = sql;
    Ok(options)
}

fn parse_context(args: &[String]) -> Result<ContextOptions, CliError> {
    let mut options = ContextOptions {
        connection: String::new(),
        namespace: None,
        format: DocFormat::Markdown,
        output: None,
        max_objects: 50,
    };
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].clone();
        let (name, inline) = split_inline(&arg);
        match name {
            "connection" | "-c" => {
                options.connection = take_value(args, &mut index, "--connection", inline)?;
            }
            "namespace" | "-n" => {
                options.namespace = Some(take_value(args, &mut index, "--namespace", inline)?);
            }
            "format" => {
                options.format =
                    DocFormat::parse(&take_value(args, &mut index, "--format", inline)?, &["markdown", "json"])?;
            }
            "output" | "-o" => {
                options.output = Some(PathBuf::from(take_value(args, &mut index, "--output", inline)?));
            }
            "max-objects" => {
                let raw = take_value(args, &mut index, "--max-objects", inline)?;
                options.max_objects = raw
                    .parse::<usize>()
                    .map_err(|_| CliError::usage(format!("--max-objects 需要一个非负整数，收到 {raw}")))?;
            }
            _other => {
                return Err(CliError::usage(format!(
                    "context 不支持选项 {arg}（可用 --connection / --namespace / --format / --output / --max-objects）"
                )))
            }
        }
        index += 1;
    }
    if options.connection.trim().is_empty() {
        return Err(CliError::usage("缺少 --connection"));
    }
    Ok(options)
}

fn parse_dump(args: &[String]) -> Result<DumpOptions, CliError> {
    let mut options = DumpOptions {
        connection: String::new(),
        namespace: None,
        object: None,
        format: DocFormat::Sql,
        output: None,
    };
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].clone();
        let (name, inline) = split_inline(&arg);
        match name {
            "connection" | "-c" => {
                options.connection = take_value(args, &mut index, "--connection", inline)?;
            }
            "namespace" | "-n" => {
                options.namespace = Some(take_value(args, &mut index, "--namespace", inline)?);
            }
            "object" | "-o" => {
                options.object = Some(take_value(args, &mut index, "--object", inline)?);
            }
            "format" => {
                options.format =
                    DocFormat::parse(&take_value(args, &mut index, "--format", inline)?, &["sql", "markdown", "json"])?;
            }
            "output" => {
                options.output = Some(PathBuf::from(take_value(args, &mut index, "--output", inline)?));
            }
            _other => {
                return Err(CliError::usage(format!(
                    "dump 不支持选项 {arg}（可用 --connection / --namespace / --object / --format / --output）"
                )))
            }
        }
        index += 1;
    }
    if options.connection.trim().is_empty() {
        return Err(CliError::usage("缺少 --connection"));
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn query_flags_are_parsed() {
        let cli = parse(&args(&[
            "--json", "query", "-c", "demo", "--format", "csv", "--limit", "5", "--write",
            "--danger", "SELECT 1",
        ]))
        .expect("解析");
        assert!(cli.json);
        let Command::Query(options) = cli.command else {
            panic!("应为 query");
        };
        assert_eq!(options.connection, "demo");
        assert_eq!(options.format, OutputFormat::Csv);
        assert_eq!(options.limit, 5);
        assert!(options.write);
        assert!(options.danger);
        assert_eq!(options.sql.as_deref(), Some("SELECT 1"));
    }

    #[test]
    fn inline_values_are_supported() {
        let cli = parse(&args(&["query", "--connection=demo", "--limit=0"])).expect("解析");
        let Command::Query(options) = cli.command else {
            panic!("应为 query");
        };
        assert_eq!(options.connection, "demo");
        assert_eq!(options.limit, 0);
    }

    #[test]
    fn unknown_command_is_a_usage_error() {
        let error = parse(&args(&["nope"])).expect_err("应报错");
        assert_eq!(error.code(), "E_USAGE");
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn unknown_flag_is_a_usage_error() {
        let error = parse(&args(&["query", "-c", "demo", "--oops"])).expect_err("应报错");
        assert_eq!(error.code(), "E_USAGE");
    }

    #[test]
    fn missing_connection_is_a_usage_error() {
        let error = parse(&args(&["query", "SELECT 1"])).expect_err("应报错");
        assert_eq!(error.code(), "E_USAGE");
        assert!(error.message.contains("--connection"));
    }

    #[test]
    fn connections_path_is_global() {
        let cli = parse(&args(&["--connections", "/tmp/a.json", "connections"])).expect("解析");
        assert_eq!(cli.connections, Some(PathBuf::from("/tmp/a.json")));
    }

    #[test]
    fn help_and_version_are_recognized() {
        assert!(matches!(
            parse(&args(&["--help"])).expect("解析").command,
            Command::Help
        ));
        assert!(matches!(
            parse(&args(&["version"])).expect("解析").command,
            Command::Version
        ));
    }

    #[test]
    fn limit_must_be_a_number() {
        let error = parse(&args(&["query", "-c", "demo", "--limit", "x"])).expect_err("应报错");
        assert_eq!(error.code(), "E_USAGE");
    }
}
