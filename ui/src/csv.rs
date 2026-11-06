//! 结果集 CSV 导出：落盘与文件命名。
//!
//! 转义与公式防护**统一走 `rustdb_core::export`**（需求 19.3：格式编码集中，UI 不拼 CSV）。
//! 口径：UTF-8 + BOM（Excel 直接双击不乱码）、CRLF、NULL 导出为空字段。

use std::path::{Path, PathBuf};

/// RFC 4180 转义：含逗号/引号/换行时整体加引号，内部引号翻倍。
pub fn escape_field(field: &str) -> String {
    rustdb_core::export::escape_field(field, ',')
}

pub fn to_csv(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut out = String::from('\u{feff}');
    out.push_str(
        &headers
            .iter()
            .map(|h| escape_field(h))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push_str("\r\n");
    for row in rows {
        out.push_str(
            &row.iter()
                // F-72：公式防护（`=cmd`、`@x` 等加前导单引号；`-5` 这类真实负数不改写）。
                .map(|v| escape_field(&rustdb_core::export::guard_formula(v)))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str("\r\n");
    }
    out
}

pub fn file_name(epoch_secs: u64) -> String {
    format!("rustdb-{epoch_secs}.csv")
}

/// 优先导出到系统的下载目录，取不到就落到应用数据目录下的 `exports/`。
pub fn export_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RUSTDB_EXPORT_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            let downloads = Path::new(&profile).join("Downloads");
            if downloads.is_dir() {
                return downloads;
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let downloads = Path::new(&home).join("Downloads");
            if downloads.is_dir() {
                return downloads;
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let downloads = Path::new(&home).join("Downloads");
            if downloads.is_dir() {
                return downloads;
            }
        }
    }
    rustdb_platform::paths::data_dir().join("exports")
}

pub fn write_csv(headers: &[String], rows: &[Vec<String>]) -> std::io::Result<PathBuf> {
    write_bytes(to_csv(headers, rows).as_bytes(), "csv")
}

/// 写出任意已编码的导出内容：沿用同一导出目录与 `rustdb-<秒>.<ext>` 命名。
pub fn write_bytes(bytes: &[u8], extension: &str) -> std::io::Result<PathBuf> {
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("rustdb-{secs}.{extension}"));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_only_when_needed() {
        assert_eq!(escape_field("plain"), "plain");
        assert_eq!(escape_field("a,b"), "\"a,b\"");
        assert_eq!(escape_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape_field("line1\nline2"), "\"line1\nline2\"");
    }

    #[test]
    fn csv_starts_with_bom_and_uses_crlf() {
        let csv = to_csv(
            &["渠道".to_string(), "GMV".to_string()],
            &[vec!["自然搜索".to_string(), "3591.25".to_string()]],
        );
        assert!(csv.starts_with('\u{feff}'));
        assert_eq!(csv, "\u{feff}渠道,GMV\r\n自然搜索,3591.25\r\n".to_string());
    }

    #[test]
    fn file_name_is_stable_for_a_timestamp() {
        assert_eq!(file_name(1_700_000_000), "rustdb-1700000000.csv");
    }

    #[test]
    fn csv_guards_formulas_but_not_negative_numbers() {
        let csv = to_csv(
            &["v".to_string()],
            &[
                vec!["=1+1".to_string()],
                vec!["@user".to_string()],
                vec!["-42".to_string()],
                vec!["-1.5e3".to_string()],
            ],
        );
        assert!(csv.contains("'=1+1"), "{csv}");
        assert!(csv.contains("'@user"), "{csv}");
        assert!(
            csv.contains("\r\n-42\r\n"),
            "真实负数不能被改写成文本：{csv}"
        );
        assert!(csv.contains("\r\n-1.5e3\r\n"), "{csv}");
    }

    #[test]
    fn writes_file_to_configured_dir() {
        let dir = std::env::temp_dir().join(format!("rustdb-csv-{}", std::process::id()));
        std::env::set_var("RUSTDB_EXPORT_DIR", &dir);
        let path = write_csv(
            &["渠道".to_string()],
            &[vec!["自然搜索".to_string()], vec![String::new()]],
        )
        .expect("写 CSV 失败");
        let content = std::fs::read_to_string(&path).expect("读回 CSV 失败");
        assert!(content.starts_with('\u{feff}'));
        assert!(content.contains("渠道\r\n"));
        assert!(content.ends_with("自然搜索\r\n\r\n"), "NULL 应导出为空行");

        std::env::remove_var("RUSTDB_EXPORT_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
