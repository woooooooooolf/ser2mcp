//! MCP 工具层：工具注册 + ServerHandler 实现。
//!
//! 工具面（11 个）：
//! - `uart_list_ports`   枚举本机可用串口
//! - `uart_open`         打开串口（全量串口参数 + 内部参数：缓冲区大小等）
//! - `uart_configure`    运行时重配置（仅更新传入项）
//! - `uart_write`        只发不等
//! - `uart_read`         拉取缓冲（空闲判定/上限/超时三种返回条件）
//! - `uart_exchange`     写 + 读（对 LLM 最常用的一步操作）
//! - `uart_expect`       等待匹配输出（可选"发送+等待"；内容匹配语义）
//! - `uart_expect_send`  匹配后立即发送（等待→命中→发送一步原子完成）
//! - `uart_available`    状态快照（含缓冲统计与读线程错误）
//! - `uart_clear`        清空未读缓冲
//! - `uart_close`        关闭串口
//!
//! 多端口与透传：支持同时打开多个串口，端口名（如 `COM3` / `/dev/ttyUSB0`）即句柄，
//! 除 `uart_list_ports` 外每个工具都要求 `port` 参数；字节流原样透传，不做解析/过滤；
//! `uart_expect` / `uart_expect_send` 仅在缓冲中做条件查找（不修改数据），
//! 命中与否不影响字节流的透传语义。
//!
//! 数据表示：串口数据是二进制，而 MCP 参数/返回值是文本，因此统一用
//! hex 字符串（如 `"41 54 0D 0A"`）传递，`mode` 参数可切换为文本。

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler, handler::server::wrapper::Parameters, model::*, tool,
    tool_handler, tool_router,
};
use serde_json::json;

use crate::hex;
use crate::manager::{
    self, DEFAULT_BUFFER_SIZE, DEFAULT_IDLE_MS, DEFAULT_MAX_BYTES, DEFAULT_READ_TIMEOUT_MS,
    DEFAULT_TIMEOUT_MS, PortConfig, ReadReason, SerialManager,
};

/// 对 AI 助手的使用指引（随 initialize 返回）。
const INSTRUCTIONS: &str = r##"ser2mcp：UART 串口 MCP 服务器（原样透传，不解析、不过滤字节流内容；uart_expect 系列仅在缓冲中做条件查找，不修改数据）。

典型流程：uart_list_ports → uart_open {port} → uart_exchange {port, data}（写+读一步完成）；时序编排用 uart_expect（等待匹配输出）→ uart_close {port}。

多端口：
- 支持同时打开多个串口；端口名（如 "COM3" / "/dev/ttyUSB0"）就是句柄，
  除 uart_list_ports 外的每个工具都需要传 port 参数。
- 重复打开同一端口会报错，请先 uart_close 再打开。

数据表示：
- 二进制一律用 hex 字符串传递（如 "41 54 0D 0A"），每字节两个大写十六进制字符、空格分隔；
  也接受连续串（"41540D0A"）、逗号/分号/0x 前缀等宽松形式。
- 文本模式（mode="text"）下直接传 UTF-8 字符串；返回时若数据非合法文本则自动降级为 hex。
- read_mode="text-escaped"（推荐终端/日志场景）：文本为主，控制字节（如 ANSI 颜色码的 ESC）
  与非法 UTF-8 字节转义为 \xNN（如 \x1B），\r\n\t 保留，恒可读、不降级；字面反斜杠转义为 \\。
- 发送终端命令（shell/uboot 等）务必带行尾：传 newline="crlf"（追加 \r\n）或 data 自带 \r\n，
  否则命令停留在设备行缓冲不执行；且未带行尾的命令会残留缓冲、与下一条命令拼合执行
  （如 "ls" + "ls /" 会实际执行 "lsls /"），造成命令被篡改，务必避免。
- 发送编码（mode）仅支持 hex 或 text；text-escaped 仅用于返回编码（read_mode）。

读取语义（重要）：
- 串口上行数据由事件驱动/非阻塞读线程持续囤积在有界环形缓冲中（写满覆盖最旧并计数溢出），
  工具按需拉取，而非设备主动推送。
