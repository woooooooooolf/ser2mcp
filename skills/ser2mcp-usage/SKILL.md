---
name: ser2mcp-usage
description: 通过 ser2mcp 的 uart_* MCP 工具操作 UART/COM 串口设备。用于枚举和打开端口、收发 hex/text 数据、执行终端或 AT 命令、等待提示符或关键字、处理 boot 时序、诊断超时/乱码/缓冲溢出，以及在 uart_exchange、uart_expect、uart_expect_send、uart_read 之间做选择。文件或固件传输改用 ser2mcp-file-transfer。
---

# ser2mcp 串口操作

## 5 步最小 happy path

以下 Linux Shell 示例可直接作为起点；把 `COM3`、波特率、命令、行尾和完成 pattern 换成目标设备的实际协议：

1. `uart_list_ports {}`，按端口名和用途选择目标，不只按 USB serial 合并。
2. `uart_open {port: "COM3", baudrate: 115200}`。
3. `uart_expect {port: "COM3", data: "printf 'SER2MCP_%s\\n' OK", mode: "text", newline: "lf", pattern: "SER2MCP_OK", pattern_mode: "text", match_scope: "new", read_mode: "text-escaped"}`。
4. 确认 `matched=true` 且 `overflow_delta=0`；仅当 `pending=true` 时补一次 `uart_read {port: "COM3", read_mode: "text-escaped"}`。
5. `uart_close {port: "COM3"}`。

## 必须遵守

- 按 `uart_list_ports → uart_open → 交互 → uart_close` 操作；重复打开同一端口前先关闭。
- `uart_list_ports` 中相同 USB serial 对应多个 COM 项时，可能是同一芯片暴露的多个串口实例；保留每个端口名并按实际功能逐一确认。
- 除 `uart_list_ports` 和 `uart_send_estimate` 外，调用时都传 `port`。
- 一次只发送一条命令，并用设备协议定义的响应特征判断完成；不要用 sleep 盲等。
- 把 `matched=true` 解释为“pattern 按所选原始字节/忽略 ANSI 语义在匹配范围内命中”，不要直接解释为当前事务成功。
- 终端命令和 `uart_expect_send.reply` 显式带行尾。通常用 `newline="crlf"`；已知设备只需 LF 时用 `lf`。
- 把 `reason="idle"` 解释为“字节流暂时静默”，不要解释为命令已完成。
- 检查每次读取结果的 `overflow_delta`；大于 0 表示数据已被覆盖，当前结果有缺口。
- 大输出优先让板端重定向到文件，再只读 `wc -c` 与 `sha256sum`（不可用时 `md5sum`）摘要对账；不要把完整内容读进 Agent 上下文。
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

## 完整参数清单

各工具只接受本行列出的字段，参数不会从相似工具继承；所有带参工具都会拒绝未知字段。文件相关三个工具的完整参数见 `ser2mcp-file-transfer`。

| 工具 | 必填字段 | 可选字段 |
|---|---|---|
| `uart_list_ports` | 无 | 无 |
| `uart_open` | `port` | `baudrate`, `data_bits`, `parity`, `stop_bits`, `flow_control`, `read_timeout_ms`, `buffer_size`, `discard_on_open` |
| `uart_configure` | `port` | `baudrate`, `data_bits`, `parity`, `stop_bits`, `flow_control`, `read_timeout_ms` |
| `uart_write` | `port`, `data` | `mode`, `newline` |
| `uart_read` | `port` | `idle_ms`, `max_bytes`, `timeout_ms`, `read_mode` |
| `uart_exchange` | `port`, `data` | `mode`, `newline`, `idle_ms`, `max_bytes`, `timeout_ms`, `read_mode` |
| `uart_expect` | `port`, `pattern` | `pattern_mode`, `timeout_ms`, `consume`, `match_scope`, `ignore_ansi`, `data`, `mode`, `newline`, `read_mode` |
| `uart_expect_send` | `port`, `pattern`, `reply` | `pattern_mode`, `reply_mode`, `newline`, `timeout_ms`, `consume`, `match_scope`, `ignore_ansi`, `read_mode` |
| `uart_available` | `port` | 无 |
| `uart_clear` | `port` | 无 |
| `uart_close` | `port` | 无 |

`uart_expect` / `uart_expect_send` 按 pattern 与总超时工作，不接受 `idle_ms` 或 `max_bytes`；需要 idle/字节上限收尾时使用 `uart_read` 或 `uart_exchange`。

## 执行交互

