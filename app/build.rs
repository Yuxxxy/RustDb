//! 构建脚本：Windows 主线程栈预留从默认 1MB 提到 16MB。

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // gpui 的整条绘制链（WndProc → 首帧构建 → 面板闭包）都在主线程上，debug 构建
    // 单帧栈占用大，1MB 会在首帧触发 "thread 'main' has overflowed its stack"。
    // 16MB 只是虚拟地址预留、按需提交，不增加实际内存占用。
    println!("cargo:rustc-link-arg-bins=/STACK:16777216");
}
