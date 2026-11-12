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
/// SQL 编辑器高度（UI-设计规范 §8）：默认 176，可拖拽范围 96-480。
pub const EDITOR_HEIGHT_MIN: f32 = 96.;
pub const EDITOR_HEIGHT_MAX: f32 = 480.;
pub const EDITOR_HEIGHT_DEFAULT: f32 = 176.;
/// 编辑器与结果区之间分隔条的视觉/命中厚度（9px：1px 线 + 上下各 4px）。
pub const EDITOR_SPLITTER_H: f32 = 9.;
/// 结果区至少保留的高度，窗口变小时优先压缩编辑器。
pub const RESULT_HEIGHT_MIN: f32 = 120.;
/// 键盘微调一档（Ctrl+Alt+↑/↓）。
pub const EDITOR_RESIZE_STEP: f32 = 16.;

/// 结果网格行高三档（UI-设计规范 §4），落 `layout.json`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridDensity {
    Compact,
    #[default]
    Standard,
    Spacious,
}

impl GridDensity {
    pub const ALL: [Self; 3] = [Self::Compact, Self::Standard, Self::Spacious];

    pub const fn row_height(self) -> f32 {
        match self {
            Self::Compact => 22.,
            Self::Standard => 25.,
            Self::Spacious => 30.,
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Standard => "standard",
            Self::Spacious => "spacious",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Compact => "紧凑",
            Self::Standard => "标准",
            Self::Spacious => "宽松",
        }
    }

    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|density| density.key() == key)
            .unwrap_or(Self::Standard)
    }
}

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
    /// 结果网格行高密度，旧配置文件缺字段时回落到标准。
    #[serde(default)]
    pub grid_density: GridDensity,
    /// SQL 编辑器高度（不含分隔条）；旧配置缺字段时回落到 176。
    #[serde(default = "default_editor_height")]
    pub editor_height: f32,
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

fn default_editor_height() -> f32 {
    EDITOR_HEIGHT_DEFAULT
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
            grid_density: GridDensity::default(),
            editor_height: EDITOR_HEIGHT_DEFAULT,
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

/// 夹取编辑器高度：`available_h` 是工作区 body 的可用总高（编辑器 + 分隔条 + 结果区）。
/// 窗口不足时优先压缩编辑器，保证结果区至少 [`RESULT_HEIGHT_MIN`]；非有限值回落到默认。
pub fn clamp_editor_height(height: f32, available_h: f32) -> f32 {
    if !height.is_finite() {
        return EDITOR_HEIGHT_DEFAULT;
    }
    let mut max = EDITOR_HEIGHT_MAX;
    if available_h.is_finite() && available_h > 0. {
        max = max.min(available_h - EDITOR_SPLITTER_H - RESULT_HEIGHT_MIN);
    }
    height.clamp(EDITOR_HEIGHT_MIN, max.max(EDITOR_HEIGHT_MIN))
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

    /// 按当前工作区可用高度夹取后的编辑器高度（唯一读取口径）。
    pub fn clamped_editor_height(&self, available_h: f32) -> f32 {
        clamp_editor_height(self.editor_height, available_h)
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
    fn grid_density_defaults_and_roundtrip() {
        let layout = LayoutSettings::default();
        assert_eq!(layout.grid_density, GridDensity::Standard);
        assert_eq!(layout.grid_density.row_height(), 25.);
        assert_eq!(GridDensity::Compact.row_height(), 22.);
        assert_eq!(GridDensity::Spacious.row_height(), 30.);
        let path = temp_path("density");
        let mut next = LayoutSettings::default();
        next.grid_density = GridDensity::Spacious;
        save_layout(&path, &next).unwrap();
        assert_eq!(load_layout(&path).grid_density, GridDensity::Spacious);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_file_without_density_uses_default() {
        let path = temp_path("legacy-density");
        std::fs::write(&path, r#"{"ai_width":400.0}"#).unwrap();
        assert_eq!(load_layout(&path).grid_density, GridDensity::Standard);
        assert_eq!(load_layout(&path).ai_width, 400.);
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

    #[test]
    fn editor_height_defaults_to_current_visual() {
        let layout = LayoutSettings::default();
        assert_eq!(layout.editor_height, EDITOR_HEIGHT_DEFAULT);
        assert_eq!(layout.clamped_editor_height(800.), 176.);
        assert_eq!(clamp_editor_height(f32::NAN, 800.), EDITOR_HEIGHT_DEFAULT);
    }

    #[test]
    fn editor_height_is_clamped_and_leaves_result_room() {
        assert_eq!(clamp_editor_height(10., 800.), EDITOR_HEIGHT_MIN);
        assert_eq!(clamp_editor_height(9999., 800.), EDITOR_HEIGHT_MAX);
        // 可用 300 → 上限 = 300 - 9 - 120 = 171。
        assert_eq!(clamp_editor_height(400., 300.), 171.);
        // 视口再小也不得压过下限。
        assert_eq!(clamp_editor_height(400., 120.), EDITOR_HEIGHT_MIN);
    }

    #[test]
    fn editor_height_roundtrip_and_legacy_default() {
        let path = temp_path("editor-height");
        let mut next = LayoutSettings::default();
        next.editor_height = 240.;
        save_layout(&path, &next).unwrap();
        assert_eq!(load_layout(&path).editor_height, 240.);
        let _ = std::fs::remove_file(&path);

        let legacy = temp_path("legacy-editor-height");
        std::fs::write(&legacy, r#"{"ai_width":400.0}"#).unwrap();
        assert_eq!(load_layout(&legacy).editor_height, EDITOR_HEIGHT_DEFAULT);
        assert_eq!(load_layout(&legacy).ai_width, 400.);
        let _ = std::fs::remove_file(&legacy);
    }

    #[test]
    fn editor_height_keyboard_step_stays_in_range() {
        let step = EDITOR_RESIZE_STEP;
        assert_eq!(
            clamp_editor_height(EDITOR_HEIGHT_MIN - step, 800.),
            EDITOR_HEIGHT_MIN
        );
        assert_eq!(
            clamp_editor_height(EDITOR_HEIGHT_MAX + step, 800.),
            EDITOR_HEIGHT_MAX
        );
        assert_eq!(clamp_editor_height(176. + step, 800.), 192.);
    }
}
