# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式，版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Fixed

- 修复 `uart_exchange` / `uart_read` 在大块数据流下的 idle 误判提前返回（[#2](https://github.com/woooooooooolf/ser2mcp/issues/2)）：空闲判定除环形缓冲 `idle_ms` 无新写入外，还需串口驱动侧无可读字节（`bytes_to_read() == 0`），避免读线程在"驱动缓冲排空后、剩余数据仍在线路/USB 传输中"的窗口期（Windows 实测可达数百 ms）被误判为响应结束、残留数据污染下一次调用（实测复现率 8/10 → 0/10）
- Windows 读线程使用独立短读超时（100ms，仅作为 `bytes_to_read()` 与 `ReadFile` 竞态的兜底），不再受用户配置的 `read_timeout_ms`（默认 500ms）影响；Unix（Linux/macOS）读线程仍为 `poll(2)` 事件驱动，行为不变

## [0.3.0] - 2026-08-04

### Added

- 事件驱动/非阻塞读线程 `src/reader.rs`（平台适配层）：Unix（Linux/macOS）用 `poll(2)` + 自建管道事件驱动、停止可被管道唤醒；Windows 用 1ms 轮询 + `bytes_to_read()` 门控 + `timeBeginPeriod(1)`，仅在数据就绪时 `read()`，读写延迟不再受读超时参数影响

### Changed

- 默认 `read_timeout_ms` 从 10ms 调整为 500ms：新读线程模型下该参数仅作为 `read()` 的安全上限（检测异常超时），不再影响读写延迟，可容纳板端命令执行时间较长的情形
- `uart_close` 延迟从 ~116ms 降至 ~1.4ms（事件等待可被停止令牌中断）；`uart_write` 净开销中位降至 ~0.4ms

### Fixed

- 消除 Windows USB 转串口驱动按读超时边界成批交付数据导致的延迟尖峰（该现象实测于手头的 CH340 / CP210x）：`read_timeout_ms=1000` 时读写往返不再呈 ~1s 整数倍（COM9 回环中位 59ms，与默认配置一致；旧模型为 2966ms）

### Docs

- README 与模块文档同步事件驱动/非阻塞读线程说明、`read_timeout_ms` 语义（默认 500ms 仅作读安全上限）与延迟调优指引

## [0.2.2] - 2026-08-04

### Changed

- 默认 `read_timeout_ms` 从 100ms 调整为 10ms：Windows 上 CH340 / CP210x 等 USB 转串口驱动对阻塞读按超时边界成批交付数据（实测于手头这两颗芯片），调小该值可显著降低 `uart_read` / `uart_exchange` 延迟（1000ms 时延迟呈 ~1s 整数倍，10ms 时与直连串口相当）

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
[0.3.0]: https://github.com/woooooooooolf/ser2mcp/releases/tag/v0.3.0
