---
name: ser2mcp-usage
description: 串口（UART/COM）设备操作指南：14 个 uart_* 工具的编码选择、读取与 expect 语义、命令完成判定、故障排查。需要读写串口/终端/AT 指令时遵循本指南。
---

# ser2mcp 使用指南（uart_* 工具）

ser2mcp 是 MCP 串口服务器：把本地串口暴露为 MCP 工具，字节流原样透传（不解析、不过滤）。本指南面向需要操作串口设备（Linux 板 / MCU / AT 模块 / uboot）的 AI Agent。

## 1. 工具速查

| 工具 | 用途 |
|---|---|
| `uart_list_ports` | 枚举本机串口（名称/类型/USB 描述） |
| `uart_open` | 打开串口并启动读线程（`port` 必填；含波特率等全部参数） |
| `uart_configure` | 运行时重配置（仅更新传入项） |
| `uart_write` | 只发不等回复 |
| `uart_read` | 拉取上行缓冲（idle / 上限 / 超时三条件返回） |
| `uart_exchange` | 写 + 读一步完成（**最常用**） |
| `uart_expect` | 等待输出中出现 pattern 或超时（时序编排核心） |
| `uart_expect_send` | pattern 命中后立即发送 reply（抢时序窗口） |
| `uart_available` | 状态快照：配置、缓冲、溢出、读线程错误、发送进度 |
| `uart_clear` | 清空未读缓冲 |
| `uart_close` | 关闭串口（进行中的文件发送会被中断） |
| `uart_send_estimate` | 估算文件发送字节数与耗时（无需打开串口） |
| `uart_send_file` | 文件流式发送（大文件一次调用，见 ser2mcp-file-transfer） |
| `uart_send_cancel` | 中止进行中的文件发送 |

端口名（`COM3` / `/dev/ttyUSB0`）即句柄；除 `uart_list_ports` 外每个工具都要传 `port`。重复打开同一端口会报错，先 `uart_close` 再开。

## 2. 标准工作流

```
uart_list_ports → uart_open {port, baudrate} → 交互 → uart_close {port}
```

一次只发一个短命令，发送后立即判断完成（见 §5），不要用 sleep 盲等。

## 3. 数据表示与编码

| 编码 | 发送 `mode` | 返回 `read_mode` | 说明 |
|---|---|---|---|
| `hex`（默认） | ✅ | ✅ | 每字节两个大写十六进制字符、空格分隔；二进制安全 |
| `text` | ✅ | ✅ | UTF-8 字符串；返回含任一非文本字节则整体降级为 hex |
| `text-escaped` | ❌ | ✅ | 文本为主，控制字节/非法 UTF-8 转义为 `\xNN`，`\r\n\t` 保留；恒可读不降级 |

- **终端命令务必带行尾**：`newline="crlf"`（追加 `\r\n`）或 data 自带行尾；否则命令停留在设备行缓冲不执行，且残留行缓冲会与下一条命令拼合（如 `"ls"` + `"ls /"` 实际执行 `"lsls /"`）。
- 发送编码仅 `hex`/`text`；`text-escaped` 仅用于返回侧。
- **示例**：
  - 终端（Linux shell/uboot）：`uart_exchange {port, data: "ls /", mode: "text", newline: "crlf", read_mode: "text-escaped"}`
  - AT/二进制帧：`uart_exchange {port, data: "AT\r\n", mode: "text"}` 或 `{data: "AA 55 01 00 0D 0A", mode: "hex"}`

## 4. 读取语义（上行数据按需拉取）

串口上行由读线程持续囤积在环形缓冲（写满覆盖最旧并计数溢出），工具按需拉取。`uart_read` / `uart_exchange` 在以下条件之一满足时返回：

1. **空闲判定**：以收到最后一个字节为起点，持续 `idle_ms`（默认 300ms）无新数据且驱动无残留 → 响应结束（`reason: "idle"`）。`idle_ms` 应大于设备响应间隙（否则响应被截断），调小则降低延迟。
2. **达到上限**：未读字节 ≥ `max_bytes`（默认 64 KiB）→ 防堆积。
3. **总超时**：等待超过 `timeout_ms`（默认 5000ms）。

**诊断**：返回的 `overflow_delta > 0` 表示缓冲溢出有数据被覆盖丢弃——数据有缺口，应调大 `buffer_size` 或减小拉取间隔。

## 5. 命令完成判定（重要）

- 优先用**输出锚点**：`uart_expect` 等待提示符/关键字（shell 的 `"# "`、`"$ "`、uboot 的 `"Zynq>"`、`"login:"` 等），锚点出现即完成，再发下一条；命中即返回（毫秒级），**不要靠加大 timeout 干等**。
- 需要"完成即触发"用 `uart_expect_send`（pattern 命中后在同一临界区内立即发送 reply，如 `{pattern: "Hit any key", reply: "\n"}` 抢 bootdelay 窗口）。
- 仅当设备没有明确锚点（如 AT 命令）时才用 `uart_exchange` 的 idle 判定收尾。
- `pattern` 是**精确子串匹配**（大小写敏感，不支持正则），作用于原始字节：设备输出带 ANSI 颜色码时用纯文本关键字（`"login:"`）仍可命中，返回用 `read_mode="text-escaped"` 即可读。
- `consume=true`（默认）返回"截至 pattern 结尾"的内容，pattern 之后的数据留在缓冲、会混入下次读取；需要精确对齐时先 `uart_clear` 或先 `uart_read` 消费残留。

## 6. 故障排查

- 端口未打开的错误：先 `uart_open`。
- 无响应/超时：查 `uart_available` 的 `read_error`（读线程致命错误，如设备被拔）与 `overflow_total`；设备无输出时用 `uart_expect` 锚点而非盲等。
- 发送文件异常：见 `ser2mcp-file-transfer`（`reason` / `device_error` / `sent_bytes` 对账）。
