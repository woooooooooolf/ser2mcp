# ser2mcp

[![CI](https://github.com/woooooooooolf/ser2mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/woooooooooolf/ser2mcp/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/woooooooooolf/ser2mcp?sort=semver)](https://github.com/woooooooooolf/ser2mcp/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Stars](https://img.shields.io/github/stars/woooooooooolf/ser2mcp)](https://github.com/woooooooooolf/ser2mcp)
[![Forks](https://img.shields.io/github/forks/woooooooooolf/ser2mcp)](https://github.com/woooooooooolf/ser2mcp/fork)
[![Last commit](https://img.shields.io/github/last-commit/woooooooooolf/ser2mcp)](https://github.com/woooooooooolf/ser2mcp/commits/main)
[![Downloads](https://img.shields.io/github/downloads/woooooooooolf/ser2mcp/total)](https://github.com/woooooooooolf/ser2mcp/releases)

[![Windows](https://img.shields.io/badge/Windows-0078D6?logo=windows&logoColor=white)](https://github.com/woooooooooolf/ser2mcp/releases)
[![Linux](https://img.shields.io/badge/Linux-FCC624?logo=linux&logoColor=black)](https://github.com/woooooooooolf/ser2mcp/releases)
[![macOS](https://img.shields.io/badge/macOS-000000?logo=apple&logoColor=white)](https://github.com/woooooooooolf/ser2mcp/releases)
[![GitHub Actions](https://img.shields.io/badge/GitHub_Actions-2088FF?logo=githubactions&logoColor=white)](https://github.com/woooooooooolf/ser2mcp/actions)

![Rust](https://img.shields.io/badge/Rust-000000?logo=rust&logoColor=white)
![Rust (top language)](https://img.shields.io/github/languages/top/woooooooooolf/ser2mcp)
![Edition 2024](https://img.shields.io/badge/Edition-2024-000000)
![MSRV 1.85](https://img.shields.io/badge/MSRV-1.85-orange)
![tokio](https://img.shields.io/badge/tokio-runtime-2CA5E0?logo=tokio)
![rmcp](https://img.shields.io/badge/rmcp-MCP_SDK-4B32C3)
![serialport](https://img.shields.io/badge/serialport-4.9-2C3E50)

![MCP](https://img.shields.io/badge/MCP-Model_Context_Protocol-000000?logo=modelcontextprotocol&logoColor=white)
![UART](https://img.shields.io/badge/UART-Serial_Port-007EC6)
![hex/text](https://img.shields.io/badge/hex%2Ftext-binary_safe-00ADD8)
![unsafe](https://img.shields.io/badge/unsafe-none-2EA043)
![single binary](https://img.shields.io/badge/single_binary-no_runtime-512BD4)

简体中文 | [English](README.en.md)

**UART 串口 MCP 服务器**：把本地串口设备封装成标准的 **MCP (Model Context Protocol) 工具**，让 AI 助手（Claude Desktop、Cursor 及任何 MCP 客户端）直接读写串口。

```mermaid
flowchart LR
    client["MCP 客户端<br>（AI 助手）"]
    server["ser2mcp<br>（事件驱动读线程+环形缓冲）"]
    uart["UART 设备<br>（TX-RX）"]

    client <==>|"JSON-RPC over stdio"| server
    server <==>|"串口"| uart
```

## 特性

- **9 个 MCP 工具**：枚举端口、打开、运行时重配置、写、读、写+读、状态、清缓冲、关闭
- **完善的串口参数配置**：波特率 / 数据位(5-8) / 校验位(none/even/odd) / 停止位(1,2) / 流控(none/software/hardware) / 读超时，均可在 `uart_open` / `uart_configure` 中指定
- **内部参数可配置**：环形缓冲大小 `buffer_size`（默认 1 MiB）、空闲判定 `idle_ms`、单次拉取上限 `max_bytes`、总超时 `timeout_ms`、读线程超时 `read_timeout_ms`（默认 500ms，仅作读安全上限，不影响延迟）
- **事件驱动/非阻塞读线程（平台适配层）**：Unix（Linux/macOS）用 `poll(2)` + 自建管道事件驱动；Windows 用 1ms 轮询 + `bytes_to_read()` 门控 + `timeBeginPeriod(1)`，仅在数据就绪时 `read()`，读写延迟不再受读超时参数影响
- **上行数据不丢不堵**：事件驱动/非阻塞读线程持续把串口数据囤积进环形缓冲；写满后覆盖最旧数据并**累计溢出计数**，返回值带 `overflow_delta / overflow_total`，数据缺口可检测
- **二进制安全**：数据以 hex 字符串传递（如 `"41 54 0D 0A"`），`mode="text"` 可切换 UTF-8 文本
- **单二进制交付**：`cargo build --release` 产出单个可执行文件，Windows / Linux / macOS 均无需额外运行时

## 快速安装

> 也可以直接从 [Releases](https://github.com/woooooooooolf/ser2mcp/releases) 下载对应平台的预编译二进制（Windows / Linux / macOS）。

```bash
# 1. 拉取仓库
git clone https://github.com/woooooooooolf/ser2mcp.git
cd ser2mcp

# 2. Linux 系统依赖（仅 Debian/Ubuntu 需要；macOS/Windows 跳过）
sudo apt-get install -y libudev-dev

# 3. 构建 release 二进制
cargo build --release
# 产物：target/release/ser2mcp（Windows 下为 ser2mcp.exe）

# 4. 自检（可选）：枚举本机串口
target/release/ser2mcp --list-ports

# 5. 注册为 MCP server（见下方「接入 MCP 客户端」）
```

**验证安装成功**：注册后调用 `uart_list_ports` 应返回本机串口列表（可能为空数组）；若有 TX-RX 回环硬件，调用 `uart_exchange` 发送的数据应原样返回。

## 构建与测试

> **Linux 用户注意**：`serialport` 枚举 USB 端口信息依赖 `libudev`，编译前需先安装
> Debian/Ubuntu：`sudo apt-get install -y libudev-dev`

```bash
cargo build --release   # 构建
cargo test              # 单元 + 端到端 MCP 协议测试（无需串口硬件）
cargo doc --no-deps     # 生成 Rust 文档
```

## 命令行

下载预编译二进制或构建完成后，可直接运行：

```bash
ser2mcp --list-ports   # 枚举本机串口
ser2mcp --version      # 显示版本号
ser2mcp --help         # 显示帮助
```

不带参数运行即进入 MCP stdio 服务模式（供 AI 助手调用）。

## 接入 MCP 客户端

MCP 客户端以 stdio 方式启动 server 子进程。通用配置（`.mcp.json` / Claude Desktop 等）：

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

Windows 示例：`"command": "C:\\tools\\ser2mcp.exe"`。

环境变量（可选）：

| 变量 | 默认 | 说明 |
|---|---|---|
| `RUST_LOG` | `info` | 日志级别（日志输出到 **stderr**，不污染 stdio 协议通道） |

## 工具一览

| 工具 | 说明 |
|---|---|
| `uart_list_ports` | 枚举本机可用串口（名称/类型/USB 描述） |
| `uart_open` | 打开串口并启动读线程（`port` 必填；含全部串口参数 + `buffer_size` 等内部参数） |
| `uart_configure` | 运行时重配置（`port` 必填，仅更新传入项） |
| `uart_write` | 发送数据，立即返回（`port` 必填，不等回复） |
| `uart_read` | 拉取上行缓冲（`port` 必填） |
| `uart_exchange` | 发送 + 读取一步完成（`port` 必填，最常用） |
| `uart_available` | 状态快照：配置、缓冲未读字节数、累计溢出、读线程错误（`port` 必填） |
| `uart_clear` | 清空未读缓冲（`port` 必填） |
| `uart_close` | 关闭串口并释放句柄（`port` 必填） |

> **多端口与透传**：支持同时打开多个串口，端口名（如 `COM3`、`/dev/ttyUSB0`）就是句柄，除 `uart_list_ports` 外每个工具都要指定 `port`。串口字节流**原样透传**：ser2mcp 不做内容解析、匹配或过滤，非预期数据也会原样返回，由 AI 与上层自行判断。

### 读取语义（核心设计）

串口上行数据由事件驱动/非阻塞读线程**持续囤积**，工具**按需拉取**，`uart_read` / `uart_exchange` 在以下三种条件之一满足时返回全部未读数据：

1. **空闲判定**：出现新数据后持续 `idle_ms`（默认 300ms）无新字节 → 视为一次响应结束（`reason: "idle"`）
2. **达到上限**：未读字节数 ≥ `max_bytes`（默认 64 KiB）→ 防堆积（`reason: "max_bytes"`）
3. **总超时**：等待超过 `timeout_ms`（默认 5000ms）（`reason: "timeout"`）

返回值示例：

```json
{
  "data": "41 54 0D 0A 4F 4B 0D 0A",
  "bytes": 8,
  "mode": "hex",
  "reason": "idle",
  "overflow_delta": 0,
  "overflow_total": 0,
  "buffered_bytes": 0
}
```

> `overflow_delta > 0` 表示自上次读取以来有数据因缓冲写满被覆盖丢弃——数据有缺口，应调大 `buffer_size` 或降低拉取间隔。

## 典型用法（AI 助手视角）

```
1. uart_list_ports                      → 找到 "COM3"
2. uart_open {port: "COM3", baudrate: 115200}
3. uart_exchange {port: "COM3", data: "41 54 0D 0A"}  → 发 "AT\r\n"，等回复
4. uart_configure {port: "COM3", baudrate: 9600}      → 设备切换波特率后重配置
5. uart_close {port: "COM3"}
```

> **延迟提示（AI 工具注意）**：ser2mcp 使用事件驱动/非阻塞读线程（Unix `poll`、Windows 1ms 轮询），`read_timeout_ms`（默认 500ms）只是读安全上限，不影响读写延迟。单次读写往返的固定等待主要来自 `idle_ms`（默认 300ms）；如需更低延迟，可按设备响应节奏调小 `uart_exchange` / `uart_read` 的 `idle_ms`（例如 50ms；注意保持大于设备响应间隙，否则可能截断响应）。

## 回环自测（真实硬件，TX-RX 短接）

内置一键自测工具：枚举串口 + 对指定端口做完整回环验证（发送 0x00-0xFF 全字节序列并校验原样返回）：

```bash
cargo run --release --example loopback -- --list      # 枚举本机串口
cargo run --release --example loopback -- COM3 115200 # 回环测试
```

## 模块结构

```
src/
├── main.rs      # 入口：stdio 传输启动
├── lib.rs       # crate 文档与模块声明
├── hex.rs       # hex 编解码（hex/text 双模式）
├── ring.rs      # 有界环形缓冲（覆盖最旧 + 溢出计数 + Notify 唤醒）
├── manager.rs   # 串口管理器（打开/重配置/读线程/写/拉取）
├── reader.rs    # 事件驱动/非阻塞读线程（平台适配层）
└── server.rs    # MCP 工具层（9 个工具 + ServerHandler）
tests/
└── e2e.rs       # 端到端 MCP 协议测试（子进程真实握手）
examples/
├── loopback.rs      # 回环自测工具
└── latency_probe.rs # 延迟探针（bench/benchw，真实硬件压测）
```

## 技术栈

- [rmcp](https://github.com/modelcontextprotocol/rust-sdk)（官方 Rust MCP SDK）
- [serialport](https://crates.io/crates/serialport)
- tokio / serde / schemars

## 安全提示

ser2mcp 会把串口的读写能力直接交给 AI 助手：已授权的 MCP 客户端（以及背后的模型）可以向串口设备发送任意字节。请只连接你信任的设备，并确保 MCP 客户端与模型来源可信；不要把该工具用于可能因错误指令而损坏的设备。

## 常见问题

- **Linux 下提示权限不足 / 无法打开 `/dev/ttyUSB0`**：当前用户不在 `dialout`（或 `uucp`）组。以 root 运行 `scripts/linux-serial-permissions.sh`，注销并重新登录后生效。
- **端口打开失败 / 提示已被占用**：确认没有其他串口终端或 MCP 实例占用该端口。
- **Windows 下枚举不到串口**：检查 CH340 / CP210x 等 USB 转串口驱动是否已安装。
- **工具调用延迟偏高**：单次读写往返的固定等待主要来自 `idle_ms`（默认 300ms）；可按设备响应节奏调小（例如 50ms）。`read_timeout_ms`（默认 500ms）只是读安全上限，不影响延迟。
- **数据不完整或缺失**：返回值 `overflow_delta > 0` 表示缓冲溢出丢数据，应调大 `buffer_size` 或减小拉取间隔。

## License

MIT OR Apache-2.0（见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)）