1. 调用 `uart_list_ports`，从返回结果中确定端口名。
2. 调用 `uart_open {port, baudrate, ...}`。端口名就是后续调用的句柄。
3. 根据设备协议选择完成判据：
   - 交互式终端：提示符、命令特有结束标记或实际输出特征。
   - AT / 无回显命令设备：`OK` / `ERROR`、响应 opcode、事务 ID 或设备状态。
   - 二进制帧协议：帧类型、地址、序列号、长度与校验；“命令回显”概念不适用。
   - 没有稳定响应特征且响应很短：才用 `uart_exchange` 的 idle 收尾。
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
uart_expect {port: "COM3", pattern: "BUILD OK", pattern_mode: "text", ignore_ansi: true, read_mode: "text-escaped"}
uart_expect {port: "COM3", pattern: "READY", pattern_mode: "text", match_scope: "new", read_mode: "text-escaped"}
uart_exchange {port: "COM3", data: "AT\r\n", mode: "text", read_mode: "text-escaped"}
uart_exchange {port: "COM3", data: "AA 55 01 00 0D 0A", mode: "hex", read_mode: "hex"}
uart_expect_send {port: "COM3", pattern: "Hit any key", pattern_mode: "text", reply: "y", reply_mode: "text", newline: "crlf", read_mode: "text-escaped"}
```

## 解释结果

- `uart_read` / `uart_exchange` 的返回原因：
  - `idle`：最后一个字节后持续 `idle_ms` 无新数据；可能只是中间静默。
  - `max_bytes`：未读数据达到 `max_bytes`；继续读取剩余数据。
  - `timeout`：总等待达到 `timeout_ms`；结合实际返回内容判断是否已有部分响应。
- `uart_read` / `uart_exchange` 只有 `timeout` 才可能伴随 `bytes=0`。`new_data_observed` 表示调用后是否观察到新上行数据；返回历史缓冲时它可能为 false。
- `pending=true` 表示返回快照中仍有未读缓冲，应继续 `uart_read`；`pending=false` 只表示该瞬间缓冲为空，不证明设备不会继续输出，也不证明命令完成。
- `uart_exchange` 会保留并返回调用前的历史缓冲，但历史数据不会单独触发 idle/max_bytes；收尾前至少等到一批本次写入后的新上行数据。
- `uart_expect` 的 pattern 大小写敏感且不支持正则。默认按原始字节匹配；仅在彩色终端中可设 `ignore_ansi=true` 跳过常见 CSI、OSC 等 ANSI 控制序列。该选项只影响匹配，返回 data 仍保留原始字节；二进制协议不要启用。
- `matched=true` 只证明 pattern 按所选原始/忽略 ANSI 语义命中，不证明它来自当前事务或具有设备协议上的成功含义。
- `match_scope="buffer"`（默认）同时匹配历史未读与调用后新数据；只等待未来响应或事件时用 `"new"`。`new` 只限制 pattern 起点，`consume=true` 的返回仍可能包含水位之前的历史前缀。
- `consume=true`（默认）只消费到 pattern 结尾；pattern 之后的数据保留在缓冲，会进入后续读取。
- `uart_expect` 返回后，`pending=true`（等价于 `buffered_bytes > 0`）时补一次 `uart_read`；`pending=false` 仍是瞬时快照，确实需要 pattern 后的未来输出时继续按协议等待。不要把 follow-up read 当成固定步骤。
- 不要固定在 expect 前调用 `uart_clear`：历史数据无价值且允许丢弃时可清理；需要保留启动日志、异步事件或遥测时使用 `match_scope="new"`。
- `overflow_delta > 0` 表示本次观察区间内有字节被覆盖；调大 `buffer_size` 或更频繁地读取，并重新获取关键数据。

## 处理终端状态

- 仅对开启输入回显的终端：若发送文本连续包含 pattern，回显可能先于实际输出命中。关闭回显，或使用完整 pattern 不连续出现在输入中的实际输出锚点。返回中包含命令行本身既不能证明假阳性，也不能证明事务成功。
- 命令停在行缓冲：补正确行尾；不要连续发送第二条命令，否则可能与残留拼接。
- 长命令中途静默：等待提示符或命令特有结束标记，并把 `timeout_ms` 设到足以覆盖整个操作；不要依赖 `uart_exchange` 的 idle。
- 提示符不可用：改用命令特有输出，如 `OK`、长度行或显式打印的结束标记。
- 需要显式结束标记且不能关闭回显：把标记拆开写在命令中，例如等待 `SLEEP-DONE-MARK` 时发送 `sleep 8; printf '%s%s\n' 'SLEEP-DONE-' 'MARK'`。回显不含连续的完整 pattern，实际输出才包含。
- 需要清除板端当前输入行：仅在确认 tty 为 icanon 时发送 `\x15`（Ctrl+U）；需要中断当前命令时可发送 `\x03`（Ctrl+C）。`uart_clear` 只清宿主缓冲，不清板端状态。
- 输出缺失或设备拔出：调用 `uart_available` 检查 `read_error` 和 `overflow_total`。
- `uart_close` 已开始时，新的普通 I/O/配置会报错；`closed=true` 返回后端口保持关闭，`uart_write` 不会隐式重开，继续操作前必须显式 `uart_open`。

## 资源边界

- 波特率：`50..=4000000`
- `buffer_size`：`1..=16 MiB`，只能在 `uart_open` 时设置；需调整时先关闭再重新打开端口
- 所有带参数的工具都拒绝未知字段；拼写错误或把 `buffer_size` 传给 `uart_configure` 会返回参数错误
- read/exchange/expect `timeout_ms`：最大 `300000`
- expect pattern：编码后最大 `64 KiB`
- 普通 I/O、配置、expect 和 close 共享全局 I/O 锁；文件发送期间会排队。关闭一经开始，排队的新普通 I/O/配置会被拒绝。`uart_available` / `uart_clear` 不持有该锁；宿主允许并发或后续任务仍能访问同一服务时，`uart_send_cancel` 可请求取消。
