# ser2mcp

[![CI](https://github.com/woooooooooolf/ser2mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/woooooooooolf/ser2mcp/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

**UART 串口 MCP 服务器**：把本地串口设备封装成标准的 **MCP (Model Context Protocol) 工具**，让 AI 助手（Reasonix、Claude Desktop、Cursor 及任何 MCP 客户端）直接读写串口。

```
┌──────────────┐  JSON-RPC over stdio  ┌──────────────────┐  串口   ┌──────────┐
│ MCP 客户端    │ ◄────────────────────► │ ser2mcp          │ ◄─────► │ UART 设备 │
│ (AI 助手)     │                        │ 后台读线程+环形缓冲│         │ (TX-RX)  │
└──────────────┘                        └──────────────────┘         └──────────┘
```

## 特性

- **9 个 MCP 工具**：枚举端口、打开、运行时重配置、写、读、写+读、状态、清缓冲、关闭
- **完善的串口参数配置**：波特率 / 数据位(5-8) / 校验位(none/even/odd) / 停止位(1,2) / 流控(none/software/hardware) / 读超时，均可在 `uart_open` / `uart_configure` 中指定
- **内部参数可配置**：环形缓冲大小 `buffer_size`（默认 1 MiB）、空闲判定 `idle_ms`、单次拉取上限 `max_bytes`、总超时 `timeout_ms`
- **上行数据不丢不堵**：后台读线程持续把串口数据囤积进环形缓冲；写满后覆盖最旧数据并**累计溢出计数**，返回值带 `overflow_delta / overflow_total`，数据缺口可检测
- **二进制安全**：数据以 hex 字符串传递（如 `"41 54 0D 0A"`），`mode="text"` 可切换 UTF-8 文本
- **单二进制交付**：`cargo build --release` 产出单个可执行文件，Windows / Linux / macOS 均无需额外运行时

## 快速安装（AI 自动安装指南）

以下步骤供人工或 AI 代理照此执行，即可完成拉取 → 构建 → 注册：

```bash
# 1. 拉取仓库
git clone https://github.com/woooooooooolf/ser2mcp.git
cd ser2mcp

# 2. 构建 release 二进制
cargo build --release
# 产物：target/release/ser2mcp（Windows 下为 ser2mcp.exe）

# 3. 自检（可选）：枚举本机串口
cargo run --release --example loopback -- --list

# 4. 注册为 MCP server（见下方「接入 MCP 客户端」）
```

**验证安装成功**：注册后调用 `uart_list_ports` 应返回本机串口列表（可能为空数组）；若有 TX-RX 回环硬件，调用 `uart_exchange` 发送的数据应原样返回。

## 构建与测试

```bash
cargo build --release   # 构建
cargo test              # 单元 + 端到端 MCP 协议测试（无需串口硬件）
cargo doc --no-deps     # 生成 Rust 文档
```

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
Reasonix 项目配置（`reasonix.toml`）示例：

```toml
[[plugins]]
name    = "ser2mcp"
command = "/absolute/path/to/ser2mcp"
```

环境变量（可选）：

| 变量 | 默认 | 说明 |
|---|---|---|
| `RUST_LOG` | `info` | 日志级别（日志输出到 **stderr**，不污染 stdio 协议通道） |

## 工具一览

| 工具 | 说明 |
|---|---|
| `uart_list_ports` | 枚举本机可用串口（名称/类型/USB 描述） |
| `uart_open` | 打开串口并启动后台读线程（含全部串口参数 + `buffer_size` 等内部参数） |
| `uart_configure` | 运行时重配置（仅更新传入项） |
| `uart_write` | 发送数据，立即返回（不等回复） |
| `uart_read` | 拉取上行缓冲 |
| `uart_exchange` | 发送 + 读取一步完成（最常用） |
| `uart_available` | 状态快照：配置、缓冲未读字节数、累计溢出、读线程错误 |
| `uart_clear` | 清空未读缓冲 |
| `uart_close` | 关闭串口并释放句柄 |

### 读取语义（核心设计）

串口上行数据由后台读线程**持续囤积**，工具**按需拉取**（AI 回合制调用的正确范式），`uart_read` / `uart_exchange` 在以下三种条件之一满足时返回全部未读数据：

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
3. uart_exchange {data: "41 54 0D 0A"}  → 发 "AT\r\n"，等回复
4. uart_configure {baudrate: 9600}      → 设备切换波特率后重配置
5. uart_close
```

## 回环自测（真实硬件，TX-RX 短接）

内置一键自测工具：枚举串口 + 对指定端口做完整回环验证（发送 0x00-0xFF 全字节序列并校验原样返回）：

```bash
cargo run --release --example loopback -- --list     # 枚举本机串口
cargo run --release --example loopback -- COM3 115200 # 回环测试
```

## 模块结构

```
src/
├── main.rs      # 入口：stdio 传输启动
├── lib.rs       # crate 文档与模块声明
├── hex.rs       # hex 编解码（hex/text 双模式）
├── ring.rs      # 有界环形缓冲（覆盖最旧 + 溢出计数 + Notify 唤醒）
├── manager.rs   # 串口管理器（打开/重配置/后台读线程/写/拉取）
└── server.rs    # MCP 工具层（9 个工具 + ServerHandler）
tests/
└── e2e.rs       # 端到端 MCP 协议测试（子进程真实握手）
examples/
└── loopback.rs  # 回环自测工具
```

## 技术栈

- [rmcp](https://github.com/modelcontextprotocol/rust-sdk)（官方 Rust MCP SDK）
- [serialport](https://crates.io/crates/serialport)
- tokio / serde / schemars

## License

MIT OR Apache-2.0（见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)）
