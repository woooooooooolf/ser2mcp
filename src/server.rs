//! MCP 工具层：工具注册 + ServerHandler 实现。
//!
//! 工具面（14 个）：
//! - `uart_list_ports`   枚举本机可用串口
//! - `uart_open`         打开串口（全量串口参数 + 内部参数：缓冲区大小等）
//! - `uart_configure`    运行时重配置（仅更新传入项）
//! - `uart_write`        只发不等
//! - `uart_read`         拉取缓冲（空闲判定/上限/超时三种返回条件）
//! - `uart_exchange`     写 + 读（同一 I/O 临界区；短命令、idle 收尾）
//! - `uart_expect`       等待匹配输出（可选"发送+等待"；内容匹配语义）
//! - `uart_expect_send`  匹配后立即发送（等待→命中→发送一步原子完成）
//! - `uart_available`    状态快照（含缓冲统计、读线程错误与发送进度）
//! - `uart_clear`        清空未读缓冲
//! - `uart_close`        关闭串口（进行中的文件发送会被中断）
//! - `uart_send_estimate` 文件发送耗时/字节数估算（无需打开串口）
//! - `uart_send_file`    文件流式发送（分片限速，服务器内部循环一次调用）
//! - `uart_send_cancel`  中止进行中的文件发送
//!
//! 多端口与透传：支持同时打开多个串口，端口名（如 `COM3` / `/dev/ttyUSB0`）即句柄，
//! 除 `uart_list_ports` / `uart_send_estimate` 外，其余工具都要求 `port` 参数；
//! 字节流原样透传，不做解析/过滤；
//! `uart_expect` / `uart_expect_send` 仅在缓冲中做条件查找（不修改数据），
//! 命中与否不影响字节流的透传语义。
//!
//! 数据表示：串口数据是二进制，而 MCP 参数/返回值是文本，因此统一用
//! hex 字符串（如 `"41 54 0D 0A"`）传递；`mode="text"` 切换 UTF-8 文本，
//! `read_mode="text-escaped"` 文本为主、非文本字节 `\xNN` 转义（不降级）；
//! 发送侧可经 `newline` 参数（none/lf/crlf）显式追加行尾。

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler, handler::server::wrapper::Parameters, model::*, tool,
    tool_handler, tool_router,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::hex;
use crate::manager::{
    self, DEFAULT_BUFFER_SIZE, DEFAULT_IDLE_MS, DEFAULT_MAX_BYTES, DEFAULT_READ_TIMEOUT_MS,
    DEFAULT_TIMEOUT_MS, MAX_BUFFER_SIZE, MAX_PATTERN_SIZE, PortConfig, ReadReason, SerialManager,
};
use crate::sendfile;

/// 对 AI 助手的核心使用约束（随 initialize 返回）；详细流程由插件 SKILL 提供。
const INSTRUCTIONS: &str = r##"ser2mcp：UART 串口 MCP 服务器，原样透传字节流，不解析设备协议。

核心流程：uart_list_ports → uart_open {port} → 交互 → uart_close {port}。
- 除 uart_list_ports 和 uart_send_estimate 外，其余工具都需要 port；重复打开前先关闭。
- 二进制使用 mode/read_mode="hex"；终端命令使用 mode="text"、显式 newline，并优先用 read_mode="text-escaped"。
- 有提示符或结束标记时用 uart_expect；终端开启回显时，data 中出现的 pattern 会先在命令回显里命中，应关闭回显或构造回显中不连续出现的输出锚点。
- 需要命中即回复时用 uart_expect_send；只有无稳定锚点的短响应才用 uart_exchange 的 idle 收尾。
- reason="idle" 只表示字节流静默，不表示命令完成。不要 sleep 盲等；一次只发送一条命令。
- 每次读取都检查 overflow_delta；大于 0 表示缓冲覆盖导致数据缺口。
- pattern 是大小写敏感的原始字节子串，不支持正则；历史未读数据会立即参与匹配。

文件发送：
- 先确认本地 path 与目标设备均在用户授权范围内；服务端可读取进程有权访问的任意普通文件，不限制目录。
- 按 uart_send_estimate → 准备对端 → uart_send_file 一次调用 → EOF/按长度结束 → 对端长度和哈希对账执行；不要循环 uart_write。
- reason 只表示服务器端结束状态；completed 不代表对端完整接收。base64 的 sent_bytes 包含编码和换行，不能与 raw_bytes 直接比较。
- send_file 的 overflow 字段是生成返回时的上行缓冲快照，0 不等于最终无溢出；返回后再调用 uart_available / uart_read 确认最新 overflow_total。
- ser2mcp 不主动发送 EOF。发送期间普通 I/O/配置/expect 会等待全局 I/O 锁；uart_available 可查进度，uart_send_cancel 或目标端口 uart_close 可中止。

