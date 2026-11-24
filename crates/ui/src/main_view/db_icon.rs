//! 数据库类型品牌图标。
//!
//! 图标按方言内嵌为 SVG，而不是塞进 `IconName`：gpui-kit 的图标集是
//! Lucide（通用线性图标），没有品牌标识；数据库类型必须一眼可辨。
//! 颜色取品牌色的中间调，深浅两套主题都能看清；渲染走 `Svg::data`，
//! 不需要额外的 AssetSource。

use crate::theme::to_hsla;
use gpui_kit::{px, AnyElement, IntoElement, Styled};
use rustdb_db_api::DbKind;

/// 连接树 / 库切换器里的统一图标边长：12px 文字配 15px 品牌图标，
/// 再小海豚 / 羽毛的轮廓就糊成一团。
pub(crate) const DB_ICON_SIZE: f32 = 15.;

/// 某一方言的品牌图标；`size` 为边长（px）。
pub(crate) fn db_icon(kind: DbKind, size: f32) -> AnyElement {
    // `Svg` 渲染的是 alpha mask，再按 `text.color` 上色：SVG 里的 fill 只决定
    // 形状（用来区分海豚 / 大象 / 羽毛 …），真正的品牌色在这里给。
    gpui_kit::svg()
        .data(db_icon_bytes(kind))
        .w(px(size))
        .h(px(size))
        .flex_none()
        .text_color(to_hsla(db_icon_color(kind)))
        .into_any_element()
}

/// 品牌色的中间调：深色（`#16181D`）与浅色（`#FFFFFF`）背景下都能看清。
pub(crate) fn db_icon_color(kind: DbKind) -> u32 {
    match kind {
        DbKind::Mysql => 0x55A0C4,
        DbKind::Mariadb => 0x74A6B5,
        DbKind::Postgres => 0x6B8CF0,
        DbKind::Sqlite => 0x74BCE0,
        DbKind::Mssql => 0xE05C58,
        DbKind::Oracle => 0xF05A50,
    }
}

/// 图标原始字节（测试与调试也用它）。
pub(crate) fn db_icon_bytes(kind: DbKind) -> &'static [u8] {
    match kind {
        DbKind::Mysql => include_bytes!("../../assets/db/mysql.svg"),
        DbKind::Mariadb => include_bytes!("../../assets/db/mariadb.svg"),
        DbKind::Postgres => include_bytes!("../../assets/db/postgresql.svg"),
        DbKind::Sqlite => include_bytes!("../../assets/db/sqlite.svg"),
        DbKind::Mssql => include_bytes!("../../assets/db/mssql.svg"),
        DbKind::Oracle => include_bytes!("../../assets/db/oracle.svg"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 六种方言都必须有自己的图标，不能回退到通用数据库图标。
    #[test]
    fn every_driver_has_a_distinct_brand_icon() {
        let mut seen: Vec<&[u8]> = Vec::new();
        for kind in DbKind::ALL {
            let bytes = db_icon_bytes(kind);
            assert!(
                bytes.starts_with(b"<svg"),
                "{} 的图标必须是 SVG",
                kind.key()
            );
            assert!(
                !seen.contains(&bytes),
                "{} 与其他方言共用了同一张图标",
                kind.key()
            );
            seen.push(bytes);
        }
    }

    /// 每个类型都必须有独立的品牌色：`Svg` 只取 alpha mask，
    /// 颜色丢失会让六种数据库的图标全都变成同一个灰点。
    #[test]
    fn brand_colors_are_distinct() {
        let mut seen = Vec::new();
        for kind in DbKind::ALL {
            let color = db_icon_color(kind);
            assert!(!seen.contains(&color), "{} 的颜色与其它方言重复", kind.key());
            seen.push(color);
        }
    }

    /// 品牌 SVG 必须有可着色的图形（`Svg` 用 alpha mask 着色）。
    #[test]
    fn brand_icons_carry_drawable_geometry() {
        for kind in DbKind::ALL {
            let text = std::str::from_utf8(db_icon_bytes(kind)).expect("UTF-8");
            assert!(
                text.contains("<path") || text.contains("<ellipse"),
                "{} 的图标没有图形",
                kind.key()
            );
            assert!(
                text.contains("viewBox="),
                "{} 的图标缺少 viewBox，缩放会失真",
                kind.key()
            );
        }
    }
}