- uart_read / uart_exchange 在三种条件下返回：① 空闲判定：以收到最后一个字节为起点，
  持续 idle_ms（默认 300ms）无新数据且驱动侧无残留字节（数据流中不算空闲）；
  ② 未读字节数达到 max_bytes（默认 64KiB）；③ 总等待超过 timeout_ms（默认 5000ms）。
- idle_ms 判定的是响应内部的静默间隙：相邻数据块间隔 < idle_ms 合并为一次响应，
  > idle_ms 截断为两次；应大于设备响应间隙（否则截断），调小则降低延迟。
- 返回值中的 overflow_delta / overflow_total 表示缓冲溢出被覆盖丢弃的字节数，
  大于 0 时说明数据有缺口，应调大 buffer_size 或减小拉取间隔。

命令执行完成判定（重要）：
- 一次只发一个短命令，发送后立即判断执行是否完成，不要用 sleep 盲等；
- 完成判定优先用输出锚点：uart_expect 等待提示符/关键字（如 shell 的 "# "、"$ "
  或设备状态字符串），锚点出现即完成，再发下一条；需要"完成即触发"用 uart_expect_send；
- 仅当设备没有明确锚点（如 AT 命令）时才用 uart_exchange 的 idle 判定收尾；
- 慢操作（需数秒）不要靠加大 timeout_ms 干等——用 uart_expect 等锚点，命中即返回（毫秒级）。

内容匹配语义（uart_expect / uart_expect_send，时序编排利器）：
- uart_expect 等待串口输出中出现指定 pattern（如 "Zynq>"、"Hit any key" 等提示符/关键字，
  pattern_mode="text"），命中或超时后返回；可选 data 实现"发送+等待"一步完成。
  consume=true（默认）时返回"截至 pattern 结尾"的内容，pattern 之后的数据留在缓冲；
  consume=false 时纯等待、数据不消费。调用时缓冲中已有的数据立即参与匹配（可命中历史输出）。
- uart_expect_send 等待 pattern 出现后在同一临界区内立即发送 reply（如
  {"pattern": "Hit any key", "reply": "\\n"} 抢 bootdelay 窗口），超时未命中时不发送。
- 两者均为精确子串匹配（大小写敏感），不支持正则；命中即返回（毫秒级），时序编排见上文
  "命令执行完成判定"。
- pattern 匹配作用于原始字节，与返回编码无关：设备输出带 ANSI 颜色码时，
  pattern 用纯文本关键字（如 "login:"、"# "）仍可命中，返回用 read_mode="text-escaped" 即可读。
- consume=true 返回"截至 pattern 结尾"的内容，pattern 之后的数据留在缓冲，
  会混入下一次 uart_read / uart_exchange 的返回值（属于未读数据，属正常语义）；
  需要精确对齐时先 uart_clear 或先 uart_read 消费残留。
- 注意：若缓冲溢出覆盖了 pattern 且设备不再重发，expect 会一直等到超时；
  返回值中的 overflow_delta > 0 可帮助识别该情况。

回环自测：TX-RX 短接时 uart_exchange 发送的内容应原样返回。"##;

/// 串口工具参数：uart_open。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct OpenArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
    /// 波特率，默认 115200。
    pub baudrate: Option<u32>,
    /// 数据位（5-8），默认 8。
    pub data_bits: Option<u8>,
    /// 校验位 none/even/odd，默认 none。
    pub parity: Option<String>,
    /// 停止位 1 或 2，默认 1。
    pub stop_bits: Option<u8>,
    /// 流控 none/software/hardware，默认 none。
    pub flow_control: Option<String>,
    /// 读线程的串口读超时（毫秒），默认 500。
    /// 在事件驱动/非阻塞读线程下仅作为 `read()` 的安全上限（检测异常超时），
    /// 不再影响读写延迟；板端命令执行时间较长时可适当调大。
    pub read_timeout_ms: Option<u64>,
    /// 上行环形缓冲大小（字节），默认 1048576（1 MiB）；写满覆盖最旧数据并计数溢出。
    pub buffer_size: Option<usize>,
    /// 打开时是否清空串口驱动输入缓冲中残留的旧数据，默认 true。
    pub discard_on_open: Option<bool>,
}