详细决策与故障处理见 ser2mcp-usage SKILL；文件/固件传输见 ser2mcp-file-transfer SKILL。"##;

/// 串口工具参数：uart_open。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct OpenArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
    /// 波特率，默认 115200，范围 50..=4000000。
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
    /// 上行环形缓冲大小（字节），默认 1048576（1 MiB），范围 1..=16777216（16 MiB）；
    /// 写满覆盖最旧数据并计数溢出。
    pub buffer_size: Option<usize>,
    /// 打开时是否清空串口驱动输入缓冲中残留的旧数据，默认 true。
    pub discard_on_open: Option<bool>,
}

/// 串口工具参数：uart_configure（全部可选，仅更新传入项）。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ConfigureArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
    /// 波特率，范围 50..=4000000。
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
    /// 总等待超时（毫秒），默认 5000，上限 300000（5 分钟）。
    pub timeout_ms: Option<u64>,
    /// 返回编码：hex（默认）、text（非文本数据自动降级为 hex）或 text-escaped
    /// （文本为主，控制字节/非法 UTF-8 以 \xNN 转义，恒可读不降级）。
    pub read_mode: Option<String>,
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
    /// 总等待超时（毫秒），默认 5000，上限 300000（5 分钟）。
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
    /// 要等待出现的非空原始字节子串（hex 或 text，取决于 pattern_mode），
    /// 编码后上限 65536 字节；历史未读数据和终端输入回显都会参与匹配。
    pub pattern: String,
    /// pattern 编码：hex（默认）或 text。
    pub pattern_mode: Option<String>,
    /// 总等待超时（毫秒），默认 5000，上限 300000（5 分钟）。
    pub timeout_ms: Option<u64>,
    /// 命中后是否取走并返回"截至 pattern 结尾"的内容，默认 true；
    /// false 时纯等待（数据留在缓冲，后续可用 uart_read 取走）。
    pub consume: Option<bool>,
    /// 可选：等待前先发送的数据（"发送+等待"一步完成），编码取决于 mode。
    /// 终端开启输入回显时，不要让该数据连续包含 pattern，否则可能提前命中回显。
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
    /// 要等待出现的非空字符串（hex 或 text，取决于 pattern_mode），编码后上限 65536 字节。
    pub pattern: String,
    /// 命中后立即发送的内容（hex 或 text，取决于 reply_mode）；超时未命中时不发送。
    pub reply: String,
    /// pattern 编码：hex（默认）或 text。
    pub pattern_mode: Option<String>,
    /// reply 编码：hex（默认）或 text。
    pub reply_mode: Option<String>,
    /// 总等待超时（毫秒），默认 5000，上限 300000（5 分钟）。
    pub timeout_ms: Option<u64>,
    /// 命中后是否取走并返回"截至 pattern 结尾"的内容，默认 true。
    pub consume: Option<bool>,
    /// 返回 data 字段的编码：hex（默认）、text（非文本数据自动降级为 hex）或 text-escaped。
    pub read_mode: Option<String>,
}

/// 串口工具参数：uart_send_file（文件流式发送）。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SendFileArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
    /// 本地文件路径（服务器读取；须存在、是普通文件、可读）。
    pub path: String,
    /// 编码：text（默认，原样按字节发）/ base64（服务器跨分片连续编码，padding 仅在文件末尾，
    /// 每 76 字符自动换行并在文件末尾补换行）。
    pub mode: Option<String>,
    /// 分片大小（原始字节），默认 256，范围 1..=1048576（1 MiB）。
    /// 模型须依据对端 tty 缓冲限制与波特率选择，宁小勿大。
    pub chunk_size: Option<usize>,
    /// 片间间隔（毫秒），默认 0，上限 60000（每片写完 flush 已天然限速到波特率上限）。
    pub gap_ms: Option<u64>,
}

