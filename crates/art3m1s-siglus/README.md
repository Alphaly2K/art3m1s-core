# art3m1s-siglus

Siglus VM 的 Host 接口。`SiglusAdapter::step` 接收经过的毫秒数并返回退出状态与可选的
平台无关帧。帧借用会话的图像资源，使用完毕后即可推进下一步。

使用共享 GPU 后端的 Host 可调用 `SiglusRenderer::draw` 绘制普通 2D sprite 帧。
目前该绘制适配器会报告无法处理的 stage wipe、E-Mote、3D、蒙版和部分颜色或混合效果。

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