/// 串口工具参数：uart_configure（全部可选，仅更新传入项）。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ConfigureArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
    /// 波特率。
    pub baudrate: Option<u32>,
    /// 数据位（5-8）。
    pub data_bits: Option<u8>,
    /// 校验位 none/even/odd。
    pub parity: Option<String>,
    /// 停止位 1 或 2。
    pub stop_bits: Option<u8>,
    /// 流控 none/software/hardware。
    pub flow_control: Option<String>,
    /// 读超时（毫秒，仅作读安全上限，不影响延迟）。
    pub read_timeout_ms: Option<u64>,
}

/// 串口工具参数：uart_write。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WriteArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
    /// 要发送的数据（hex 字符串或文本，取决于 mode）。
    pub data: String,
    /// 数据编码：hex（默认）或 text。
    pub mode: Option<String>,
    /// 发送后追加的行尾：none（默认，原样发送）/ lf（追加 \n）/ crlf（追加 \r\n）。
    /// 终端命令（shell/uboot 等）建议 crlf，否则命令可能不执行。
    pub newline: Option<String>,
}

/// 串口工具参数：uart_read。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
    /// 空闲判定阈值（毫秒）：以收到最后一个字节为起点，持续该时长无新数据
    /// 且驱动侧无残留字节即返回（响应内部静默间隙），默认 300。
    pub idle_ms: Option<u64>,
    /// 未读字节数达到该值立即返回（防堆积），默认 65536。
    pub max_bytes: Option<usize>,
    /// 总等待超时（毫秒），默认 5000。
    pub timeout_ms: Option<u64>,
    /// 返回编码：hex（默认）、text（非文本数据自动降级为 hex）或 text-escaped
    /// （文本为主，控制字节/非法 UTF-8 以 \xNN 转义，恒可读不降级）。
    pub mode: Option<String>,
}

/// 串口工具参数：uart_exchange。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExchangeArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
    /// 要发送的数据（hex 字符串或文本，取决于 mode）。
    pub data: String,
    /// 发送编码：hex（默认）或 text。
    pub mode: Option<String>,
    /// 发送后追加的行尾：none（默认）/ lf / crlf。终端命令建议 crlf。
    pub newline: Option<String>,
    /// 空闲判定阈值（毫秒）：以收到最后一个字节为起点，持续该时长无新数据
    /// 且驱动侧无残留字节即返回（响应内部静默间隙），默认 300。
    pub idle_ms: Option<u64>,
    /// 未读字节数达到该值立即返回，默认 65536。
    pub max_bytes: Option<usize>,
    /// 总等待超时（毫秒），默认 5000。
    pub timeout_ms: Option<u64>,
    /// 返回编码：hex（默认）、text 或 text-escaped。
    pub read_mode: Option<String>,
}

/// 串口工具参数：uart_available。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AvailableArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
}

/// 串口工具参数：uart_clear。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClearArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
}

/// 串口工具参数：uart_close。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CloseArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
}

/// 串口工具参数：uart_expect（等待匹配输出，可选"发送+等待"）。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExpectArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
    /// 要等待出现的字符串（hex 或 text，取决于 pattern_mode）。
    pub pattern: String,
    /// pattern 编码：hex（默认）或 text。
    pub pattern_mode: Option<String>,
    /// 总等待超时（毫秒），默认 5000。
    pub timeout_ms: Option<u64>,
    /// 命中后是否取走并返回"截至 pattern 结尾"的内容，默认 true；
    /// false 时纯等待（数据留在缓冲，后续可用 uart_read 取走）。
    pub consume: Option<bool>,
    /// 可选：等待前先发送的数据（"发送+等待"一步完成），编码取决于 mode。
    pub data: Option<String>,
    /// data 的编码：hex（默认）或 text。
    pub mode: Option<String>,
    /// data 发送后追加的行尾：none（默认）/ lf / crlf。终端命令建议 crlf。
    pub newline: Option<String>,
    /// 返回 data 字段的编码：hex（默认）、text（非文本数据自动降级为 hex）或 text-escaped。
    pub read_mode: Option<String>,
}

