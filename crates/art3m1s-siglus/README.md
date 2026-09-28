# art3m1s-siglus

Siglus VM 与 `art3m1s-render` 的宿主兼容接口。与 RFVP 一样，这个 crate
引用相邻目录中的引擎 fork；原项目只暴露无自带渲染器的 `SiglusHost`
构造、`RenderFrame` 和图像管理器，贴图上传及绘制转换都留在这里。

当前 `SiglusAdapter::step` 可直接提交普通 2D sprite 帧，不经 wgpu 离屏回读。
若遇到 stage wipe、E-Mote、3D、蒙版或尚未等价实现的颜色/混合效果，
会返回错误，避免悄悄显示错误画面。引擎退出由返回值传递给调用方。

顶层通过 `siglus-engine` feature 接入独立 FFI 与 Flutter PlayerScene，
默认不启用。该 feature 同时启用应用包内的 FFmpeg 解码，确保 GUI 环境不依赖
外部 `ffmpeg` 可执行文件，并启用无设备 Kira 后端，由 macOS Flutter Host
拉取 48 kHz 双声道 f32 PCM 输出。独立窗口样例不启用 `host-audio`，仍由
上游 Kira 直接输出声音。基本编译检查：

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

独立样例默认保留上游的 `ffmpeg` 命令解码路径。要模拟正式 Host 中的进程内
UCI/H.264 解码（包括 High 10），给样例加上 `uci-ffmpeg` feature，并设置
`FFMPEG_DIR` 指向已构建的 FFmpeg 前缀。正式 `siglus-engine` 会自动启用它。
