---
name: ser2mcp-usage
description: 通过 ser2mcp 的 uart_* MCP 工具操作 UART/COM 串口设备。用于枚举和打开端口、收发 hex/text 数据、执行终端或 AT 命令、等待提示符或关键字、处理 boot 时序、诊断超时/乱码/缓冲溢出，以及在 uart_exchange、uart_expect、uart_expect_send、uart_read 之间做选择。文件或固件传输改用 ser2mcp-file-transfer。
---

# ser2mcp 串口操作

## 必须遵守

- 按 `uart_list_ports → uart_open → 交互 → uart_close` 操作；重复打开同一端口前先关闭。
- 除 `uart_list_ports` 和 `uart_send_estimate` 外，调用时都传 `port`。
- 一次只发送一条命令，并用设备输出锚点判断完成；不要用 sleep 盲等。
- 终端命令显式带行尾。通常用 `newline="crlf"`；已知设备只需 LF 时用 `lf`。
- 把 `reason="idle"` 解释为“字节流暂时静默”，不要解释为命令已完成。
- 检查每次读取结果的 `overflow_delta`；大于 0 表示数据已被覆盖，当前结果有缺口。
- 大文件或固件使用 `ser2mcp-file-transfer`，不要循环调用 `uart_write`。

## 选择工具

| 目标 | 工具 | 判定依据 |
|---|---|---|
| 枚举、打开、重配置、关闭端口 | `uart_list_ports` / `uart_open` / `uart_configure` / `uart_close` | 端口状态 |
| 只发送、不等待回复 | `uart_write` | 返回写入字节数 |
| 发送短命令，设备没有稳定锚点 | `uart_exchange` | idle / 上限 / 超时 |
| 发送命令并等待提示符、关键字或结束标记 | `uart_expect`（带 `data`） | pattern 命中 / 超时 |
| 只等待已经在进行的输出 | `uart_expect`（不带 `data`） | pattern 命中 / 超时 |
| 命中输出后立即回复，如抢 bootdelay | `uart_expect_send` | pattern 命中后原子发送 reply |
| 拉取已积累的上行数据 | `uart_read` | idle / 上限 / 超时 |
| 查看缓冲、溢出、读线程错误或发送进度 | `uart_available` | 状态快照 |
| 清空宿主侧未读数据 | `uart_clear` | 清除字节数 |
| 发送本地文件 | `uart_send_file` | 使用 `ser2mcp-file-transfer` |

## 执行交互

1. 调用 `uart_list_ports`，从返回结果中确定端口名。
2. 调用 `uart_open {port, baudrate, ...}`。端口名就是后续调用的句柄。
3. 根据设备能力选择完成判据：
   - 有提示符或确定结束标记：用 `uart_expect`。
   - 无稳定锚点、响应很短：用 `uart_exchange` 的 idle 收尾。
   - 需要命中即响应：用 `uart_expect_send`。
4. 选择编码并发送：
   - 二进制协议：发送 `mode="hex"`，读取 `read_mode="hex"`。
   - 终端/日志：发送 `mode="text"`，读取 `read_mode="text-escaped"`。
   - 纯 UTF-8 响应：可用 `read_mode="text"`；遇到非文本字节时整体降级为 hex。
5. 检查 `reason`、`overflow_delta`、`read_error` 和实际返回内容，再决定下一步。
6. 完成后调用 `uart_close`。

常用调用：

```text
uart_expect {port: "COM3", data: "ls /", mode: "text", newline: "crlf", pattern: "# ", pattern_mode: "text", read_mode: "text-escaped"}
uart_exchange {port: "COM3", data: "AT\r\n", mode: "text", read_mode: "text-escaped"}
uart_exchange {port: "COM3", data: "AA 55 01 00 0D 0A", mode: "hex", read_mode: "hex"}
uart_expect_send {port: "COM3", pattern: "Hit any key", pattern_mode: "text", reply: "\n", reply_mode: "text"}
```

## 解释结果

- `uart_read` / `uart_exchange` 的返回原因：
  - `idle`：最后一个字节后持续 `idle_ms` 无新数据；可能只是中间静默。
  - `max_bytes`：未读数据达到 `max_bytes`；继续读取剩余数据。
  - `timeout`：总等待达到 `timeout_ms`；结合实际返回内容判断是否已有部分响应。
- `uart_expect` 的 `matched=true` 才表示找到指定锚点。pattern 是大小写敏感的原始字节子串，不支持正则。
- `consume=true`（默认）只消费到 pattern 结尾；pattern 之后的数据保留在缓冲，会进入后续读取。
- 调用 `uart_expect` 时，缓冲中已有的历史数据立即参与匹配。需要只匹配新输出时，先读取或清理残留。
- `overflow_delta > 0` 表示本次观察区间内有字节被覆盖；调大 `buffer_size` 或更频繁地读取，并重新获取关键数据。

## 处理终端状态

- 命令停在行缓冲：补正确行尾；不要连续发送第二条命令，否则可能与残留拼接。
- 长命令中途静默：等待提示符或命令特有结束标记，并把 `timeout_ms` 设到足以覆盖整个操作；不要依赖 `uart_exchange` 的 idle。
- 提示符不可用：改用命令特有输出，如 `OK`、长度行或显式打印的结束标记。
- 需要清除板端当前输入行：仅在确认 tty 为 icanon 时发送 `\x15`（Ctrl+U）；需要中断当前命令时可发送 `\x03`（Ctrl+C）。`uart_clear` 只清宿主缓冲，不清板端状态。
- 输出缺失或设备拔出：调用 `uart_available` 检查 `read_error` 和 `overflow_total`。

## 资源边界

- 波特率：`50..=4000000`
- `buffer_size`：`1..=16 MiB`
- read/exchange/expect `timeout_ms`：最大 `300000`
- expect pattern：编码后最大 `64 KiB`
- 普通 I/O、配置、expect 和 close 共享全局 I/O 锁；文件发送期间会排队。`uart_available` / `uart_clear` 可并发，`uart_send_cancel` 可请求取消。
