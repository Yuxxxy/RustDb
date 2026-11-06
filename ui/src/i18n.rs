//! i18n 骨架（M5）：把「文案 → 语言」这件事收口到一个模块，
//! 后续把各处的硬编码中文逐条搬进 `dict()` 即可，不需要改调用点。
//!
//! 设计取舍：
//! * 不做编译期宏，避免 6000 行 UI 一次性重写；
//! * 键用点分命名（`status.connected`），缺失时回落中文原文并 `log::warn`，
//!   保证漏翻不会出现空白文案（R5：不静默）。

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Self::Zh => "zh-CN",
            Self::En => "en-US",
        }
    }

    fn index(self) -> u8 {
        match self {
            Self::Zh => 0,
            Self::En => 1,
        }
    }

    fn from_index(value: u8) -> Self {
        if value == 1 {
            Self::En
        } else {
            Self::Zh
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn current() -> Lang {
    Lang::from_index(CURRENT.load(Ordering::Relaxed))
}

pub fn set_current(lang: Lang) {
    CURRENT.store(lang.index(), Ordering::Relaxed);
}

/// 词条表：`(key, 中文, English)`。
const ENTRIES: &[(&str, &str, &str)] = &[
    ("app.name", "RustDb", "RustDb"),
    ("status.idle", "未连接", "Disconnected"),
    ("status.connected", "已连接", "Connected"),
    ("status.busy", "正在执行…", "Running…"),
    ("grid.loaded", "已加载", "Loaded"),
    ("grid.filtered", "已筛选", "filtered"),
    ("grid.detail", "详情", "Details"),
    ("grid.submit", "写入", "Commit"),
    ("grid.revert", "撤销", "Revert"),
    ("designer.preview", "预览 DDL", "Preview DDL"),
    ("designer.apply", "执行", "Apply"),
    ("ai.confirm", "需要确认", "Confirmation required"),
    ("ai.approve", "允许执行", "Allow"),
    ("ai.reject", "拒绝", "Reject"),
];

/// 取词条；缺失返回 `key` 本身并告警（方便一眼发现漏翻）。
pub fn t(key: &str) -> String {
    t_in(current(), key)
}

pub fn t_in(lang: Lang, key: &str) -> String {
    for (entry_key, zh, en) in ENTRIES {
        if *entry_key == key {
            return match lang {
                Lang::Zh => (*zh).to_string(),
                Lang::En => (*en).to_string(),
            };
        }
    }
    log::warn!("缺少词条：{key}");
    key.to_string()
}

/// 词条总数（自检用）。
pub fn entry_count() -> usize {
    ENTRIES.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_keys_translate() {
        assert_eq!(t_in(Lang::Zh, "grid.detail"), "详情");
        assert_eq!(t_in(Lang::En, "grid.detail"), "Details");
        assert_eq!(t_in(Lang::En, "app.name"), "RustDb");
    }

    #[test]
    fn unknown_key_falls_back_to_key() {
        assert_eq!(t_in(Lang::Zh, "nope.missing"), "nope.missing");
    }

    #[test]
    fn keys_are_unique_and_switchable() {
        let mut keys: Vec<&str> = ENTRIES.iter().map(|entry| entry.0).collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "词条 key 不能重复");
        assert!(entry_count() >= 10);

        set_current(Lang::En);
        assert_eq!(current(), Lang::En);
        assert_eq!(t("status.connected"), "Connected");
        set_current(Lang::Zh);
        assert_eq!(t("status.connected"), "已连接");
    }
}
