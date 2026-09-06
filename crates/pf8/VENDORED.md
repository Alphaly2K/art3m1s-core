# Vendored pf8 (from pfs-rs)

Source: https://github.com/sakarie9/pfs-rs (`pf8/`), MIT license（见 LICENSE）。

本目录是 pf8 的 vendored fork，供 art3m1s-core 使用。相对上游的改动：

- `Pf8Reader` 的底层读取从 `File` 泛化为 `Box<dyn ReadSeek>`（`open_reader`），
  以支撑分卷归档（`root.pfs` + `root.pfs.000`…）的串联读取。
- 新增 `open_with_encoding` / `open_reader_with_encoding`：条目路径按显式编码
  解码（缺省保持上游的 UTF-8 → Shift_JIS 自动回退）。
- 接受 `pf2` magic（按 PF6 同等布局处理，不加密）。
- 条目查找键统一小写（大小写不敏感，与引擎历史行为一致；同键取先出现者）。
- 新增 `read_range`：按条目内偏移做带解密的范围读取（视频/音频流式读取用）。

升级上游版本时请保留以上改动并跑 `cargo test`。