/// 串口工具参数：uart_send_estimate（发送耗时估算，无需打开串口）。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SendEstimateArgs {
    /// 本地文件路径（只读元数据，不发送）。
    pub path: String,
    /// 编码：text（默认）/ base64。
    pub mode: Option<String>,
    /// 分片大小（原始字节），默认 256，范围 1..=1048576（1 MiB）。
    pub chunk_size: Option<usize>,
    /// 片间间隔（毫秒），默认 0，上限 60000。
    pub gap_ms: Option<u64>,
    /// 波特率，默认 115200。
    pub baudrate: Option<u32>,
}

/// 串口工具参数：uart_send_cancel。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SendCancelArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
}

fn require_port(port: &str) -> Result<(), McpError> {
    if port.trim().is_empty() {
        Err(McpError::invalid_params("port 不能为空", None))
    } else {
        Ok(())
    }
}

fn validate_buffer_size(value: usize) -> Result<(), McpError> {
    if value == 0 || value > MAX_BUFFER_SIZE {
        return Err(McpError::invalid_params(
            format!("buffer_size must be in 1..={MAX_BUFFER_SIZE} bytes"),
            None,
        ));
    }
    Ok(())
}

fn validate_chunk_size(value: usize) -> Result<(), McpError> {
    if value == 0 || value > sendfile::MAX_CHUNK_SIZE {
        return Err(McpError::invalid_params(
            format!(
                "chunk_size must be in 1..={} bytes",
                sendfile::MAX_CHUNK_SIZE
            ),
            None,
        ));
    }
    Ok(())
}

fn validate_read_timeout(value: u64) -> Result<(), McpError> {
    if value > manager::MAX_READ_TIMEOUT_MS {
        return Err(McpError::invalid_params(
            format!(
                "timeout_ms exceeds the limit of {}ms",
                manager::MAX_READ_TIMEOUT_MS
            ),
            None,
        ));
    }
    Ok(())
}

fn validate_pattern_size(value: &[u8]) -> Result<(), McpError> {
    if value.len() > MAX_PATTERN_SIZE {
        return Err(McpError::invalid_params(
            format!("pattern exceeds the limit of {MAX_PATTERN_SIZE} bytes"),
            None,
        ));
    }
    Ok(())
}

/// 校验发送文件：path 非空、存在、是普通文件、可读；返回文件字节数。
fn send_file_meta(path: &str) -> Result<u64, String> {
    if path.trim().is_empty() {
        return Err("path 不能为空".into());
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("无法访问文件 {path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path} 不是普通文件"));
    }
    // 打开验证可读（权限问题在这里暴露）。
    std::fs::File::open(path).map_err(|e| format!("打开文件 {path} 失败: {e}"))?;
    Ok(meta.len())
}

/// 校验发送编码模式：hex 或 text（text-escaped 仅用于返回侧）。
fn parse_send_mode(s: &str) -> Result<(), String> {
    match s.to_ascii_lowercase().as_str() {
        "hex" | "text" => Ok(()),
        "text-escaped" => {
            Err("text-escaped 仅用于返回编码（read_mode），发送编码仅支持 hex 或 text".into())
        }
        other => Err(format!("mode 仅支持 hex 或 text，收到 {other:?}")),
    }
}

