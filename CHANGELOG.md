# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式，版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added

- 返回编码新增 `read_mode="text-escaped"`：文本为主、非文本字节转义（`src/hex.rs` 新增 `encode_escaped`）。可打印 UTF-8 原样，`\r` `\n` `\t` 保留，其余控制字节（如 ANSI 颜色码的 ESC）与非法 UTF-8 字节转义为 `\xNN`，字面反斜杠转义为 `\\`；输出恒为合法文本、不降级。解决 `text` 模式"任一非文本字节导致整段日志降级 hex"的问题（`uart_read` / `uart_exchange` / `uart_expect` / `uart_expect_send` 均可用）
- 发送新增 `newline` 参数（`none` 默认 / `lf` 追加 `\n` / `crlf` 追加 `\r\n`），作用于 `uart_write` / `uart_exchange` / `uart_expect` 的 `data`：终端命令（shell/uboot 等）显式传 `newline="crlf"` 即自动补齐行尾，避免命令不执行及残留行缓冲与下一条命令拼合（实测 "ls" + "ls /" 会执行 "lsls /"）

### Changed

- 发送编码（`mode`）与返回编码（`read_mode`）校验拆分：`text-escaped` 仅用于返回侧，发送侧误传会得到明确错误；`encode_send` / `encode_recv` 改为大小写不敏感（修复旧版传 `"TEXT"` 等大小写变体可能触发 `unreachable!` 的隐患）
- `uart_write` / `uart_exchange` / `uart_expect` 返回值新增 `newline` 字段（回显实际使用的行尾）

### Docs

- `INSTRUCTIONS` 与 README（中英）新增"数据表示"章节：hex / text / text-escaped 三编码对照表、终端命令行尾必要性（含行缓冲污染风险）、pattern 字节层匹配对 ANSI 免疫（纯文本关键字可命中带颜色码的输出）、expect 消费后残留数据会混入下次读取的提示

## [0.5.0] - 2026-08-06

### Added

- `uart_expect`：等待匹配输出原语（`port`、`pattern` 必填；可选 `data` 实现"发送+等待"一步完成）。阻塞直到串口输出中出现指定字符串（如 `Zynq>`、`Hit any key` 等提示符/关键字）或超时，把时序编排从 AI 侧 `sleep`+盲发 转移到服务器（命中即返回，毫秒级）。`consume=true`（默认）时取走并返回"截至 pattern 结尾"的内容，pattern 之后的数据保留在缓冲；`consume=false` 时纯等待、数据不消费。精确子串匹配（大小写敏感），跨分片/环形 wrap 均可命中，缓冲中已有数据立即参与匹配
- `uart_expect_send`：匹配后立即发送（`port`、`pattern`、`reply` 必填）。等待→命中→发送在同一临界区内一步原子完成，消除"expect 返回 → 再调 write"的往返延迟，适合 bootdelay 抢窗口等时序敏感场景；超时未命中时不发送 reply
- `ring` 新增 `find` / `find_and_take`（锁内原子查找+消费，读线程无法插入覆盖）/ `take_prefix`，配套单元测试覆盖跨分片、跨 wrap、溢出覆盖等场景

### Changed

- 工具面 9 → 11；`uart_exchange` / `uart_write` 等既有工具行为不变（内部写入路径抽取为 `write_locked` 复用）

### Docs

- 明确 `idle_ms` 空闲语义：判定起点为收到最后一个字节的时刻、响应内部静默间隙模型（< `idle_ms` 合并 / > `idle_ms` 截断）、驱动侧无残留字节的完整判定
- 新增使用模式引导：短命令 + 输出锚点判断命令执行完成（`uart_expect` / `uart_expect_send`），同步至 `INSTRUCTIONS` 与 README（中英）

## [0.4.0] - 2026-08-05

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
[0.4.0]: https://github.com/woooooooooolf/ser2mcp/releases/tag/v0.4.0
[0.5.0]: https://github.com/woooooooooolf/ser2mcp/releases/tag/v0.5.0
