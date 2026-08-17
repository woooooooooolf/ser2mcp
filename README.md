# ser2mcp

[![CI](https://github.com/woooooooooolf/ser2mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/woooooooooolf/ser2mcp/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/woooooooooolf/ser2mcp?sort=semver)](https://github.com/woooooooooolf/ser2mcp/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

简体中文 | [English](README.en.md)

ser2mcp 是一个本地 UART 串口 MCP 服务器，把串口枚举、配置、读写、输出匹配和文件发送封装为标准 MCP 工具，供支持 stdio MCP 的 AI 客户端调用。

## 核心能力

- 提供 14 个 `uart_*` 工具，支持多串口、运行时重配置、写后读取和按输出 pattern 编排时序
- 后台持续读取串口数据，使用有界环形缓冲保存未读内容，并报告 `overflow_delta / overflow_total`
- 支持 hex、UTF-8 text 和仅用于返回侧的 text-escaped，适配二进制协议与终端日志
- 通过一次 `uart_send_file` 流式发送本地文件，支持连续 base64、进度查询、估算和取消
- Windows、Linux、macOS 单可执行文件交付，无需安装 Rust 运行时

## 安装与接入

可从 [Releases](https://github.com/woooooooooolf/ser2mcp/releases) 下载对应平台的预编译包，也可以从源码构建：

```bash
git clone https://github.com/woooooooooolf/ser2mcp.git
cd ser2mcp

# Debian/Ubuntu 构建依赖；Windows/macOS 跳过
sudo apt-get install -y libudev-dev

cargo build --release
target/release/ser2mcp --list-ports
```

不带参数运行 `ser2mcp` 即进入 MCP stdio 服务模式。通用客户端配置：

```json
{
  "mcpServers": {
    "ser2mcp": {
      "command": "/absolute/path/to/ser2mcp",
      "args": []
    }
  }
}
```

Windows 路径示例：`"command": "C:\\tools\\ser2mcp.exe"`。日志写入 stderr，可用 `RUST_LOG` 调整级别，默认 `info`。

### Reasonix 插件安装

仓库根目录包含 `reasonix-plugin.json`，`bin/` 内含三平台预编译文件与 POSIX 启动脚本。Manifest 统一使用 `bin/ser2mcp`：Windows 自动解析同名 `ser2mcp.exe`，Linux/macOS 由脚本选择对应二进制。在 Reasonix 中让 Agent 执行：

> Install the ser2mcp plugin package from https://github.com/woooooooooolf/ser2mcp. Use install_source with kind="auto" (or "plugin").

安装后调用 `uart_list_ports` 验证；返回空数组也表示服务器已正常工作，只是当前没有可枚举串口。离线安装时下载完整源码仓库，并把仓库目录作为 `install_source` 的本地路径。

### DeepSeek-Harness 接入

在 DSH 中对 Agent 说：

> 请从 [https://github.com/woooooooooolf/ser2mcp](https://github.com/woooooooooolf/ser2mcp) 安装 ser2mcp 到 DSH，并按照 [docs/DSH_INTEGRATION.md](docs/DSH_INTEGRATION.md) 完成部署。

Agent 应以当前 DSH 版本的配置和目录约定为准，完成 stdio MCP 服务器注册，并安装仓库内的两个 SKILL。

## 工具

| 工具 | 用途 |
|---|---|
| `uart_list_ports` | 枚举串口名称、类型与 USB 描述 |
| `uart_open` / `uart_configure` / `uart_close` | 打开、运行时重配置和关闭端口 |
| `uart_write` | 只发送数据，不等待回复 |
| `uart_read` | 按 idle、字节上限或总超时拉取上行缓冲 |
| `uart_exchange` | 在同一 I/O 临界区完成短命令的写入与 idle 收尾读取 |
| `uart_expect` | 可选发送数据，并等待输出出现指定 pattern |
| `uart_expect_send` | 命中 pattern 后立即发送 reply |
| `uart_available` / `uart_clear` | 查询状态、溢出、错误与发送进度；清空未读缓冲 |
| `uart_send_estimate` | 无需打开串口，估算文件发送字节数和耗时 |
| `uart_send_file` / `uart_send_cancel` | 同步阻塞地流式发送本地文件；请求取消仍在进行的传输 |

除 `uart_list_ports` 和 `uart_send_estimate` 外，其余工具都需要 `port`。端口名（如 `COM3`、`/dev/ttyUSB0`）就是句柄。普通 I/O、配置、expect 和 close 共享全局 I/O 锁；文件发送期间这些调用会排队，`uart_available` / `uart_clear` 仍可并发执行。宿主允许并发调用或后续任务仍能访问同一服务时，可用 `uart_send_cancel` 请求取消仍在进行的发送。

## AI 使用指南

仓库内含两个通用 Agent Skills：

- [`ser2mcp-usage`](skills/ser2mcp-usage/SKILL.md)：工具选择、编码、命令完成判定、缓冲与故障处理
- [`ser2mcp-file-transfer`](skills/ser2mcp-file-transfer/SKILL.md)：文件发送授权、估算、接收端准备、EOF、取消和端到端对账

Reasonix 安装插件后会同时获得这两个 SKILL。Claude Code、Codex 等 Agent 可把 `skills/` 挂载到各自的技能目录。

最重要的语义边界：

- `reason="idle"` 只表示字节流静默，不表示命令已完成；有提示符或结束标记时使用 `uart_expect`
- `uart_exchange` 会返回调用前已有的历史缓冲，但只有观察到本次写入后的新上行数据，才允许按 idle 或字节上限收尾
- `uart_read` / `uart_exchange` 的 `new_data_observed` 表示调用后是否观察到新上行数据；`pending` 表示返回快照中仍有未读缓冲。`pending=false` 不证明设备之后不会继续输出，也不代表命令完成
- `uart_read` / `uart_exchange` 只有 `reason="timeout"` 才可能返回 `bytes=0`；并发清缓冲不会再产生空的 idle/max_bytes 结果
- `matched=true` 只证明匹配范围内出现了原始字节 pattern，不代表设备事务成功；终端用提示符/输出标记，AT 或无回显 MCU 用状态码/事务标识，二进制协议用帧字段与校验
- `uart_expect.match_scope="buffer"`（默认）允许历史未读数据参与匹配；只等待调用后的新数据时用 `"new"`。对开启输入回显的终端，关闭回显或使用完整 pattern 不连续出现在命令文本中的输出锚点
- `uart_expect_send.newline` 作用于 `reply`；终端回复可传 `reply_mode="text"` 和 `newline="crlf"`，不需要把行尾嵌入 reply 文本
- `uart_expect` 默认只消费到 pattern 结尾；返回的 `pending=true`（等价于 `buffered_bytes > 0`）时补一次 `uart_read`。`pending=false` 仍只是瞬时快照；确实需要 pattern 后的未来输出时继续按协议等待
- 带参数工具的未知字段会报错，不再静默忽略；`buffer_size` 只能在 `uart_open` 时设置，需调整时先关闭再重新打开端口
- `overflow_delta > 0` 表示环形缓冲已有数据被覆盖，当前读取结果存在缺口
- `uart_send_file` 的 overflow 是返回时快照；返回后用 `uart_available` / `uart_read` 再确认最新 `overflow_total`，0 不代表最终无溢出
- `uart_send_file` 默认同步阻塞至结束；可选 `max_duration_ms` 只在显式设置时自动止损并返回 `reason="duration_limit"`，通常仍应根据估算等待完成
- `uart_send_file` 的 `reason="completed"` 只表示服务器已完成写入；端到端完整性必须用对端长度和解码后哈希确认

## 验证与开发

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo doc --no-deps
```

真实硬件 TX-RX 回环测试：

```bash
cargo run --release --example loopback -- --list
cargo run --release --example loopback -- COM3 115200
```

Linux 无权打开 `/dev/ttyUSB0` 时，以 root 运行 `scripts/linux-serial-permissions.sh`，然后注销并重新登录。Windows 枚举不到端口时，检查 CH340、CP210x 等 USB 转串口驱动。

## 安全

ser2mcp 会把串口读写和本地文件发送能力交给 AI 客户端。`uart_send_file` 可以读取 ser2mcp 进程有权访问的任意普通文件并经串口发出，服务端不限制目录。请使用权限受限的账户运行，只连接可信设备，并在发送文件前确认路径与目标设备均在用户授权范围内。完整说明见 [SECURITY.md](SECURITY.md)。

## License

MIT OR Apache-2.0（见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)）