/// 串口工具参数：uart_expect_send（匹配后立即发送）。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExpectSendArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
    /// 要等待出现的字符串（hex 或 text，取决于 pattern_mode）。
    pub pattern: String,
    /// 命中后立即发送的内容（hex 或 text，取决于 reply_mode）；超时未命中时不发送。
    pub reply: String,
    /// pattern 编码：hex（默认）或 text。
    pub pattern_mode: Option<String>,
    /// reply 编码：hex（默认）或 text。
    pub reply_mode: Option<String>,
    /// 总等待超时（毫秒），默认 5000。
    pub timeout_ms: Option<u64>,
    /// 命中后是否取走并返回"截至 pattern 结尾"的内容，默认 true。
    pub consume: Option<bool>,
    /// 返回 data 字段的编码：hex（默认）、text（非文本数据自动降级为 hex）或 text-escaped。
    pub read_mode: Option<String>,
}

fn require_port(port: &str) -> Result<(), McpError> {
    if port.trim().is_empty() {
        Err(McpError::invalid_params("port 不能为空", None))
    } else {
        Ok(())
    }
}

/// 校验发送编码模式：hex 或 text（text-escaped 仅用于返回侧）。
fn parse_send_mode(s: &str) -> Result<(), String> {
    match s.to_ascii_lowercase().as_str() {
        "hex" | "text" => Ok(()),
        "text-escaped" => Err("text-escaped 仅用于返回编码（read_mode），发送编码仅支持 hex 或 text".into()),
        other => Err(format!("mode 仅支持 hex 或 text，收到 {other:?}")),
    }
}

/// 校验返回编码模式：hex、text 或 text-escaped。
fn parse_recv_mode(s: &str) -> Result<(), String> {
    match s.to_ascii_lowercase().as_str() {
        "hex" | "text" | "text-escaped" => Ok(()),
        other => Err(format!("read_mode 仅支持 hex、text 或 text-escaped，收到 {other:?}")),
    }
}

/// 校验换行参数：none / lf / crlf。
fn parse_newline(s: &str) -> Result<(), String> {
    match s.to_ascii_lowercase().as_str() {
        "none" | "lf" | "crlf" => Ok(()),
        other => Err(format!("newline 仅支持 none、lf 或 crlf，收到 {other:?}")),
    }
}

/// 按 newline 参数在发送数据末尾追加行尾字节（none 时原样）。
fn apply_newline(mut data: Vec<u8>, newline: &str) -> Vec<u8> {
    match newline {
        "lf" => data.extend_from_slice(b"\n"),
        "crlf" => data.extend_from_slice(b"\r\n"),
        _ => {}
    }
    data
}

/// 按 mode 编码发送数据（大小写不敏感）。
fn encode_send(data: &str, mode: &str) -> Result<Vec<u8>, String> {
    match mode.to_ascii_lowercase().as_str() {
        "hex" => hex::decode(data),
        "text" => Ok(data.as_bytes().to_vec()),
        _ => Err(format!("mode 仅支持 hex 或 text，收到 {mode:?}")),
    }
}

/// 按 mode 编码返回数据（大小写不敏感）：
/// - `hex`：全 hex；
/// - `text-escaped`：文本为主，非文本字节 `\xNN` 转义（恒为合法文本，不降级）；
/// - `text`：合法文本原样，非法时整体降级为 hex。
fn encode_recv(bytes: &[u8], mode: &str) -> (String, String) {
    match mode.to_ascii_lowercase().as_str() {
        "hex" => (hex::encode(bytes), "hex".to_string()),
        "text-escaped" => (hex::encode_escaped(bytes), "text-escaped".to_string()),
        _ => {
            if hex::is_text(bytes) {
                (
                    String::from_utf8_lossy(bytes).into_owned(),
                    "text".to_string(),
                )
            } else {
                (hex::encode(bytes), "hex (text 降级)".to_string())
            }
        }
    }
}

fn read_reason_str(r: ReadReason) -> &'static str {
    match r {
        ReadReason::Idle => "idle",
        ReadReason::MaxBytes => "max_bytes",
        ReadReason::Timeout => "timeout",
    }
}

