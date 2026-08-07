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

- **14 个 MCP 工具**：枚举端口、打开、运行时重配置、写、读、写+读、等待匹配输出、匹配后立即发送、状态、清缓冲、关闭、文件发送（估算/发送/取消）
- **文件流式发送**：`uart_send_file` 一次调用把本地文件分片限速发送到串口（text 原样 / base64 自动换行），替代模型逐块调 `uart_write`；配套 `uart_send_estimate` 耗时估算与 `uart_send_cancel` / `uart_close` / 客户端取消通知三级中止，发送中可查进度
- **完整串口参数配置**：波特率 / 数据位(5-8) / 校验位(none/even/odd) / 停止位(1,2) / 流控(none/software/hardware) / 读超时，均可在 `uart_open` / `uart_configure` 中指定
- **内部参数可配置**：环形缓冲大小 `buffer_size`（默认 1 MiB）、空闲判定 `idle_ms`、单次拉取上限 `max_bytes`、总超时 `timeout_ms`、读线程超时 `read_timeout_ms`（默认 500ms，仅作读安全上限，不影响延迟）
- **事件驱动/非阻塞读线程（平台适配层）**：Unix（Linux/macOS）用 `poll(2)` + 自建管道事件驱动；Windows 用 1ms 轮询 + `bytes_to_read()` 门控 + `timeBeginPeriod(1)`，仅在数据就绪时 `read()`，读写延迟不再受读超时参数影响
- **上行数据持续缓冲**：事件驱动/非阻塞读线程持续把串口数据囤积进环形缓冲；写满后覆盖最旧数据并**累计溢出计数**，返回值带 `overflow_delta / overflow_total`，数据缺口可检测
- **二进制安全**：数据以 hex 字符串传递（如 `"41 54 0D 0A"`），`mode="text"` 可切换 UTF-8 文本；`read_mode="text-escaped"` 文本为主、非文本字节 `\xNN` 转义（终端/日志场景不降级）
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

### 以 Reasonix 插件包安装（推荐，零手动配置）

在 Reasonix 中执行：

> Install the ser2mcp plugin package from https://github.com/woooooooooolf/ser2mcp. Use install_source with kind="auto" (or "plugin").

仓库根目录的 `reasonix-plugin.json` 将 ser2mcp 声明为标准 MCP 服务器（`bin/` 内含 Windows / Linux / macOS 三平台预编译二进制与跨平台启动脚本）：

1. 在 Reasonix 中执行 `install_source`：**源填仓库 URL** `https://github.com/woooooooooolf/ser2mcp`，kind 用 `auto`（自动识别为插件包）或显式 `plugin`，scope 默认 `global`
2. Reasonix 把整个仓库复制到自己的全局插件目录（Windows 为 `%APPDATA%\reasonix\plugins\ser2mcp`），manifest 里的 `command`（相对路径 `bin/ser2mcp.cmd`）按插件包根目录解析——**无需手动改任何路径**
3. 安装后自动注册名为 `ser2mcp` 的 MCP 服务器，工具以 `mcp__ser2mcp__uart_*` 暴露
4. **验证**：调用 `uart_list_ports`，应返回本机串口列表（可能为空数组）

> `bin/ser2mcp.cmd` 是跨平台启动脚本（Unix 按 `uname` 选 `ser2mcp` / `ser2mcp-macos`，Windows 直接调用 `ser2mcp.exe`；注意保持纯 ASCII，cmd.exe 在非 UTF-8 代码页下解析非 ASCII 字节会出错）。
>
> 离线安装：也可用 `install_source` 的本地路径作为源（本地仓库目录或解压后的 release 包目录），同样按插件包方式安装。

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
| `uart_expect` | 等待匹配输出：阻塞直到串口输出中出现指定 pattern 或超时（`port`、`pattern` 必填；可选 `data` 实现"发送+等待"） |
| `uart_expect_send` | 匹配后立即发送：等待 pattern 出现后在同一临界区内发送 reply（`port`、`pattern`、`reply` 必填） |
| `uart_available` | 状态快照：配置、缓冲未读字节数、累计溢出、读线程错误、文件发送进度（`port` 必填） |
| `uart_clear` | 清空未读缓冲（`port` 必填） |
| `uart_close` | 关闭串口并释放句柄（`port` 必填；进行中的文件发送会被中断） |
| `uart_send_estimate` | 估算文件发送字节数与耗时（`path` 必填；无需打开串口，`baudrate` 默认 115200） |
| `uart_send_file` | 文件流式发送：分片限速发送本地文件到串口，一次调用（`port`、`path` 必填） |
| `uart_send_cancel` | 中止进行中的文件发送（`port` 必填；无传输时为 no-op） |

