---
name: ser2mcp-usage
description: 串口（UART/COM）设备操作指南：14 个 uart_* 工具的编码选择、读取与 expect 语义、命令完成判定、故障排查。需要读写串口/终端/AT 指令时遵循本指南。
---

# ser2mcp 使用指南（uart_* 工具）

ser2mcp 是 MCP 串口服务器：把本地串口暴露为 MCP 工具，字节流原样透传（不解析、不过滤）。本指南面向需要操作串口设备（Linux 板 / MCU / AT 模块 / uboot）的 AI Agent，说明各工具的正确用法与选择原则；设备环境可能复杂多变，具体调试动作由 AI 运行时自行判断（见 §6）。

## 1. 工具速查

| 工具 | 用途 |
|---|---|
| `uart_list_ports` | 枚举本机串口（名称/类型/USB 描述） |
| `uart_open` | 打开串口并启动读线程（`port` 必填；含波特率等全部参数） |
| `uart_configure` | 运行时重配置（仅更新传入项） |
| `uart_write` | 只发不等回复 |
| `uart_read` | 拉取上行缓冲（idle / 上限 / 超时三条件返回） |
| `uart_exchange` | 写 + 读一步完成（**短命令**，idle 收尾） |
| `uart_expect` | 等待输出中出现 pattern 或超时（时序编排核心；可选 data 一步"发送+等待"） |
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

一次只发一个短命令，发送后立即判断完成（见 §5），不要用 sleep 盲等。终端会话中每条命令用输出锚点收尾（`uart_expect` 等提示符，见 §5）。

## 3. 数据表示与编码

| 编码 | 发送 `mode` | 返回 `read_mode` | 说明 |
|---|---|---|---|
| `hex`（默认） | ✅ | ✅ | 每字节两个大写十六进制字符、空格分隔；二进制安全 |
| `text` | ✅ | ✅ | UTF-8 字符串；返回含任一非文本字节则整体降级为 hex |
| `text-escaped` | ❌ | ✅ | 文本为主，控制字节/非法 UTF-8 转义为 `\xNN`，`\r\n\t` 保留；恒可读不降级 |

- **终端命令务必带行尾**：`newline="crlf"`（追加 `\r\n`）或 data 自带行尾；否则命令停留在设备行缓冲不执行，且残留行缓冲会与下一条命令拼合（如 `"ls"` + `"ls /"` 实际执行 `"lsls /"`）。
- 发送编码仅 `hex`/`text`；`text-escaped` 仅用于返回侧。
- **终端场景推荐 `read_mode="text-escaped"`**：恒可读、不降级（ANSI 颜色码等控制字节转义为 `\xNN`）；**纯二进制读取（如固件 dump）仍用 `hex`**（空格分隔、二进制安全）。
- **示例**：
  - 终端（Linux shell/uboot）单命令：`uart_exchange {port, data: "ls /", mode: "text", newline: "crlf", read_mode: "text-escaped"}`
  - 终端会话（推荐，发送+等提示符收尾一步完成）：`uart_expect {port, data: "ls /", mode: "text", newline: "crlf", pattern: "# ", pattern_mode: "text", read_mode: "text-escaped"}`
  - AT/二进制帧：`uart_exchange {port, data: "AT\r\n", mode: "text"}` 或 `{data: "AA 55 01 00 0D 0A", mode: "hex"}`

## 4. 读取语义（上行数据按需拉取）

串口上行由读线程持续囤积在环形缓冲（写满覆盖最旧并计数溢出），工具按需拉取。`uart_read` / `uart_exchange` 在以下条件之一满足时返回：

1. **空闲判定**：以收到最后一个字节为起点，持续 `idle_ms`（默认 300ms）无新数据且驱动无残留 → 响应结束（`reason: "idle"`）。`idle_ms` 应大于设备响应间隙（否则响应被截断），调小则降低延迟。**它度量的是字节流里的静默，不是命令执行时间**——慢操作（需数秒）不要靠加大 `idle_ms` 干等，改用 `uart_expect` 输出锚点。
2. **达到上限**：未读字节 ≥ `max_bytes`（默认 64 KiB）→ 防堆积。
3. **总超时**：等待超过 `timeout_ms`（默认 5000ms）。

**诊断**：返回的 `overflow_delta > 0` 表示缓冲溢出有数据被覆盖丢弃——数据有缺口，应调大 `buffer_size` 或减小拉取间隔。

**语义边界**：`reason: "idle"` 只表示字节流出现静默，**不等于命令执行完成**——慢命令输出中途的静默间隙、或命令失败无输出时，idle 同样会返回。命令是否完成用 §5 的锚点判定，不要用 idle 推断。

## 5. 命令完成判定（重要）

