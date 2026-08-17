---
name: ser2mcp-file-transfer
description: 使用 ser2mcp 经 UART/COM 串口发送本地文件或固件。用于 uart_send_estimate、uart_send_file、uart_send_cancel，选择 text/base64、chunk_size/gap_ms 与可选 max_duration_ms，准备 Linux tty 接收端，处理 EOF/取消/设备断开，并用对端长度和哈希验证完整性。
---

# ser2mcp 文件传输

## 必须遵守

- 先确认用户授权把指定本地文件发送到目标设备。`path` 可指向 ser2mcp 进程有权访问的任意普通文件，服务端不限制目录。
- 先调用 `uart_send_estimate`，向用户说明预计字节数和耗时，再调用一次 `uart_send_file`；不要循环调用 `uart_write`。
- `uart_send_file` 默认同步阻塞至结束；估算可接受时优先等待完成，不要把取消或短时限当作常规分段机制。
- 根据对端缓冲能力选择 `chunk_size`，不确定时从默认值 256 开始；无流控时宁小勿大。
- 把 `reason` 解释为服务器端结束状态，不解释为对端完整接收。
- 传输完成后在对端核对字节数和解码后哈希；只有对账一致才能确认端到端完整性。
- `uart_send_file` 返回后立即调用 `uart_available` 或 `uart_read`，以最新 `overflow_total` 确认上行缓冲是否覆盖；发送返回中的 0 不是最终无溢出证明。
- ser2mcp 不主动发送 EOF。开始发送前先确定对端按长度结束，还是需要调用方另发 EOF。

## 执行流程

1. 确认本地 `path`、目标 `port`、文件用途和用户授权范围。
2. 确定对端接收方式：
   - 对端可按确定长度读取：优先用 `dd bs=1 count=N`，无需 EOF。
   - 对端使用 icanon `cat`：可用 base64，并在发送后另发 `\x04` 结束输入。
   - 对端需要原始二进制：先关闭 tty 字节转换，例如 Linux 使用 `stty raw`。
3. 调用估算：

   ```text
   uart_send_estimate {path, mode?, chunk_size?, gap_ms?, baudrate?}
   ```

4. 准备对端接收，再调用一次：

   ```text
   uart_send_file {port, path, mode?, chunk_size?, gap_ms?, max_duration_ms?}
   ```

5. 必要时单独发送 EOF 或等待按长度接收结束。
6. 检查返回状态，并在对端核对长度和哈希。

## 完整参数清单

各工具只接受本行列出的字段，参数不会从相似工具继承；未知字段会被拒绝。

| 工具 | 必填字段 | 可选字段 |
|---|---|---|
| `uart_send_estimate` | `path` | `mode`, `chunk_size`, `gap_ms`, `baudrate` |
| `uart_send_file` | `port`, `path` | `mode`, `chunk_size`, `gap_ms`, `max_duration_ms` |
| `uart_send_cancel` | `port` | 无 |

`uart_send_estimate` 不需要 `port`，其 `baudrate` 只参与耗时估算；`uart_send_file` 使用已打开端口的实际波特率，不接受 `baudrate`。

## 选择发送参数

| 参数 | 选择规则 |
|---|---|
| `mode="text"` | 原样发送文件字节。适合原始二进制或对端按长度接收；不要把名称理解为 UTF-8 转码。 |
| `mode="base64"` | 连续编码整个文件，padding 仅在 EOF；每 76 字符换行，末尾补 `\n`。适合文本安全通道或 icanon 行缓冲。 |
| `chunk_size` | 原始文件分片大小，默认 256，范围 `1..=1 MiB`。应不大于对端可安全接收的缓冲；base64 输出约为原始数据的 1.34 倍并含换行。 |
| `gap_ms` | 分片间隔，默认 0，最大 60000。仅在设备处理能力低于串口持续输入速率时增加。 |
| `max_duration_ms` | 可选自动止损时限，默认不限制。仅在不能接受无限等待或调用预算明确时设置；达到后在检查点返回部分进度。 |
| `baudrate` | 只用于估算，默认 115200；应与实际串口波特率一致。 |

理论参考：1 MiB @ 115200 时，text 下限约 91 秒，base64 约 123 秒，均未计 flush、调度和 `gap_ms` 开销。以 `uart_send_estimate` 的当前结果为准。

## 解释发送结果

- `reason="completed"`：服务器发送循环已把全部输出字节写入串口驱动；仍需对端对账。
- `reason="duration_limit"`：调用方显式设置的 `max_duration_ms` 到达，发送在检查点停止。检查 `sent_bytes` 和对端残留；不要把部分 base64 数据当成可直接续接的完整编码流。
- `reason="cancelled"`：由 `uart_send_cancel`、目标端口的 `uart_close`，或宿主实际发送的客户端取消通知中止。停止等待、结束 Agent 任务或客户端超时本身不保证服务器收到取消。检查对端残留并清理后再决定是否重发。
- `reason="device_error"`：读线程检测到致命错误，如设备断开。即使写调用曾成功，也不要认为设备收到数据；查看 `device_error` 并停止对账假设。
- 中途写入或文件读取失败以工具错误返回，错误消息包含已发送字节数和分片数。
- `raw_bytes`：原文件总字节数。
- `sent_bytes`：实际写入串口的输出字节数；base64 下包含编码和换行，不能与 `raw_bytes` 直接比较来判断完成。
- `chunks`：已完成写入的输出分片数。
- `overflow_delta` / `overflow_total`：从发送开始到生成返回时，读线程已观察到的上行环形缓冲覆盖快照。串口驱动或线路中的尾部字节可能在返回后继续推高计数；即使返回 0，也要用随后的 `uart_available` / `uart_read` 获取最新 `overflow_total`。这些字段不是下行传输完整性证明。

发送期间服务端允许用 `uart_available` 查看 `send.active`、`sent_bytes`、`total_bytes`、`chunks` 和 `last_reason`，也允许 `uart_send_cancel` 请求取消；宿主必须支持并发调用，或在前一次会话/任务停止等待后仍能访问同一 ser2mcp 服务。严格串行且持续等待当前调用的宿主无法同时发出取消，此时依赖事前估算或显式 `max_duration_ms`。普通 I/O、配置和 expect 调用仍会等待全局 I/O 锁。

## 对端接收示例

Base64 写入 Linux 文件：

```text
uart_write {port, data: "stty -echo; cat > /tmp/f.b64", mode: "text", newline: "lf"}
uart_send_file {port, path: "C:/tmp/fw.bin", mode: "base64", chunk_size: 256}
uart_write {port, data: "04"}
uart_exchange {port, data: "wc -c /tmp/f.b64; base64 -d < /tmp/f.b64 | sha256sum", mode: "text", newline: "lf", read_mode: "text-escaped"}
```

确认对端编码文件字节数等于 `sent_bytes`，解码后 SHA-256 等于源文件。

原始二进制按长度接收：

```text
uart_write {port, data: "stty raw; dd bs=1 count=65536 of=/tmp/f.bin", mode: "text", newline: "lf"}
uart_send_file {port, path: "C:/tmp/f.bin", mode: "text", chunk_size: 1024}
uart_exchange {port, data: "stty sane; wc -c /tmp/f.bin; sha256sum /tmp/f.bin", mode: "text", newline: "lf", read_mode: "text-escaped"}
```

把 `count` 设为源文件精确字节数。默认 tty 的 IXON、ICRNL 等转换会破坏任意二进制；发送原始字节前确认已进入 raw 模式。启动接收命令时使用 `newline="lf"`，避免 `\r\n` 中残留的 `\n` 被文件接收程序读入。