> **多端口与透传**：支持同时打开多个串口，端口名（如 `COM3`、`/dev/ttyUSB0`）就是句柄，除 `uart_list_ports` 外每个工具都要指定 `port`。串口字节流**原样透传**：ser2mcp 不做内容解析或过滤（`uart_expect` / `uart_expect_send` 仅在缓冲中做条件查找、不修改数据），非预期数据也会原样返回，由 AI 与上层自行判断。

### 数据表示（hex / text / text-escaped）

| 编码 | 发送（`mode`） | 返回（`read_mode`） | 说明 |
|---|---|---|---|
| `hex`（默认） | ✅ | ✅ | 每字节两个大写十六进制字符、空格分隔，二进制安全 |
| `text` | ✅ | ✅ | UTF-8 字符串；返回时若含任一非文本字节则**整体降级为 hex**（严格判定） |
| `text-escaped` | ❌ | ✅ | 文本为主：可打印 UTF-8 原样，`\r` `\n` `\t` 保留，控制字节（如 ANSI 颜色码的 ESC）与非法 UTF-8 字节转义为 `\xNN`，字面 `\` 转义为 `\\`；恒可读、不降级 |

> **终端命令务必带行尾**：`uart_write` / `uart_exchange` / `uart_expect` 的 `data` 支持 `newline` 参数（`none` 默认 / `lf` 追加 `\n` / `crlf` 追加 `\r\n`）。shell、uboot 等交互式终端在收到回车前不会执行命令；不带行尾的命令还会**残留设备行缓冲、与下一条命令拼合执行**（实测 `"ls"` + `"ls /"` 会实际执行 `"lsls /"`），因此终端场景请显式传 `newline="crlf"` 或让 `data` 自带 `\r\n`。

### 按场景选择编码（最简示例）

**交互式终端（Linux Shell / uboot）**：命令需行尾触发，输出常含 ANSI 颜色码。

```
uart_exchange {port: "COM3", data: "ls /", mode: "text", newline: "crlf", read_mode: "text-escaped"}
```

- `newline="crlf"`：自动追加 `\r\n`，回车即执行；
- `read_mode="text-escaped"`：文本为主，颜色码等控制字节转义为 `\x1B[...`，输出整段可读；
- 多命令流程用 `uart_expect` 等待提示符锚点（如 `pattern: "# "`）判断命令完成。

**MCU / AT 指令调试**：协议逐字节严格，不应自动追加任何字节。

```
uart_exchange {port: "COM3", data: "AT\r\n", mode: "text"}                 // 文本指令，data 自带行尾
uart_exchange {port: "COM3", data: "AA 55 01 00 0D 0A", mode: "hex"}      // 二进制帧，hex 精确传递
```

- 缺省 `newline="none"`、`mode="hex"`：行为与旧版一致，适配任意协议；
- 返回 `mode="text"` 仅当数据为纯文本时可用，含任意非文本字节将整体降级为 hex，此时改用 `read_mode="text-escaped"`。

### 文件发送（uart_send_estimate → uart_send_file）

大文件（几 KB 以上，固件下载/文件传输）**不要逐块调 `uart_write`**：每次调用都有协议往返与 token 开销。用 `uart_send_file` 一次调用完成：服务器内部循环分片（`chunk_size`）+ 片间间隔（`gap_ms`）发送，模型只调一次。

**典型流程（模型视角）**：

```
1. uart_send_estimate {path, mode?, chunk_size?, gap_ms?, baudrate?}
   → 先估算发送字节数与耗时（无需打开串口）
2. uart_send_file {port, path, mode?, chunk_size?, gap_ms?}
   → 发送，返回 raw_bytes / sent_bytes / chunks / elapsed_ms / overflow 统计
3. 对账：与对端 wc -c / md5sum 比对（sent_bytes 应对应 wc -c；raw_bytes 应对应解码后字节数）
```

**参数与语义**：

| 参数 | 说明 |
|---|---|
| `port` | 串口名（必填） |
| `path` | 本地文件路径（必填；服务器校验存在、是普通文件、可读） |
| `mode` | `text`（默认，原样按字节发）/ `base64`（编码后发，每 76 字符自动换行、末尾补 `\n`，适合对端 icanon 行缓冲 `cat > file`） |
| `chunk_size` | 分片大小（原始字节），默认 256。**模型的责任**：先查对端 tty 缓冲限制（如板端 `stty -a` 看行缓冲/`icanon`）与波特率，选 `chunk_size` ≤ 缓冲上限，宁小勿大——无流控下超限即丢字节且不可恢复 |
| `gap_ms` | 片间间隔（毫秒），默认 0（每片写完 flush 已天然限速到波特率上限，一般无需设置） |

**要点**：

- **只承诺"把文件字节发出去"**：不解析数据格式、不主动发 EOF。对端需要 EOF 时模型用 `uart_write` 补 `\x04`（对端 icanon 下通常触发 EOF；不可靠时改用 `dd bs=1 count=N` 收满自动退出）
- **base64 膨胀**：实际发送 ≈ 文件字节数 × 4/3 + 换行数（每 76 字符一行），选 `chunk_size` 时按对端 tty 缓冲 ÷1.34 取整
- **耗时估算**：8N1 公式 `耗时 ≈ 发送字节数 × 10 / 波特率 + 片数 × gap_ms`；1 MiB @ 115200 ≈ 87 秒，发送前先估算并提示用户预期等待
- **发送期间 io_lock 独占**：`uart_configure` / `uart_close` 会排队到发送结束；`uart_available` 不受影响，可随时查 `send` 进度（`active` / `sent_bytes` / `total_bytes` / `chunks` / `last_reason`）
- **中止**：`uart_send_cancel`（检查点退出，最坏多写一片）、`uart_close`（先中断发送再关闭端口）、客户端取消通知（`notifications/cancelled`）均可；中止时返回 `reason="cancelled"` + 已发统计，模型据此与对端对账后决定是否重发
- **中途失败**：写失败/文件读取失败返回错误，错误信息含已发送字节/片数

**对端实测注意事项**（真实 Linux 板经验）：

- **收二进制（text 模式）先 `stty raw`**：默认 tty 的 IXON 流控会把数据中的 `\x11`/`\x13`（Ctrl-Q/S）消费掉、ICRNL 转换 `\r` 等，造成内容错位/缺失；`stty raw` 关闭全部转换
- **命令行尾的 `\r\n` 会在对端 tty 残留 `\n`**（`\r` 触发行交付、`\n` 残留给下一个读取者），被 `cat`/`dd` 先读到会使文件开头多 1 字节——对账时注意，或命令用 `newline="lf"` 规避
- **结束 `cat`**：base64 + icanon 下 `uart_write {data: "04"}`（`\x04` EOF）通常有效；对账以 `base64 -d | wc -c` / `md5sum` 为准

### 读取语义（核心设计）

串口上行数据由事件驱动/非阻塞读线程**持续囤积**，工具**按需拉取**，`uart_read` / `uart_exchange` 在以下三种条件之一满足时返回全部未读数据：

1. **空闲判定**：以环形缓冲收到**最后一个字节**的时刻为起点，持续 `idle_ms`（默认 300ms）无新数据、且串口驱动侧无待搬入缓冲的字节 → 视为一次响应结束（`reason: "idle"`）
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

> **`idle_ms` 的语义**：它判定的是**响应内部的静默间隙**——相邻数据块的间隔 < `idle_ms` 会合并为同一次响应，> `idle_ms` 会被截断为两次。因此它必须**大于设备的响应间隙**（否则响应被截断），调小则降低往返延迟（判定精度受 10ms 轮询限制）。注意它度量的是"字节流里的静默"，**不是命令执行时间**——慢操作（需数秒）不要靠加大 `idle_ms` 干等，应改用 `uart_expect` 等输出锚点（见下节）。

### 内容匹配语义（uart_expect / uart_expect_send）

与 `uart_read` / `uart_exchange` 的**时间语义**（空闲判定）不同，expect 系列基于**内容匹配**：等待串口输出中出现指定字符串，命中（或超时）即返回，把"设备何时就绪"的判断交给服务器（命中即返回，毫秒级），替代 AI 侧 `sleep`+盲发 的时序编排：

- `uart_expect {port, pattern: "Zynq>", pattern_mode: "text"}`：等待提示符出现；可选 `data` 先发送再等待，一步完成"发送+等待"
- `uart_expect_send {port, pattern: "Hit any key", reply: "\n", pattern_mode: "text"}`：命中瞬间发送按键，抢 bootdelay 窗口

行为要点：

- **精确子串匹配**（大小写敏感），不支持正则；pattern 可跨多次到达分片、跨环形缓冲 wrap，均能命中
- **历史数据立即参与匹配**：调用时缓冲中已有的数据（如 `uart_open` 后已囤积的 bootlog）直接参与查找，可能无需等待即命中
- **consume 语义**：`consume=true`（默认）命中后取走并返回"截至 pattern 结尾"的内容，pattern 之后的数据留在缓冲（后续 `uart_read` 可取）；`consume=false` 纯等待、不消费数据
- **超时语义**：`timeout_ms`（默认 5000）内未命中返回 `matched=false`、`reason="timeout"`，数据不消费（留在缓冲供诊断）
- **溢出注意**：若缓冲溢出覆盖了 pattern 且设备不再重发，expect 会一直等到超时；返回值 `overflow_delta > 0` 可帮助识别该情况
- **ANSI 免疫**：pattern 匹配作用于原始字节、与返回编码无关——设备输出带 ANSI 颜色码时，pattern 用纯文本关键字（如 `"login:"`、`"# "`）仍可命中，返回用 `read_mode="text-escaped"` 即可读
- **残留数据**：`consume=true` 消费后 pattern 之后的数据留在缓冲，会混入下一次 `uart_read` / `uart_exchange` 的返回值（属未读数据、正常语义）；需要精确对齐时先 `uart_clear` 或先 `uart_read` 消费残留

### 使用模式：短命令 + 输出锚点（推荐）

- 一次只发一个**短命令**，发送后立即判断执行是否完成，不要用 `sleep` 盲等
- 完成判定优先用**输出锚点**：`uart_expect` 等待提示符/关键字（如 shell 的 `# `、`$ ` 或设备状态字符串），锚点出现即完成，再发下一条；需要"完成即触发"用 `uart_expect_send`
- 仅当设备没有明确锚点（如 AT 命令）时才用 `uart_exchange` 的 idle 判定收尾
- 慢操作（需数秒）不要靠加大 `timeout_ms` 干等——用 `uart_expect` 等锚点，命中即返回（毫秒级）

## 典型用法（AI 助手视角）

```
1. uart_list_ports                      → 定位 "COM3"
2. uart_open {port: "COM3", baudrate: 115200}
3. uart_exchange {port: "COM3", data: "41 54 0D 0A"}  → AT 指令（hex，自带 \r\n）
4. uart_exchange {port: "COM3", data: "ls /", mode: "text", newline: "crlf", read_mode: "text-escaped"}  → 终端命令（Shell 场景，见上节最简示例）
5. uart_expect {port: "COM3", pattern: "Zynq>", pattern_mode: "text"}  → 等待提示符（时序编排）
6. uart_expect_send {port: "COM3", pattern: "Hit any key", reply: "\n", pattern_mode: "text"}  → 命中即按键（抢 bootdelay 窗口）
7. uart_configure {port: "COM3", baudrate: 9600}      → 设备切换波特率后重配置
8. uart_close {port: "COM3"}
```

> **延迟提示（AI 工具注意）**：ser2mcp 使用事件驱动/非阻塞读线程（Unix `poll`、Windows 1ms 轮询），`read_timeout_ms`（默认 500ms）只是读安全上限，不影响读写延迟；单次读写往返的固定等待主要来自 `idle_ms`（默认 300ms），调优见上文 `idle_ms` 语义。

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
├── hex.rs       # hex 编解码（hex/text/text-escaped 三模式）
├── ring.rs      # 有界环形缓冲（覆盖最旧 + 溢出计数 + Notify 唤醒 + pattern 查找）
├── manager.rs   # 串口管理器（打开/重配置/读线程/写/拉取/期待匹配）
├── reader.rs    # 事件驱动/非阻塞读线程（平台适配层）
└── server.rs    # MCP 工具层（11 个工具 + ServerHandler）
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