- 优先用**输出锚点**：`uart_expect` 等待提示符/关键字（shell 的 `"# "`、`"$ "`、uboot 的 `"Zynq>"`、`"login:"` 等），锚点出现即完成，再发下一条；命中即返回（毫秒级），**有锚点时不要靠加大 timeout 干等**。提示符**因设备而异、无通用提示符**；提示符不可用（如 echo 关闭、无提示符设备）时，改用命令特有的结束标记（如 `wc -c` 的输出行、`OK` 等状态串）。
- **长命令（wget/tar 解包等，存在中间静默期）**：`uart_expect` 的语义是"等 pattern 或超时"，**与 idle 无关**，天然适配长操作。其 `timeout_ms` 只是兜底上限（上限 5 分钟），命中即提前返回（毫秒级），放大无成本——无中间锚点的长命令，把 `timeout_ms` 放大到覆盖整个命令时长即可；**不要**用 `uart_exchange` 的 idle 判定干等。
- **可选对齐（tty 处于 icanon 时）**：可发送 `\x15`（Ctrl+U）清空板端行缓冲残留、`\x03`（Ctrl+C）中断当前命令，作为每条命令前的对齐步骤；`uart_clear` 只清宿主上行缓冲，**覆盖不到板端残留**。tty 非 icanon（如 `stty raw` 后）时这些控制字节只是普通数据、无效。是否需要对齐由 AI 依据现场判断。
- 需要"完成即触发"用 `uart_expect_send`（pattern 命中后在同一临界区内立即发送 reply，如 `{pattern: "Hit any key", reply: "\n", reply_mode: "text"}` 抢 bootdelay 窗口）。
- 仅当设备没有明确锚点（如 AT 命令）时才用 `uart_exchange` 的 idle 判定收尾。
- `pattern` 是**精确子串匹配**（大小写敏感，不支持正则），作用于原始字节：设备输出带 ANSI 颜色码时用纯文本关键字（`"login:"`）仍可命中，返回用 `read_mode="text-escaped"` 即可读。
- **历史数据立即参与匹配**：调用时缓冲中已囤积的数据（如 `uart_open` 后设备启动的 bootlog）直接参与查找，可能无需等待即命中。
- **溢出注意**：若缓冲溢出覆盖了 pattern 且设备不再重发，expect 会一直等到超时——返回值 `overflow_delta > 0` 可识别该情况。
- `consume=true`（默认）返回"截至 pattern 结尾"的内容，pattern 之后的数据留在缓冲、会混入下次读取；需要精确对齐时先 `uart_clear` 或先 `uart_read` 消费残留。

## 6. 环境假设与接口选择原则

文档示例（提示符锚点、`\x15`/`\x03` 对齐、行缓冲拼合等）依赖以下环境事实，**它们是示例假设，不是工具要求**：

- 设备有提示符且 echo 开启；
- tty 处于 icanon（默认）行缓冲模式；
- 命令有确定性输出。

**失效信号**（出现即说明假设不成立，需换用相应收尾方式；具体处置由 AI 运行时判断）：

- 无提示符/回显消失 → 提示符锚点不可用 → 改用命令特有结束标记；
- tty 非 icanon → 行缓冲与控制字节语义不适用 → 改用结束标记或按字节对账（见 ser2mcp-file-transfer §4）；
- 历史指令残留/输出混乱 → 读取内容与预期不符 → 先消费残留再继续。

**工具语义不变性**：无论设备处于何种状态，工具行为恒定——`uart_expect` 恒为"等 pattern 或超时"、`uart_exchange`/`uart_read` 恒为 idle 判定、字节流原样透传。设备状态只影响"选什么 pattern、能否用 idle 收尾、如何带行尾"，由 AI 依据当前观察自行决定。

**边界**：ser2mcp 仅提供字节透传与等待/读取原语，不做设备状态管理；环境清理、状态恢复与调试策略属于 AI 的工作范畴，本指南不指导具体调试动作。`uart_available`（配置/缓冲/溢出/读线程错误）与读取回显是获取环境信息的渠道，何时使用由 AI 判断。

**接口选择原则**（依设备能力而非设备类型，详见 §5）：

- 有明确输出锚点 → `uart_expect`（可选 data 一步"发送+等待"）；
- 无锚点（如 AT 模块）→ `uart_exchange` 的 idle 判定收尾；
- 需"完成即触发" → `uart_expect_send`；
- 大文件 → `uart_send_file`（见 ser2mcp-file-transfer）。

## 7. 故障排查

- 端口未打开的错误：先 `uart_open`。
- 无响应/超时：查 `uart_available` 的 `read_error`（读线程致命错误，如设备被拔）与 `overflow_total`；设备无输出时用 `uart_expect` 锚点而非盲等。
- 发送文件异常：见 `ser2mcp-file-transfer`（`reason` / `device_error` / `sent_bytes` 对账）。
