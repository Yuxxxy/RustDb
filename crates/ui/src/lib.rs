// gpui 宏（如 #[gpui_kit::test]）展开仍引用 gpui:: 路径，按 kit 约定别名。
extern crate gpui_kit as gpui;

pub mod completion;
pub mod csv;
pub mod ddl;
pub mod design;
pub mod detail;
pub mod diagnostics;
pub mod i18n;
pub mod main_view;
pub mod theme;

pub use main_view::MainView;