/// 校验返回编码模式：hex、text 或 text-escaped。
fn parse_recv_mode(s: &str) -> Result<(), String> {
    match s.to_ascii_lowercase().as_str() {
        "hex" | "text" | "text-escaped" => Ok(()),
        other => Err(format!(
            "read_mode 仅支持 hex、text 或 text-escaped，收到 {other:?}"
        )),
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
        validate_buffer_size(buffer_size)?;
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
        let newline = args
            .newline
            .unwrap_or_else(|| "none".into())
            .to_ascii_lowercase();
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

    /// 估算 uart_send_file 的发送字节数与耗时（8N1：每字节 10 bit，未计片间
    /// flush 开销，实际耗时通常略高于估算值）。无需打开串口，只读文件元数据。
    #[tool(
        description = "估算 uart_send_file 的传输字节数与耗时（path 必填；无需打开串口，baudrate 默认 115200）。典型流程：先 uart_send_estimate，再 uart_send_file。"
    )]
    async fn uart_send_estimate(
        &self,
        Parameters(args): Parameters<SendEstimateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mode = match sendfile::SendMode::parse(args.mode.as_deref().unwrap_or("text")) {
            Ok(m) => m,
            Err(e) => return Err(McpError::invalid_params(e, None)),
        };
        let chunk_size = match args.chunk_size {
            None => sendfile::DEFAULT_CHUNK_SIZE,
            Some(0) => return Err(McpError::invalid_params("chunk_size 必须 ≥ 1", None)),
            Some(v) => {
                validate_chunk_size(v)?;
                v
            }
        };
        let gap_ms = args.gap_ms.unwrap_or(0);
        if gap_ms > manager::MAX_SEND_GAP_MS {
            return Err(McpError::invalid_params(
                format!(
                    "gap_ms 超出上限 {}ms（发送期间其它需要 I/O 锁的工具调用会排队）",
                    manager::MAX_SEND_GAP_MS
                ),
                None,
            ));
        }
        let baudrate = args.baudrate.unwrap_or(manager::DEFAULT_BAUDRATE);
        if baudrate == 0 {
            return Err(McpError::invalid_params("baudrate 必须 ≥ 1", None));
        }
        let size = match send_file_meta(&args.path) {
            Ok(s) => s,
            Err(e) => return Err(McpError::invalid_params(e, None)),
        };
        Ok(CallToolResult::structured(json!(sendfile::estimate(
            mode, size, chunk_size, gap_ms, baudrate
        ))))
    }

    /// 把本地文件分片限速发送到串口：服务器内部循环一次调用，替代模型逐块
    /// 调 uart_write（省协议与 token 成本）。只承诺"把文件字节发出去"：不解析
    /// 数据格式、不主动发 EOF（对端需要 EOF 时模型另用 uart_write 补 \x04）。
    /// 发送期间 `uart_available` 可查进度，`uart_send_cancel` / `uart_close`
    /// / 客户端取消通知（notifications/cancelled）均可中止。
    #[tool(
        description = "文件流式发送（port、path 必填）：读取 ser2mcp 进程有权访问的本地普通文件并分片发送到串口；服务端不限制目录，调用前须确认路径与目标设备均在用户授权范围内。mode=text（默认，原样按字节发）/ base64（跨分片连续编码，padding 仅在文件末尾，每 76 字符自动换行、末尾补换行）；chunk_size 默认 256、上限 1 MiB；gap_ms 默认 0、上限 60000。只发字节、不解析、不主动发 EOF。返回 reason/raw_bytes/sent_bytes/chunks/elapsed_ms/overflow/device_error 统计；reason 只表示服务器端结束状态，端到端完整性须用对端字节数与解码后哈希确认。overflow 是生成返回时的上行缓冲快照，返回后须用 uart_available / uart_read 再确认最新 overflow_total。发送期间可 uart_available 查进度、uart_send_cancel 或 uart_close 中止。"
    )]
    async fn uart_send_file(
        &self,
        Parameters(args): Parameters<SendFileArgs>,
        ct: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        let mode = match sendfile::SendMode::parse(args.mode.as_deref().unwrap_or("text")) {
            Ok(m) => m,
            Err(e) => return Err(McpError::invalid_params(e, None)),
        };
        let chunk_size = match args.chunk_size {
            None => sendfile::DEFAULT_CHUNK_SIZE,
            Some(0) => return Err(McpError::invalid_params("chunk_size 必须 ≥ 1", None)),
            Some(v) => {
                validate_chunk_size(v)?;
                v
            }
        };
        let gap_ms = args.gap_ms.unwrap_or(0);
        if gap_ms > manager::MAX_SEND_GAP_MS {
            return Err(McpError::invalid_params(
                format!(
                    "gap_ms 超出上限 {}ms（发送期间其它需要 I/O 锁的工具调用会排队）",
                    manager::MAX_SEND_GAP_MS
                ),
                None,
            ));
        }
        let total = match send_file_meta(&args.path) {
            Ok(s) => s,
            Err(e) => return Err(McpError::invalid_params(e, None)),
        };
        let file = match std::fs::File::open(&args.path) {
            Ok(f) => f,
            Err(e) => return Err(McpError::invalid_params(format!("打开文件失败: {e}"), None)),
        };
        let chunks = sendfile::ChunkIter::new(file, mode, chunk_size);
        match self
            .manager
            .send_file(&args.port, chunks, total, gap_ms, Some(&ct))
            .await
        {
            Ok(outcome) => Ok(CallToolResult::structured(json!({
                "reason": outcome.reason,
                "raw_bytes": outcome.raw_bytes,
                "sent_bytes": outcome.sent_bytes,
                "chunks": outcome.chunks,
                "elapsed_ms": outcome.elapsed_ms,
                "overflow_delta": outcome.overflow_delta,
                "overflow_total": outcome.overflow_total,
                "device_error": outcome.device_error,
                "mode": mode.as_str(),
                "chunk_size": chunk_size,
                "gap_ms": gap_ms,
            }))),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 中止当前 uart_send_file 传输（无传输时为 no-op）。发送循环在下一个
    /// 检查点退出，最坏多写一片；返回调用前的发送状态快照供判断。
    #[tool(
        description = "中止当前 uart_send_file 文件发送（port 必填；无传输时为 no-op）。返回调用前的发送状态快照。"
    )]
    async fn uart_send_cancel(
        &self,
        Parameters(args): Parameters<SendCancelArgs>,
    ) -> Result<CallToolResult, McpError> {
        require_port(&args.port)?;
        match self.manager.cancel_send(&args.port) {
            Ok(snap) => Ok(CallToolResult::structured(json!({
                "cancelled": snap.active,
                "send": snap,
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
        validate_read_timeout(timeout_ms)?;
        let mode = args.read_mode.unwrap_or_else(|| "hex".into());
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

    /// 一步完成"发送 + 读取"：先写数据，再按 uart_read 的语义拉取回复；写入与读取
    /// 持有同一全局 I/O 临界区，不会被其它工具调用插入。
    /// 适合短命令/无锚点场景（如 AT 查询）；长命令（存在中间静默期）改用 uart_expect。
    #[tool(
        description = "发送数据并等待回复（port 必填；写入与读取在同一 I/O 临界区完成，不会被其它工具调用插入）。适合短命令、idle 收尾；返回 written、data、reason 及溢出统计。"
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
        let newline = args
            .newline
            .unwrap_or_else(|| "none".into())
            .to_ascii_lowercase();
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
        validate_read_timeout(timeout_ms)?;
        let read_mode = args.read_mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_recv_mode(&read_mode) {
            return Err(McpError::invalid_params(e, None));
        }
        match self
            .manager
            .exchange(&args.port, &data, idle_ms, max_bytes, timeout_ms)
            .await
        {
            Ok((written, outcome)) => {
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
        }
    }

    /// 等待串口输出中出现指定 pattern（内容匹配，替代 AI 侧 sleep+盲发 的时序编排）。
    /// 可选 `data` 实现"发送+等待"一步完成；命中（或超时）后返回。
    /// 若终端开启输入回显且 `data` 本身含 pattern，命令回显可先于真实输出命中；
    /// 调用方应关闭回显，或构造在回显中不连续出现、只在实际输出中出现的锚点。
    /// `consume=true`（默认）时取走并返回"截至 pattern 结尾"的内容，pattern 之后
    /// 的字节留在缓冲；`consume=false` 时纯等待、数据不消费（可用 uart_read 取走诊断）。
    #[tool(
        description = "等待串口输出中出现指定 pattern（port、pattern 必填；可选 data 实现\"发送+等待\"）。命中或超时后返回，consume=true（默认）时返回截至 pattern 结尾的内容。pattern 是原始字节匹配；若终端开启输入回显且 data 含 pattern，命令回显会造成提前命中，应关闭回显或使用在回显中不连续出现的输出锚点。"
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
        validate_pattern_size(&pattern)?;
        if pattern.is_empty() {
            return Err(McpError::invalid_params("pattern 不能为空", None));
        }
        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        if timeout_ms > manager::MAX_EXPECT_TIMEOUT_MS {
            return Err(McpError::invalid_params(
                format!(
                    "timeout_ms 超出上限 {}ms（expect 期间其它需要 I/O 锁的工具调用会排队）",
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
        validate_pattern_size(&pattern)?;
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
                    "timeout_ms 超出上限 {}ms（expect 期间其它需要 I/O 锁的工具调用会排队）",
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

    /// 查询串口状态：是否打开、当前配置、缓冲未读字节数、累计溢出字节数、
    /// 读线程错误、文件发送进度（send 字段）等。
    #[tool(
        description = "查询指定串口的运行状态与缓冲统计（port 必填；open、配置、buffered_bytes、overflow_total、read_error、send 发送进度）。"
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

    /// 关闭串口：若目标端口正在发送文件，先请求取消并等待发送退出（最长 30 秒），
    /// 再停止并回收读线程、释放端口句柄。
    #[tool(
        description = "关闭指定串口并释放端口（port 必填；后续可重新 uart_open）。目标端口正在 uart_send_file 时会先请求取消并等待其退出（最长 30 秒），再关闭端口。"
    )]
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
