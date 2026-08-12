---
name: ser2mcp-file-transfer
description: 串口文件流式发送指南：uart_send_estimate/uart_send_file/uart_send_cancel 完整流程、chunk_size 选择、EOF 与对账、对端 tty 注意事项。经串口传文件/固件时遵循。
---

# 串口文件流式发送（uart_send_file 系列）

大文件（几 KB 以上，固件下载/文件传输）**不要逐块调 `uart_write`**：每次调用都有协议往返与 token 开销。用 `uart_send_file` 一次调用：服务器内部循环分片（`chunk_size`）+ 片间间隔（`gap_ms`）发送。

## 1. 完整流程（先估算 → 再发送 → 对账）

```
1. uart_send_estimate {path, mode?, chunk_size?, gap_ms?, baudrate?}
   → 先估算发送字节数与耗时（无需打开串口；baudrate 默认 115200）
2. 对端准备接收（见 §4）
3. uart_send_file {port, path, mode?, chunk_size?, gap_ms?}
   → 一次调用发送
4. 结束传输（EOF，见 §4）
5. 对账：wc -c / md5sum 与返回统计比对（见 §4）
```

大文件耗时可能很长（1 MiB @ 115200：text 理论下限约 91 秒，base64 约 123 秒，均未计额外开销），务必先估算并提示用户预期等待。

## 2. 参数语义

| 参数 | 说明 |
|---|---|
| `port` | 串口名（必填） |
| `path` | 本地文件路径（必填；服务器校验存在、是普通文件、可读） |
| `mode` | `text`（默认，原样按字节发）/ `base64`（跨原始分片连续编码，padding 仅在文件末尾；每 76 字符自动换行、末尾补 `\n`） |
| `chunk_size` | 分片大小（原始字节），默认 256，范围 1..=1 MiB。**模型的责任**：宁小勿大——无流控下超限即丢字节且不可恢复 |
| `gap_ms` | 片间间隔（毫秒），默认 0，上限 60000（每片写完 flush 已天然限速到波特率上限） |

**chunk_size 选择**：先查对端 tty 缓冲限制（如板端 `stty -a` 看 `icanon`/行缓冲）与波特率，选 `chunk_size` ≤ 缓冲上限。**base64 模式实际发送 ≈ 文件字节数 × 4/3 + 换行**（每 76 字符一行），按对端缓冲 ÷1.34 取整。

## 3. 返回统计与异常语义

`uart_send_file` 返回：`reason` / `raw_bytes` / `sent_bytes` / `chunks` / `elapsed_ms` / `overflow_delta` / `overflow_total` / `device_error`。

- `reason="completed"`：发送循环已把全部输出字节写入串口驱动；不代表对端已经完整接收。
- `reason="cancelled"`：被 `uart_send_cancel`、`uart_close` 或客户端取消通知（`notifications/cancelled`）中止——检查对端已收内容后决定是否清理并重发。
- `reason="device_error"`：读线程检测到致命错误（串口物理断开/硬件故障）——写侧可能仍"假成功"（数据进驱动缓冲但设备已不在），**以此为准**并做对端对账；`device_error` 含详情。
- `reason` 只表示服务器端的结束状态：`raw_bytes` 是原文件总字节数，`sent_bytes` 是实际写入串口的字节数；base64 下后者包含编码与换行，不能通过两者大小关系判断是否发完。端到端完整性必须用对端字节数与解码后哈希确认。
- 中途写失败/文件读取失败：返回错误，错误信息含已发送字节/片数。
- 发送期间 `uart_available` 可并发查询 `send` 进度（`active` / `sent_bytes` / `total_bytes` / `chunks` / `last_reason`），`uart_clear` 也可并发执行，普通 I/O/配置/期待工具会排队；`uart_send_cancel` 可请求取消，目标端口的 `uart_close` 会主动取消并等待发送退出（最长 30 秒），然后关闭端口。

## 4. 对端准备、EOF 与实测注意事项（真实 Linux 板经验）

- **收二进制（text 模式）先在对端执行 `stty raw`**：默认 tty 的 IXON 流控会消费数据中的 `\x11`/`\x13`（Ctrl-Q/S）、ICRNL 转换 `\r` 等，造成内容错位/缺失；`stty raw` 关闭全部转换。
- **命令行尾 `\r\n` 会在对端 tty 残留 `\n`**（`\r` 触发行交付、`\n` 残留给下一个读取者），被 `cat`/`dd` 先读到会使文件开头多 1 字节——命令用 `newline="lf"` 规避。
- **结束 `cat`（EOF）**：base64 + icanon 下 `uart_write {data: "04"}`（`\x04`）通常触发 EOF；不可靠时改用对端 `dd bs=1 count=N` 收满自动退出（text 模式收满即止，不依赖 EOF）。

**完整示例（base64 传文件到 Linux 板）**：

```
uart_exchange {port, data: "stty -echo; cat > /tmp/f.b64", mode: "text", newline: "lf"}   # 对端开始接收
uart_send_file {port, path: "C:/tmp/fw.bin", mode: "base64", chunk_size: 256}            # 一次调用发完
uart_write {port, data: "04"}                                                            # 补 \x04 结束对端 cat
uart_exchange {port, data: "wc -c /tmp/f.b64; base64 -d < /tmp/f.b64 | md5sum", mode: "text", newline: "lf"}
# 对账：wc -c 应与 sent_bytes 一致；md5sum 应与本地文件一致
```

**text 模式完整示例（对端 `stty raw` + `dd` 收满自动退出）**：

```
uart_exchange {port, data: "stty raw; dd bs=1 count=65536 of=/tmp/f.bin", mode: "text", newline: "lf"}
uart_send_file {port, path: "C:/tmp/f.bin", mode: "text", chunk_size: 1024}
uart_exchange {port, data: "stty sane; wc -c /tmp/f.bin; md5sum /tmp/f.bin; rm -f /tmp/f.bin", mode: "text", newline: "lf"}
# 对账：wc -c 应为 65536，md5sum 应与本地一致；dd 读满 count 自动退出，无需 EOF
```
