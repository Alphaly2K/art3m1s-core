# art3m1s-siglus

Siglus VM 与 `art3m1s-render` 的宿主兼容接口。与 RFVP 一样，这个 crate
引用相邻目录中的引擎 fork；原项目只暴露无自带渲染器的 `SiglusHost`
构造、`RenderFrame` 和图像管理器，贴图上传及绘制转换都留在这里。

当前 `SiglusAdapter::step` 可直接提交普通 2D sprite 帧，不经 wgpu 离屏回读。
若遇到 stage wipe、E-Mote、3D、蒙版或尚未等价实现的颜色/混合效果，
会返回错误，避免悄悄显示错误画面。引擎退出由返回值传递给调用方。

这个 crate 目前仅是 Rust 侧接口，还没有接入 core 的版本化 FFI 与 Flutter
PlayerScene，也不能视为完整的 Siglus 游戏支持。顶层可通过 `siglus-engine`
feature 引入，但默认不启用。基本编译检查：

```sh
cargo check --manifest-path crates/art3m1s-siglus/Cargo.toml --lib
```

macOS 上可用独立 winit 窗口进行交互测试：

```sh
cargo run --manifest-path crates/art3m1s-siglus/Cargo.toml \
  --features window --bin siglus_window -- /path/to/Siglus-game
```

也可以直接保存 Metal 后端帧：

```sh
cargo run --manifest-path crates/art3m1s-siglus/Cargo.toml \
  --features frame-dump --bin siglus_frame_dump -- \
  /path/to/Siglus-game /tmp/siglus-frame.png 850
```

引擎 fork 目前对 UCI/H.264 G00 图像（包括 High 10）调用系统的 `ffmpeg`
可执行文件；这类游戏需要把 `ffmpeg` 放在 `PATH` 中，待后续打包解码器。
不使用 UCI 图像的游戏不受影响。