/// 构造 expect 系列工具（uart_expect / uart_expect_send）的统一返回值。
fn expect_result(
    outcome: manager::ExpectOutcome,
    pattern: &str,
    read_mode: &str,
    newline: &str,
) -> CallToolResult {
    let (data, used_mode) = encode_recv(&outcome.data, read_mode);
    CallToolResult::structured(json!({
        "matched": outcome.matched,
        "pattern": pattern,
        "data": data,
        "bytes": outcome.data.len(),
        "mode": used_mode,
        "newline": newline,
        "written": outcome.written,
        "reason": match outcome.reason {
            manager::ExpectReason::Matched => "matched",
            manager::ExpectReason::Timeout => "timeout",
        },
        "overflow_delta": outcome.overflow_delta,
        "overflow_total": outcome.overflow_total,
        "buffered_bytes": outcome.buffered,
    }))
}

/// MCP 服务器主体。
#[derive(Clone)]
pub struct Ser2Mcp {
    manager: Arc<SerialManager>,
}

#[tool_router]
impl Ser2Mcp {
    /// 创建服务器实例（内部持有串口管理器，线程安全，可 Clone）。
    pub fn new() -> Self {
        Self {
            manager: Arc::new(SerialManager::new()),
        }
    }

    /// 枚举本机当前可用的串口。
    #[tool(
        description = "枚举本机当前可用的串口（名称、类型、USB 描述）。串口被占用时可能不出现。"
    )]
    async fn uart_list_ports(&self) -> Result<CallToolResult, McpError> {
        match self.manager.list_ports() {
            Ok(ports) => Ok(CallToolResult::structured(json!({ "ports": ports }))),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 打开串口并启动事件驱动读线程（持续把上行数据囤积进环形缓冲）。
    /// 参数：port 必填；baudrate=115200、data_bits=8、parity=none、stop_bits=1、
    /// flow_control=none、read_timeout_ms=500、buffer_size=1048576（覆盖最旧+溢出计数）、
    /// discard_on_open=true。支持同时打开多个串口；同一端口重复打开会报错。
    #[tool(
        description = "打开串口并启动读线程。支持同时打开多个串口（端口名即句柄）；返回当前配置。"
    )]
    async fn uart_open(
        &self,
        Parameters(args): Parameters<OpenArgs>,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        let baudrate = match args.baudrate.map(manager::parse_baudrate) {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Err(McpError::invalid_params(e, None)),
            None => PortConfig::default().baudrate,
        };
        let data_bits = match args.data_bits.map(manager::parse_data_bits) {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Err(McpError::invalid_params(e, None)),
            None => PortConfig::default().data_bits,
        };
        let parity = match args.parity.as_deref().map(manager::parse_parity) {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Err(McpError::invalid_params(e, None)),
            None => PortConfig::default().parity,
        };
        let stop_bits = match args.stop_bits.map(manager::parse_stop_bits) {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Err(McpError::invalid_params(e, None)),
            None => PortConfig::default().stop_bits,
        };
        let flow_control = match args
            .flow_control
            .as_deref()
            .map(manager::parse_flow_control)
        {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Err(McpError::invalid_params(e, None)),
            None => PortConfig::default().flow_control,
        };
        let read_timeout_ms = args.read_timeout_ms.unwrap_or(DEFAULT_READ_TIMEOUT_MS);
        let buffer_size = args.buffer_size.unwrap_or(DEFAULT_BUFFER_SIZE);
        let discard_on_open = args.discard_on_open.unwrap_or(true);

        match self.manager.open(
            &args.port,
            baudrate,
            data_bits,
            parity,
            stop_bits,
            flow_control,
            read_timeout_ms,
            buffer_size,
            discard_on_open,
        ) {
            Ok(()) => {
                let info = self.manager.available(&args.port);
                tracing::info!(port = %args.port, baudrate, "串口已打开");
                Ok(CallToolResult::structured(json!(info)))
            }
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 运行时重配置已打开的串口（仅更新传入的参数项）。
    #[tool(
        description = "运行时重配置已打开的串口（port 必填）：baudrate / data_bits / parity / stop_bits / flow_control / read_timeout_ms，仅更新传入项。"
    )]
    async fn uart_configure(
        &self,
        Parameters(args): Parameters<ConfigureArgs>,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        let baudrate = match args.baudrate.map(manager::parse_baudrate) {
            Some(Ok(v)) => Some(v),
            Some(Err(e)) => return Err(McpError::invalid_params(e, None)),
            None => None,
        };
        let data_bits = match args.data_bits.map(manager::parse_data_bits) {
            Some(Ok(v)) => Some(v),
            Some(Err(e)) => return Err(McpError::invalid_params(e, None)),
            None => None,
        };
        let parity = match args.parity.as_deref().map(manager::parse_parity) {
            Some(Ok(v)) => Some(v),
            Some(Err(e)) => return Err(McpError::invalid_params(e, None)),
            None => None,
        };
        let stop_bits = match args.stop_bits.map(manager::parse_stop_bits) {
            Some(Ok(v)) => Some(v),
            Some(Err(e)) => return Err(McpError::invalid_params(e, None)),
            None => None,
        };
        let flow_control = match args
            .flow_control
            .as_deref()
            .map(manager::parse_flow_control)
        {
            Some(Ok(v)) => Some(v),
            Some(Err(e)) => return Err(McpError::invalid_params(e, None)),
            None => None,
        };
        match self
            .manager
            .configure(
                &args.port,
                baudrate,
                data_bits,
                parity,
                stop_bits,
                flow_control,
                args.read_timeout_ms,
            )
            .await
        {
            Ok(()) => Ok(CallToolResult::structured(json!(
                self.manager.available(&args.port)
            ))),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 向串口发送数据（只发不等回复），返回实际写入字节数。
    #[tool(
        description = "向串口发送数据并立即返回（port 必填，不等待回复）；如需发送+读取请用 uart_exchange。"
    )]
    async fn uart_write(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        let mode = args.mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_send_mode(&mode) {
            return Err(McpError::invalid_params(e, None));
        }
        let newline = args.newline.unwrap_or_else(|| "none".into()).to_ascii_lowercase();
        if let Err(e) = parse_newline(&newline) {
            return Err(McpError::invalid_params(e, None));
        }
        let data = match encode_send(&args.data, &mode) {
            Ok(d) => apply_newline(d, &newline),
            Err(e) => return Err(McpError::invalid_params(e, None)),
        };
        match self.manager.write(&args.port, &data).await {
            Ok(written) => Ok(CallToolResult::structured(json!({
                "written": written,
                "mode": mode.to_ascii_lowercase(),
                "newline": newline,
            }))),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 拉取串口上行缓冲：空闲判定（收到最后一个字节后持续 idle_ms 无新数据
    /// 且驱动侧无残留字节）、未读字节数达 max_bytes、或总等待超时 timeout_ms
    /// 三者之一满足时返回全部未读数据。
    /// 返回值含 overflow_delta/overflow_total（缓冲溢出被覆盖丢弃的字节数，>0 表示数据有缺口）。
    #[tool(
        description = "读取串口上行缓冲（port 必填；后台持续囤积，按需拉取）。返回 data、bytes、reason（idle/max_bytes/timeout）及溢出统计。"
    )]
    async fn uart_read(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        let idle_ms = args.idle_ms.unwrap_or(DEFAULT_IDLE_MS);
        let max_bytes = args.max_bytes.unwrap_or(DEFAULT_MAX_BYTES).max(1);
        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let mode = args.mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_recv_mode(&mode) {
            return Err(McpError::invalid_params(e, None));
        }
        match self
            .manager
            .read(&args.port, idle_ms, max_bytes, timeout_ms)
            .await
        {
            Ok(outcome) => {
                let (data, used_mode) = encode_recv(&outcome.data, &mode);
                Ok(CallToolResult::structured(json!({
                    "data": data,
                    "bytes": outcome.data.len(),
                    "mode": used_mode,
                    "reason": read_reason_str(outcome.reason),
                    "overflow_delta": outcome.overflow_delta,
                    "overflow_total": outcome.overflow_total,
                    "buffered_bytes": outcome.buffered,
                })))
            }
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 一步完成"发送 + 读取"：先写数据，再按 uart_read 的语义拉取回复。对大多数 AT 命令/查询场景最常用。
    #[tool(
        description = "发送数据并等待回复（port 必填；uart_write + uart_read 的组合，一步完成）。返回 written、data、reason 及溢出统计。"
    )]
    async fn uart_exchange(
        &self,
        Parameters(args): Parameters<ExchangeArgs>,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        let mode = args.mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_send_mode(&mode) {
            return Err(McpError::invalid_params(e, None));
        }
        let newline = args.newline.unwrap_or_else(|| "none".into()).to_ascii_lowercase();
        if let Err(e) = parse_newline(&newline) {
            return Err(McpError::invalid_params(e, None));
        }
        let data = match encode_send(&args.data, &mode) {
            Ok(d) => apply_newline(d, &newline),
            Err(e) => return Err(McpError::invalid_params(e, None)),
        };
        let idle_ms = args.idle_ms.unwrap_or(DEFAULT_IDLE_MS);
        let max_bytes = args.max_bytes.unwrap_or(DEFAULT_MAX_BYTES).max(1);
        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let read_mode = args.read_mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_recv_mode(&read_mode) {
            return Err(McpError::invalid_params(e, None));
        }
        match self.manager.write(&args.port, &data).await {
            Ok(written) => match self
                .manager
                .read(&args.port, idle_ms, max_bytes, timeout_ms)
                .await
            {
                Ok(outcome) => {
                    let (resp, used_mode) = encode_recv(&outcome.data, &read_mode);
                    Ok(CallToolResult::structured(json!({
                        "written": written,
                        "data": resp,
                        "bytes": outcome.data.len(),
                        "mode": used_mode,
                        "newline": newline,
                        "reason": read_reason_str(outcome.reason),
                        "overflow_delta": outcome.overflow_delta,
                        "overflow_total": outcome.overflow_total,
                        "buffered_bytes": outcome.buffered,
                    })))
                }
                Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
            },
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 等待串口输出中出现指定 pattern（内容匹配，替代 AI 侧 sleep+盲发 的时序编排）。
    /// 可选 `data` 实现"发送+等待"一步完成；命中（或超时）后返回。
    /// `consume=true`（默认）时取走并返回"截至 pattern 结尾"的内容，pattern 之后
    /// 的字节留在缓冲；`consume=false` 时纯等待、数据不消费（可用 uart_read 取走诊断）。
    #[tool(
        description = "等待串口输出中出现指定 pattern（port、pattern 必填；可选 data 实现\"发送+等待\"）。命中或超时后返回，consume=true（默认）时返回截至 pattern 结尾的内容。"
    )]
    async fn uart_expect(
        &self,
        Parameters(args): Parameters<ExpectArgs>,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        let pattern_mode = args.pattern_mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_send_mode(&pattern_mode) {
            return Err(McpError::invalid_params(e, None));
        }
        let pattern = match encode_send(&args.pattern, &pattern_mode) {
            Ok(p) => p,
            Err(e) => return Err(McpError::invalid_params(e, None)),
        };
        if pattern.is_empty() {
            return Err(McpError::invalid_params("pattern 不能为空", None));
        }
        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        if timeout_ms > manager::MAX_EXPECT_TIMEOUT_MS {
            return Err(McpError::invalid_params(
                format!(
                    "timeout_ms 超出上限 {}ms（expect 期间其它工具调用会排队）",
                    manager::MAX_EXPECT_TIMEOUT_MS
                ),
                None,
            ));
        }
        let consume = args.consume.unwrap_or(true);
        let read_mode = args.read_mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_recv_mode(&read_mode) {
            return Err(McpError::invalid_params(e, None));
        }
        let newline = args
            .newline
            .unwrap_or_else(|| "none".into())
            .to_ascii_lowercase();
        if let Err(e) = parse_newline(&newline) {
            return Err(McpError::invalid_params(e, None));
        }
        let send = match args.data {
            Some(d) => {
                let mode = args.mode.unwrap_or_else(|| "hex".into());
                if let Err(e) = parse_send_mode(&mode) {
                    return Err(McpError::invalid_params(e, None));
                }
                match encode_send(&d, &mode) {
                    Ok(bytes) => Some(apply_newline(bytes, &newline)),
                    Err(e) => return Err(McpError::invalid_params(e, None)),
                }
            }
            None => None,
        };
        match self
            .manager
            .expect(&args.port, send.as_deref(), &pattern, timeout_ms, consume)
            .await
        {
            Ok(outcome) => Ok(expect_result(outcome, &args.pattern, &read_mode, &newline)),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 等待串口输出中出现指定 pattern，命中后在同一临界区内**立即**发送 `reply`
    /// （等待→命中→发送一步原子完成，消除"expect 返回 → 再调 write"的往返延迟，
    /// 适合 bootdelay 抢窗口等时序敏感场景）。超时未命中时不发送 reply。
    #[tool(
        description = "等待串口输出中出现指定 pattern 后立即发送 reply（port、pattern、reply 必填；超时未命中不发送）。返回 matched、written、data 及溢出统计。"
    )]
    async fn uart_expect_send(
        &self,
        Parameters(args): Parameters<ExpectSendArgs>,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        let pattern_mode = args.pattern_mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_send_mode(&pattern_mode) {
            return Err(McpError::invalid_params(e, None));
        }
        let pattern = match encode_send(&args.pattern, &pattern_mode) {
            Ok(p) => p,
            Err(e) => return Err(McpError::invalid_params(e, None)),
        };
        if pattern.is_empty() {
            return Err(McpError::invalid_params("pattern 不能为空", None));
        }
        let reply_mode = args.reply_mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_send_mode(&reply_mode) {
            return Err(McpError::invalid_params(e, None));
        }
        let reply = match encode_send(&args.reply, &reply_mode) {
            Ok(r) => r,
            Err(e) => return Err(McpError::invalid_params(e, None)),
        };
        if reply.is_empty() {
            return Err(McpError::invalid_params("reply 不能为空", None));
        }
        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        if timeout_ms > manager::MAX_EXPECT_TIMEOUT_MS {
            return Err(McpError::invalid_params(
                format!(
                    "timeout_ms 超出上限 {}ms（expect 期间其它工具调用会排队）",
                    manager::MAX_EXPECT_TIMEOUT_MS
                ),
                None,
            ));
        }
        let consume = args.consume.unwrap_or(true);
        let read_mode = args.read_mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_recv_mode(&read_mode) {
            return Err(McpError::invalid_params(e, None));
        }
        match self
            .manager
            .expect_send(&args.port, &pattern, &reply, timeout_ms, consume)
            .await
        {
            Ok(outcome) => Ok(expect_result(outcome, &args.pattern, &read_mode, "none")),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 查询串口状态：是否打开、当前配置、缓冲未读字节数、累计溢出字节数、读线程错误等。
    #[tool(
        description = "查询指定串口的运行状态与缓冲统计（port 必填；open、配置、buffered_bytes、overflow_total、read_error）。"
    )]
    async fn uart_available(
        &self,
        Parameters(args): Parameters<AvailableArgs>,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        Ok(CallToolResult::structured(json!(
            self.manager.available(&args.port)
        )))
    }

    /// 清空缓冲中未读取的上行数据。
    #[tool(description = "清空指定串口环形缓冲中未读取的上行数据（port 必填），返回清掉的字节数。")]
    async fn uart_clear(
        &self,
        Parameters(args): Parameters<ClearArgs>,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        match self.manager.clear(&args.port) {
            Ok(cleared) => Ok(CallToolResult::structured(json!({ "cleared": cleared }))),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 关闭串口：停止并回收读线程，释放端口句柄。
    #[tool(description = "关闭指定串口并释放端口（port 必填；后续可重新 uart_open）。")]
    async fn uart_close(
        &self,
        Parameters(args): Parameters<CloseArgs>,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        match self.manager.close(&args.port).await {
            Ok(()) => Ok(CallToolResult::structured(json!({ "closed": true }))),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }
}

#[tool_handler]
impl ServerHandler for Ser2Mcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ser2mcp", env!("CARGO_PKG_VERSION")))
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(INSTRUCTIONS.to_string())
    }
}

impl Default for Ser2Mcp {
    fn default() -> Self {
        Self::new()
    }
}
