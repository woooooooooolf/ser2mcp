# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式，版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [0.2.2] - 2026-08-04

### Changed

- 默认 `read_timeout_ms` 从 100ms 调整为 10ms：Windows 上 CH340 / CP210x 等 USB 转串口驱动对阻塞读按超时边界成批交付数据，调小该值可显著降低 `uart_read` / `uart_exchange` 延迟（CH340 / CP210x 双芯片实测：1000ms 时延迟呈 ~1s 整数倍，10ms 时与直连串口相当）

### Added

- 延迟探针示例 `examples/latency_probe.rs`：通过真实 MCP 协议测量各工具延迟，支持 `bench`（读写往返压测）与 `benchw`（纯写入路径），便于复测与参数对比

### Docs

- README 补充 Windows USB 转串口延迟说明与调优提醒（`read_timeout_ms` / `idle_ms`），提醒 AI 工具在实际使用中按需调整

## [0.2.1] - 2026-08-04

### Fixed

- 读线程改用独立串口句柄（`try_clone`），修复部分 USB 转串口驱动（如 CH340）偶发读阻塞导致 `write` / 工具调用长时间无响应的问题（真实硬件稳定性测试发现）

## [0.2.0] - 2026-08-04

### Added

- 多端口支持：可同时打开多个串口，端口名即句柄；除 `uart_list_ports` 外每个工具都需要传 `port` 参数
- CLI：`ser2mcp --list-ports` / `--version` / `--help`
- Linux 串口权限辅助脚本 `scripts/linux-serial-permissions.sh`
- README 新增命令行用法、多端口/透传说明与常见问题（Troubleshooting）

### Changed

- 破坏性 API 变更：`uart_configure` / `uart_write` / `uart_read` / `uart_exchange` / `uart_available` / `uart_clear` / `uart_close` 新增必填 `port` 参数
- 定位明确为原样透传：不解析、不匹配、不过滤串口字节流内容

## [0.1.0] - 2026-08-04

### Added

- 9 个 MCP 工具：`uart_list_ports` / `uart_open` / `uart_configure` / `uart_write` / `uart_read` / `uart_exchange` / `uart_available` / `uart_clear` / `uart_close`
- 后台读线程 + 有界环形缓冲：上行数据不丢不堵，溢出计数可检测数据缺口
- 完整串口参数控制：波特率 / 数据位 / 校验位 / 停止位 / 流控 / 读写超时
- hex / text 双模式传输，二进制安全
- 可配置内部参数：`buffer_size` / `idle_ms` / `max_bytes` / `timeout_ms`
- 回环自测示例 `examples/loopback.rs`
- 双语 README（简体中文 / English）
- GitHub Actions CI：fmt / clippy / test / doc / 跨平台 release 构建
- 自动化 Release：Windows / Linux / macOS 预编译二进制 + sha256 校验和 + Rust 文档

[0.1.0]: https://github.com/woooooooooolf/ser2mcp/releases/tag/v0.1.0
[0.2.0]: https://github.com/woooooooooolf/ser2mcp/releases/tag/v0.2.0
[0.2.1]: https://github.com/woooooooooolf/ser2mcp/releases/tag/v0.2.1
[0.2.2]: https://github.com/woooooooooolf/ser2mcp/releases/tag/v0.2.2
