// 构建脚本：针对 Android target 链接 C++ 标准库。
//
// ffmpeg-next (ffmpeg-sys-next) 在 "build" feature 下会从源码编译 FFmpeg，
// 某些编解码器模块即使静态编译也会引用 C++ ABI 符号（如 __cxa_pure_virtual）。
// Cargo.toml 里的 [target.*] rustflags 不会被 Cargo 应用（rustflags 只能放
// .cargo/config.toml 或环境变量），所以必须通过 build.rs 显式链接。
//
// 用 c++_shared（动态）而不是 c++_static（静态），原因：
// - 静态链接时链接器按符号解析顺序处理 .a，libc++ 若先于 ffmpeg 静态库
//   处理，里面的对象文件不会被拉入，导致 __cxa_pure_virtual 仍然未定义。
// - c++_shared 没有顺序问题，且 NDK 自带 libc++_shared.so，打包进 APK 即可。

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "android" {
        // 链接 NDK 自带的动态 C++ 标准库（libc++_shared.so）。
        println!("cargo:rustc-link-lib=dylib=c++_shared");
        // Android 动态加载（dlopen/dlsym）必需的库。
        println!("cargo:rustc-link-lib=dylib=dl");
        // 数学函数库（FFmpeg 依赖）。
        println!("cargo:rustc-link-lib=dylib=m");
    }
}
