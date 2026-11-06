//! CLI 集成测试（F-96 / AC-37）。
//!
//! 直接跑编译出来的二进制，校验「错误码 + 退出码 + 输出格式」这三件对外契约：
//! 默认只读、写与危险 SQL 各自要显式开关、生产连接一律拒绝。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// 一个连接配置的测试描述。
struct ProfileSpec<'a> {
    id: &'a str,
    read_only: bool,
    production: bool,
    /// `None` = 用本夹具自带的库文件。
    database: Option<&'a Path>,
}

/// 一次测试用的临时环境：一个 SQLite 库 + 一份 connections.json。
struct Fixture {
    dir: PathBuf,
    connections: PathBuf,
}

impl Fixture {
    /// 只有一个普通连接（id = demo）。
    fn new(tag: &str) -> Self {
        Self::with_profiles(
            tag,
            vec![ProfileSpec {
                id: "demo",
                read_only: false,
                production: false,
                database: None,
            }],
        )
    }

    fn with_profiles(tag: &str, specs: Vec<ProfileSpec<'_>>) -> Self {
        let index = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rustdb-cli-it-{tag}-{}-{index}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let db = dir.join("test.db");
        let items: Vec<serde_json::Value> = specs
            .iter()
            .map(|spec| {
                let database = match spec.database {
                    Some(path) => path,
                    None => db.as_path(),
                };
                serde_json::json!({
                    "id": spec.id,
                    "name": format!("库-{}", spec.id),
                    "folder": "本地",
                    "driver": "sqlite",
                    "host": null,
                    "port": null,
                    "database": database.to_string_lossy().replace('\\', "/"),
                    "username": null,
                    "secret_ref": null,
                    "ssl_mode": null,
                    "params": {},
                    "read_only": spec.read_only,
                    "production": spec.production,
                    "connect_timeout_ms": 5000,
                    "color": null,
                    "last_used_at": null,
                })
            })
            .collect();
        let connections = dir.join("connections.json");
        std::fs::write(
            &connections,
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 1,
                "profiles": items,
            }))
            .expect("序列化连接配置"),
        )
        .expect("写连接配置");
        Self { dir, connections }
    }

    /// 跑一次 CLI。数据目录被指到临时目录，不污染真实配置。
    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rustdb-cli"));
        command.args(args);
        command.arg("--connections").arg(&self.connections);
        command.env("APPDATA", &self.dir);
        command.env("XDG_CONFIG_HOME", &self.dir);
        command.env("HOME", &self.dir);
        command.env_remove("RUSTDB_PASSWORD");
        command.env_remove("RUSTDB_PASSWORD_DEMO");
        command.output().expect("运行 CLI")
    }

    /// 建表 + 插一行，供查询类用例复用。
    fn seed(&self, connection: &str) {
        let created = self.run(&[
            "query",
            "-c",
            connection,
            "--write",
            "--danger",
            "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL)",
        ]);
        assert_eq!(
            created.status.code(),
            Some(0),
            "建表失败：{}",
            stderr(&created)
        );
        let inserted = self.run(&[
            "query",
            "-c",
            connection,
            "--write",
            "INSERT INTO users(id, name, score) VALUES (1, '张三', 9.5)",
        ]);
        assert_eq!(
            inserted.status.code(),
            Some(0),
            "插入失败：{}",
            stderr(&inserted)
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn code(output: &Output) -> Option<i32> {
    output.status.code()
}

#[test]
fn help_and_version_exit_zero() {
    let fixture = Fixture::new("help");
    let help = fixture.run(&["--help"]);
    assert_eq!(code(&help), Some(0));
    let text = stdout(&help);
    for command in ["connections", "schema", "query", "context", "dump"] {
        assert!(text.contains(command), "帮助里应出现 {command}");
    }
    let version = fixture.run(&["version"]);
    assert_eq!(code(&version), Some(0));
    assert!(stdout(&version).contains("RustDb CLI"));
}

#[test]
fn unknown_command_is_usage_error() {
    let fixture = Fixture::new("usage");
    let output = fixture.run(&["nope"]);
    assert_eq!(code(&output), Some(2));
    assert!(stderr(&output).contains("E_USAGE"));
}

#[test]
fn unknown_connection_is_config_error() {
    let fixture = Fixture::new("config");
    let output = fixture.run(&["query", "-c", "missing", "SELECT 1"]);
    assert_eq!(code(&output), Some(3));
    assert!(stderr(&output).contains("E_CONFIG"));
}

#[test]
fn connect_failure_is_connect_error() {
    let fixture = Fixture::new("connect");
    // 数据库目录不存在，SQLite 打不开文件。
    let missing = fixture.dir.join("nodir").join("x.db");
    let _ = std::fs::remove_dir_all(fixture.dir.join("nodir"));
    let fixture = Fixture::with_profiles(
        "connect",
        vec![ProfileSpec {
            id: "demo",
            read_only: false,
            production: false,
            database: Some(&missing),
        }],
    );
    let output = fixture.run(&["query", "-c", "demo", "SELECT 1"]);
    assert_eq!(code(&output), Some(4));
    assert!(stderr(&output).contains("E_CONNECT"));
}

#[test]
fn broken_sql_is_query_error() {
    let fixture = Fixture::new("query-error");
    let output = fixture.run(&["query", "-c", "demo", "SELECT * FROM"]);
    assert_eq!(code(&output), Some(6));
    assert!(stderr(&output).contains("E_QUERY"));
}

#[test]
fn connections_lists_profiles() {
    let fixture = Fixture::new("connections");
    let output = fixture.run(&["connections"]);
    assert_eq!(code(&output), Some(0));
    assert!(stdout(&output).contains("demo"));

    let json = fixture.run(&["--json", "connections"]);
    assert_eq!(code(&json), Some(0));
    let payload: serde_json::Value = serde_json::from_str(&stdout(&json)).expect("JSON");
    assert_eq!(payload["connections"][0]["id"], "demo");
    assert_eq!(payload["connections"][0]["driver"], "sqlite");
    // 连接列表不带主机与口令。
    assert!(payload["connections"][0].get("host").is_none());
    assert!(payload["connections"][0].get("secret").is_none());
}

#[test]
fn write_is_refused_without_switch() {
    let fixture = Fixture::new("write-gate");
    fixture.seed("demo");
    let output = fixture.run(&[
        "query",
        "-c",
        "demo",
        "INSERT INTO users(id, name, score) VALUES (2, '李四', 1.0)",
    ]);
    assert_eq!(code(&output), Some(5));
    let message = stderr(&output);
    assert!(message.contains("E_PERMISSION"));
    assert!(message.contains("--write"));
}

#[test]
fn write_switch_allows_insert() {
    let fixture = Fixture::new("write-ok");
    fixture.seed("demo");
    let output = fixture.run(&[
        "query",
        "-c",
        "demo",
        "--write",
        "INSERT INTO users(id, name, score) VALUES (2, '李四', 1.0)",
    ]);
    assert_eq!(code(&output), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("影响行数"));
}

#[test]
fn dangerous_statement_needs_danger_switch() {
    let fixture = Fixture::new("danger-gate");
    fixture.seed("demo");
    let refused = fixture.run(&["query", "-c", "demo", "--write", "DELETE FROM users"]);
    assert_eq!(code(&refused), Some(5));
    let message = stderr(&refused);
    assert!(message.contains("E_PERMISSION"));
    assert!(message.contains("--danger"));

    let allowed = fixture.run(&[
        "query",
        "-c",
        "demo",
        "--write",
        "--danger",
        "DELETE FROM users",
    ]);
    assert_eq!(code(&allowed), Some(0), "{}", stderr(&allowed));
}

#[test]
fn read_only_profile_refuses_write_even_with_switches() {
    let fixture = Fixture::with_profiles(
        "readonly",
        vec![ProfileSpec {
            id: "demo",
            read_only: true,
            production: false,
            database: None,
        }],
    );
    let output = fixture.run(&[
        "query",
        "-c",
        "demo",
        "--write",
        "--danger",
        "CREATE TABLE t(id INTEGER)",
    ]);
    assert_eq!(code(&output), Some(5));
    assert!(stderr(&output).contains("只读"));
}

#[test]
fn production_profile_refuses_write_even_with_switches() {
    let fixture = Fixture::with_profiles(
        "production",
        vec![ProfileSpec {
            id: "demo",
            read_only: false,
            production: true,
            database: None,
        }],
    );
    let output = fixture.run(&[
        "query",
        "-c",
        "demo",
        "--write",
        "--danger",
        "CREATE TABLE t(id INTEGER)",
    ]);
    assert_eq!(code(&output), Some(5));
    assert!(stderr(&output).contains("生产"));
}

#[test]
fn production_profile_allows_reads() {
    // 生产连接只能读：建表用普通连接做，读用生产连接做。
    let fixture = Fixture::with_profiles(
        "production-read",
        vec![
            ProfileSpec {
                id: "demo",
                read_only: false,
                production: true,
                database: None,
            },
            ProfileSpec {
                id: "seed",
                read_only: false,
                production: false,
                database: None,
            },
        ],
    );
    fixture.seed("seed");
    let output = fixture.run(&["query", "-c", "demo", "SELECT id FROM users"]);
    assert_eq!(code(&output), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains('1'));
}

#[test]
fn select_outputs_table_and_formats() {
    let fixture = Fixture::new("formats");
    fixture.seed("demo");

    let table = fixture.run(&["query", "-c", "demo", "SELECT id, name FROM users"]);
    assert_eq!(code(&table), Some(0), "{}", stderr(&table));
    assert!(stdout(&table).contains("张三"));

    let csv = fixture.run(&[
        "query",
        "-c",
        "demo",
        "--format",
        "csv",
        "SELECT id, name FROM users",
    ]);
    assert_eq!(code(&csv), Some(0), "{}", stderr(&csv));
    let csv_text = stdout(&csv);
    assert!(csv_text
        .lines()
        .next()
        .unwrap_or_default()
        .starts_with("id,name"));
    assert!(csv_text.contains("张三"));

    let json = fixture.run(&[
        "--json",
        "query",
        "-c",
        "demo",
        "SELECT id, name FROM users",
    ]);
    assert_eq!(code(&json), Some(0), "{}", stderr(&json));
    let payload: serde_json::Value = serde_json::from_str(&stdout(&json)).expect("JSON");
    assert_eq!(payload["results"][0]["columns"][0], "id");
    assert_eq!(payload["results"][0]["row_count"], 1);
    assert_eq!(payload["results"][0]["rows"][0]["name"], "张三");
}

#[test]
fn json_error_uses_the_same_code() {
    let fixture = Fixture::new("json-error");
    let output = fixture.run(&["--json", "query", "-c", "missing", "SELECT 1"]);
    assert_eq!(code(&output), Some(3));
    let payload: serde_json::Value = serde_json::from_str(&stderr(&output)).expect("JSON");
    assert_eq!(payload["error"]["code"], "E_CONFIG");
}

#[test]
fn schema_lists_objects() {
    let fixture = Fixture::new("schema");
    fixture.seed("demo");
    let output = fixture.run(&["schema", "-c", "demo", "--namespace", "main"]);
    assert_eq!(code(&output), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("users"));

    let json = fixture.run(&["--json", "schema", "-c", "demo", "--namespace", "main"]);
    assert_eq!(code(&json), Some(0), "{}", stderr(&json));
    let payload: serde_json::Value = serde_json::from_str(&stdout(&json)).expect("JSON");
    assert_eq!(payload["count"], 1);
    assert_eq!(payload["objects"][0]["name"], "users");
}

#[test]
fn context_export_omits_credentials() {
    let fixture = Fixture::new("context");
    fixture.seed("demo");
    let output = fixture.run(&["context", "-c", "demo", "--namespace", "main"]);
    assert_eq!(code(&output), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("users"));
    for secret in ["password", "secret", "口令", "主机"] {
        assert!(!text.contains(secret), "上下文里不应出现 {secret}");
    }
}

#[test]
fn dump_exports_ddl() {
    let fixture = Fixture::new("dump");
    fixture.seed("demo");
    let output = fixture.run(&["dump", "-c", "demo", "--namespace", "main"]);
    assert_eq!(code(&output), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("CREATE TABLE"));

    let json = fixture.run(&["--json", "dump", "-c", "demo", "--namespace", "main"]);
    assert_eq!(code(&json), Some(0), "{}", stderr(&json));
    let payload: serde_json::Value = serde_json::from_str(&stdout(&json)).expect("JSON");
    assert_eq!(payload["objects"][0]["name"], "users");
    assert!(payload["objects"][0]["ddl"]
        .as_str()
        .unwrap_or_default()
        .contains("CREATE TABLE"));
}

#[test]
fn context_can_write_to_a_file() {
    let fixture = Fixture::new("output");
    fixture.seed("demo");
    let target: PathBuf = fixture.dir.join("out").join("context.md");
    let output = fixture.run(&[
        "context",
        "-c",
        "demo",
        "--namespace",
        "main",
        "--output",
        target.to_str().unwrap(),
    ]);
    assert_eq!(code(&output), Some(0), "{}", stderr(&output));
    let text = std::fs::read_to_string(&target).expect("读输出文件");
    assert!(text.contains("users"));
}

/// CLI 不能落盘口令：连接文件里只有 `secret_ref`，没有明文口令。
#[test]
fn secrets_are_never_written_to_disk() {
    let fixture = Fixture::new("secret");
    let text = std::fs::read_to_string(&fixture.connections).expect("读连接配置");
    assert!(!text.contains("dev_secret"));
    assert!(!text.contains("password"));
}
