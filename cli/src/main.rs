//! RustDb 命令行入口（F-96）。
//!
//! 与桌面端共用 core：驱动只在 `rustdb-bootstrap` 注册，CLI 不直接持有驱动，
//! 只读 / 生产 / 隧道门控仍由 core 统一执行。

mod args;
mod commands;
mod error;
mod gate;
mod output;
mod runtime;

use std::process::ExitCode;

use args::{Cli, Command};
use error::CliError;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // 解析失败时也要按 `--json` 输出错误：脚本不依赖「参数是否被识别」。
    let json_hint = argv.iter().any(|arg| arg == "--json");
    let cli = match args::parse(&argv) {
        Ok(cli) => cli,
        Err(error) => return fail(&error, json_hint),
    };
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&error, cli.json),
    }
}

fn run(cli: &Cli) -> Result<(), CliError> {
    match &cli.command {
        Command::Help => {
            print!("{}", args::USAGE);
            Ok(())
        }
        Command::Version => {
            println!("RustDb CLI {}", rustdb_core::APP_VERSION);
            Ok(())
        }
        Command::Connections => {
            let core = commands::build_core(cli.connections.clone());
            commands::connections::run(cli, &core)
        }
        command => {
            let core = commands::build_core(cli.connections.clone());
            let mut runtime = runtime::Runtime::new(core);
            let handle = runtime.core().runtime_handle();
            handle.block_on(async {
                match command {
                    Command::Schema(_) => commands::schema::run(cli, &mut runtime).await,
                    Command::Query(_) => commands::query::run(cli, &mut runtime).await,
                    Command::Context(_) => commands::context::run(cli, &mut runtime).await,
                    Command::Dump(_) => commands::dump::run(cli, &mut runtime).await,
                    _ => Err(CliError::internal("未支持的命令")),
                }
            })
        }
    }
}

/// 失败：错误写 stderr（`--json` 时是 JSON 信封），退出码是稳定契约。
fn fail(error: &CliError, json: bool) -> ExitCode {
    if json {
        let payload = serde_json::json!({
            "error": { "code": error.code(), "message": error.message }
        });
        eprintln!(
            "{}",
            serde_json::to_string(&payload).unwrap_or_else(|_| error.to_string())
        );
    } else {
        eprintln!("rustdb-cli: error[{}]: {}", error.code(), error.message);
    }
    ExitCode::from(error.exit_code() as u8)
}
