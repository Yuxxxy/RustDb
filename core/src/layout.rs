//! 三列布局的尺寸与折叠状态：纯配置，不依赖 UI。
//!
//! 与 RustShell 同口径：侧栏 180–520（默认 260，按面板分别记忆），
//! AI 列 280–480（默认 360），窗口窄于 [`NARROW_WINDOW`] 时侧栏自动让位。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const LAYOUT_VERSION: u32 = 1;
pub const SIDEBAR_WIDTH_MIN: f32 = 180.;
pub const SIDEBAR_WIDTH_MAX: f32 = 520.;
pub const SIDEBAR_WIDTH_DEFAULT: f32 = 260.;
pub const AI_WIDTH_MIN: f32 = 280.;
pub const AI_WIDTH_MAX: f32 = 480.;
pub const AI_WIDTH_DEFAULT: f32 = 360.;
/// 低于该窗口宽度时侧栏自动收起（不改变用户自己的折叠开关）。
pub const NARROW_WINDOW: f32 = 1024.;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutSettings {
    #[serde(default = "default_version")]
    pub version: u32,
    /// 侧栏宽度按面板记忆，键为 `PanelKind::key()`。
    #[serde(default)]
    pub sidebar_widths: BTreeMap<String, f32>,
    #[serde(default = "default_ai_width")]
    pub ai_width: f32,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    #[serde(default = "default_ai_open")]
    pub ai_open: bool,
    #[serde(default)]
    pub theme: crate::theme::ThemeMode,
}

fn default_version() -> u32 {
    LAYOUT_VERSION
}

fn default_ai_width() -> f32 {
    AI_WIDTH_DEFAULT
}

fn default_ai_open() -> bool {
    false
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            version: LAYOUT_VERSION,
            sidebar_widths: BTreeMap::new(),
            ai_width: AI_WIDTH_DEFAULT,
            sidebar_collapsed: false,
            ai_open: false,
            theme: crate::theme::ThemeMode::Dark,
        }
    }
}

pub fn clamp_sidebar_width(width: f32) -> f32 {
    if width.is_finite() {
        width.clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX)
    } else {
        SIDEBAR_WIDTH_DEFAULT
    }
}

pub fn clamp_ai_width(width: f32) -> f32 {
    if width.is_finite() {
        width.clamp(AI_WIDTH_MIN, AI_WIDTH_MAX)
    } else {
        AI_WIDTH_DEFAULT
    }
}

impl LayoutSettings {
    pub fn sidebar_width(&self, panel: &str) -> f32 {
        self.sidebar_widths
            .get(panel)
            .copied()
            .map(clamp_sidebar_width)
            .unwrap_or(SIDEBAR_WIDTH_DEFAULT)
    }

    pub fn remember_sidebar_width(&mut self, panel: &str, width: f32) {
        self.sidebar_widths
            .insert(panel.to_string(), clamp_sidebar_width(width));
    }

    pub fn clamped_ai_width(&self) -> f32 {
        clamp_ai_width(self.ai_width)
    }
}

pub fn layout_path(dir: &Path) -> PathBuf {
    dir.join("layout.json")
}

pub fn load_layout(path: &Path) -> LayoutSettings {
    let Ok(text) = std::fs::read_to_string(path) else {
        return LayoutSettings::default();
    };
    match serde_json::from_str::<LayoutSettings>(&text) {
        Ok(settings) => settings,
        Err(error) => {
            log::warn!("布局配置解析失败：{error}（{}）", path.display());
            LayoutSettings::default()
        }
    }
}

pub fn save_layout(path: &Path, settings: &LayoutSettings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(settings)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rustdb-layout-{name}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn defaults_match_the_spec() {
        let layout = LayoutSettings::default();
        assert_eq!(layout.sidebar_width("connections"), 260.);
        assert_eq!(layout.clamped_ai_width(), 360.);
        assert!(!layout.sidebar_collapsed);
        assert!(!layout.ai_open);
        assert_eq!(layout.version, LAYOUT_VERSION);
    }

    #[test]
    fn widths_are_clamped_and_remembered_per_panel() {
        let mut layout = LayoutSettings::default();
        layout.remember_sidebar_width("connections", 900.);
        layout.remember_sidebar_width("objects", 10.);
        assert_eq!(layout.sidebar_width("connections"), SIDEBAR_WIDTH_MAX);
        assert_eq!(layout.sidebar_width("objects"), SIDEBAR_WIDTH_MIN);
        assert_eq!(layout.sidebar_width("unknown"), SIDEBAR_WIDTH_DEFAULT);
        layout.ai_width = 999.;
        assert_eq!(layout.clamped_ai_width(), AI_WIDTH_MAX);
        layout.ai_width = f32::NAN;
        assert_eq!(layout.clamped_ai_width(), AI_WIDTH_DEFAULT);
        layout.ai_width = 300.;
        assert_eq!(layout.clamped_ai_width(), 300.);
    }

    #[test]
    fn roundtrip_preserves_widths_and_flags() {
        let path = temp_path("roundtrip");
        let mut layout = LayoutSettings::default();
        layout.remember_sidebar_width("objects", 321.);
        layout.ai_width = 401.;
        layout.sidebar_collapsed = true;
        layout.ai_open = true;
        save_layout(&path, &layout).unwrap();
        assert_eq!(load_layout(&path), layout);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_or_corrupt_file_falls_back_to_default() {
        let missing = temp_path("missing");
        assert_eq!(load_layout(&missing), LayoutSettings::default());
        let corrupt = temp_path("corrupt");
        std::fs::write(&corrupt, "{not json").unwrap();
        assert_eq!(load_layout(&corrupt), LayoutSettings::default());
        let _ = std::fs::remove_file(&corrupt);
    }

    #[test]
    fn partial_file_keeps_defaults_for_missing_fields() {
        let path = temp_path("partial");
        std::fs::write(&path, r#"{"sidebar_widths":{"objects":300.0}}"#).unwrap();
        let layout = load_layout(&path);
        assert_eq!(layout.sidebar_width("objects"), 300.);
        assert_eq!(layout.sidebar_width("connections"), SIDEBAR_WIDTH_DEFAULT);
        assert_eq!(layout.ai_width, AI_WIDTH_DEFAULT);
        assert!(!layout.ai_open);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn atomic_save_replaces_previous_content() {
        let path = temp_path("replace");
        save_layout(&path, &LayoutSettings::default()).unwrap();
        let mut next = LayoutSettings::default();
        next.ai_width = 420.;
        save_layout(&path, &next).unwrap();
        assert_eq!(load_layout(&path).ai_width, 420.);
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_file(&path);
    }
}
