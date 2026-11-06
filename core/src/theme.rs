//! 视觉 Token。唯一来源：UI 只允许经 Token 取色，禁止十六进制字面量。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeTokens {
    pub bg_base: u32,
    pub bg_elevated: u32,
    pub bg_overlay: u32,
    pub border_subtle: u32,
    pub fg_primary: u32,
    pub fg_secondary: u32,
    pub fg_muted: u32,
    pub accent: u32,
    pub success: u32,
    pub purple: u32,
    pub warning: u32,
    pub danger: u32,
    pub on_accent: u32,
    pub on_danger: u32,

    // 数据库专属
    pub sql_keyword: u32,
    pub sql_string: u32,
    pub sql_number: u32,
    pub sql_comment: u32,
    pub sql_function: u32,
    pub grid_header_bg: u32,
    pub grid_row_alt: u32,
    pub cell_changed: u32,
    pub diff_add: u32,
    pub diff_del: u32,
}

impl ThemeTokens {
    pub const fn dark() -> Self {
        Self {
            bg_base: 0x0D1117,
            bg_elevated: 0x161B22,
            bg_overlay: 0x1C2128,
            border_subtle: 0x30363D,
            fg_primary: 0xE6EDF3,
            fg_secondary: 0xB6C2CF,
            fg_muted: 0x8B949E,
            accent: 0x58A6FF,
            success: 0x3FB950,
            purple: 0xD2A8FF,
            warning: 0xD29922,
            danger: 0xF85149,
            on_accent: 0x0D1117,
            on_danger: 0xFFFFFF,
            sql_keyword: 0xFF7B72,
            sql_string: 0xA5D6FF,
            sql_number: 0x79C0FF,
            sql_comment: 0x8B949E,
            sql_function: 0xD2A8FF,
            grid_header_bg: 0x161B22,
            grid_row_alt: 0x10151C,
            cell_changed: 0xD29922,
            diff_add: 0x1A7F37,
            diff_del: 0xA32D2D,
        }
    }

    pub const fn light() -> Self {
        Self {
            bg_base: 0xFFFFFF,
            bg_elevated: 0xF6F8FA,
            bg_overlay: 0xFFFFFF,
            border_subtle: 0xD0D7DE,
            fg_primary: 0x1F2328,
            fg_secondary: 0x57606A,
            fg_muted: 0x656D76,
            accent: 0x0969DA,
            success: 0x1A7F37,
            purple: 0x8250DF,
            warning: 0x9A6700,
            danger: 0xCF222E,
            on_accent: 0xFFFFFF,
            on_danger: 0xFFFFFF,
            sql_keyword: 0xCF222E,
            sql_string: 0x0A3069,
            sql_number: 0x0550AE,
            sql_comment: 0x6E7781,
            sql_function: 0x8250DF,
            grid_header_bg: 0xF6F8FA,
            grid_row_alt: 0xFAFBFC,
            cell_changed: 0x9A6700,
            diff_add: 0x1A7F37,
            diff_del: 0xCF222E,
        }
    }

    pub fn resolved(mode: ThemeMode, system_dark: bool) -> Self {
        match mode {
            ThemeMode::Dark => Self::dark(),
            ThemeMode::Light => Self::light(),
            ThemeMode::System => {
                if system_dark {
                    Self::dark()
                } else {
                    Self::light()
                }
            }
        }
    }
}
