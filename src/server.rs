//! MCP 工具层：工具注册 + ServerHandler 实现。
//!
//! 工具面（9 个）：
//! - `uart_list_ports`   枚举本机可用串口
//! - `uart_open`         打开串口（全量串口参数 + 内部参数：缓冲区大小等）
//! - `uart_configure`    运行时重配置（仅更新传入项）
//! - `uart_write`        只发不等
//! - `uart_read`         拉取缓冲（空闲判定/上限/超时三种返回条件）
//! - `uart_exchange`     写 + 读（对 LLM 最常用的一步操作）
//! - `uart_available`    状态快照（含缓冲统计与读线程错误）
//! - `uart_clear`        清空未读缓冲
//! - `uart_close`        关闭串口
//!
//! 数据表示：串口数据是二进制，而 MCP 参数/返回值是文本，因此统一用
//! hex 字符串（如 `"41 54 0D 0A"`）传递，`mode` 参数可切换为文本。

use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::wrapper::Parameters,
    model::*,
    tool, tool_handler, tool_router,
};
use serde_json::json;

use crate::hex;
use crate::manager::{
    self, PortConfig, ReadReason, SerialManager, DEFAULT_BUFFER_SIZE, DEFAULT_IDLE_MS,
    DEFAULT_MAX_BYTES, DEFAULT_READ_TIMEOUT_MS, DEFAULT_TIMEOUT_MS,
};

/// 对 AI 助手的使用指引（随 initialize 返回）。
const INSTRUCTIONS: &str = r#"ser2mcp：UART 串口 MCP 服务器。

典型流程：uart_list_ports → uart_open → uart_exchange（写+读一步完成）→ uart_close。

数据表示：
- 二进制一律用 hex 字符串传递（如 "41 54 0D 0A"），每字节两个大写十六进制字符、空格分隔；
  也接受连续串（"41540D0A"）、逗号/分号/0x 前缀等宽松形式。
- 文本模式（mode="text"）下直接传 UTF-8 字符串；返回时若数据非合法文本则自动降级为 hex。

读取语义（重要）：
- 串口上行数据由后台读线程持续囤积在有界环形缓冲中（写满覆盖最旧并计数溢出），
  工具按需拉取，而非设备主动推送。
- uart_read / uart_exchange 在三种条件下返回：① 出现新数据后持续 idle_ms 无新字节
  （视为一次响应结束，默认 300ms）；② 未读字节数达到 max_bytes（默认 64KiB）；
  ③ 总等待超过 timeout_ms（默认 5000ms）。
- 返回值中的 overflow_delta / overflow_total 表示缓冲溢出被覆盖丢弃的字节数，
  大于 0 时说明数据有缺口，应调大 buffer_size 或减小拉取间隔。

回环自测：TX-RX 短接时 uart_exchange 发送的内容应原样返回。"#;

/// 串口工具参数：uart_open。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct OpenArgs {
    /// 串口名，如 "COM3"（Windows）或 "/dev/ttyUSB0"（Linux/macOS）。
    pub port: String,
    /// 波特率，默认 115200。
    pub baudrate: Option<u32>,
    /// 数据位（5-8），默认 8。
    pub data_bits: Option<u8>,
    /// 校验位 none/even/odd/mark/space，默认 none。
    pub parity: Option<String>,
    /// 停止位 1 或 2，默认 1。
    pub stop_bits: Option<u8>,
    /// 流控 none/software/hardware，默认 none。
    pub flow_control: Option<String>,
    /// 读线程的串口读超时（毫秒），默认 100；也决定关闭时的最长等待。
    pub read_timeout_ms: Option<u64>,
    /// 上行环形缓冲大小（字节），默认 1048576（1 MiB）；写满覆盖最旧数据并计数溢出。
    pub buffer_size: Option<usize>,
    /// 打开时是否清空串口驱动输入缓冲中残留的旧数据，默认 true。
    pub discard_on_open: Option<bool>,
}

/// 串口工具参数：uart_configure（全部可选，仅更新传入项）。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ConfigureArgs {
    /// 波特率。
    pub baudrate: Option<u32>,
    /// 数据位（5-8）。
    pub data_bits: Option<u8>,
    /// 校验位 none/even/odd/mark/space。
    pub parity: Option<String>,
    /// 停止位 1 或 2。
    pub stop_bits: Option<u8>,
    /// 流控 none/software/hardware。
    pub flow_control: Option<String>,
    /// 读超时（毫秒）。
    pub read_timeout_ms: Option<u64>,
}

/// 串口工具参数：uart_write。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WriteArgs {
    /// 要发送的数据（hex 字符串或文本，取决于 mode）。
    pub data: String,
    /// 数据编码：hex（默认）或 text。
    pub mode: Option<String>,
}

/// 串口工具参数：uart_read。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadArgs {
    /// 空闲判定阈值（毫秒）：出现新数据后持续该时长无新字节即返回，默认 300。
    pub idle_ms: Option<u64>,
    /// 未读字节数达到该值立即返回（防堆积），默认 65536。
    pub max_bytes: Option<usize>,
    /// 总等待超时（毫秒），默认 5000。
    pub timeout_ms: Option<u64>,
    /// 返回编码：hex（默认）或 text（非文本数据自动降级为 hex）。
    pub mode: Option<String>,
}

/// 串口工具参数：uart_exchange。
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExchangeArgs {
    /// 要发送的数据（hex 字符串或文本，取决于 mode）。
    pub data: String,
    /// 发送编码：hex（默认）或 text。
    pub mode: Option<String>,
    /// 空闲判定阈值（毫秒），默认 300。
    pub idle_ms: Option<u64>,
    /// 未读字节数达到该值立即返回，默认 65536。
    pub max_bytes: Option<usize>,
    /// 总等待超时（毫秒），默认 5000。
    pub timeout_ms: Option<u64>,
    /// 返回编码：hex（默认）或 text。
    pub read_mode: Option<String>,
}

fn parse_mode(s: &str) -> Result<(), String> {
    match s.to_ascii_lowercase().as_str() {
        "hex" | "text" => Ok(()),
        other => Err(format!("mode 仅支持 hex 或 text，收到 {other:?}")),
    }
}

/// 按 mode 编码发送数据。
fn encode_send(data: &str, mode: &str) -> Result<Vec<u8>, String> {
    match mode {
        "hex" => hex::decode(data),
        "text" => Ok(data.as_bytes().to_vec()),
        _ => unreachable!("mode 已校验"),
    }
}

/// 按 mode 编码返回数据；text 模式下非法文本自动降级为 hex。
fn encode_recv(bytes: &[u8], mode: &str) -> (String, String) {
    match mode {
        "hex" => (hex::encode(bytes), "hex".to_string()),
        _ => {
            if hex::is_text(bytes) {
                (String::from_utf8_lossy(bytes).into_owned(), "text".to_string())
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

/// MCP 服务器主体。
#[derive(Clone)]
pub struct Ser2Mcp {
    manager: Arc<SerialManager>,
}

#[tool_router]
impl Ser2Mcp {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(SerialManager::new()),
        }
    }

    /// 枚举本机当前可用的串口。
    #[tool(description = "枚举本机当前可用的串口（名称、类型、USB 描述）。串口被占用时可能不出现。")]
    async fn uart_list_ports(&self) -> Result<CallToolResult, McpError> {
        match self.manager.list_ports() {
            Ok(ports) => Ok(CallToolResult::structured(json!({ "ports": ports }))),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 打开串口并启动后台读线程（持续把上行数据囤积进环形缓冲）。
    /// 参数：port 必填；baudrate=115200、data_bits=8、parity=none、stop_bits=1、
    /// flow_control=none、read_timeout_ms=100、buffer_size=1048576（覆盖最旧+溢出计数）、
    /// discard_on_open=true。若已有打开的串口会先关闭再打开。
    #[tool(description = "打开串口并启动后台读线程。返回当前配置。")]
    async fn uart_open(&self, Parameters(args): Parameters<OpenArgs>) -> Result<CallToolResult, McpError> {
        if args.port.trim().is_empty() {
            return Err(McpError::invalid_params("port 不能为空", None));
        }
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
        let flow_control = match args.flow_control.as_deref().map(manager::parse_flow_control) {
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
                let info = self.manager.available();
                tracing::info!(port = %args.port, baudrate, "串口已打开");
                Ok(CallToolResult::structured(json!(info)))
            }
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 运行时重配置已打开的串口（仅更新传入的参数项）。
    #[tool(description = "运行时重配置已打开的串口：baudrate / data_bits / parity / stop_bits / flow_control / read_timeout_ms，仅更新传入项。")]
    async fn uart_configure(&self, Parameters(args): Parameters<ConfigureArgs>) -> Result<CallToolResult, McpError> {
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
        let flow_control = match args.flow_control.as_deref().map(manager::parse_flow_control) {
            Some(Ok(v)) => Some(v),
            Some(Err(e)) => return Err(McpError::invalid_params(e, None)),
            None => None,
        };
        match self
            .manager
            .configure(baudrate, data_bits, parity, stop_bits, flow_control, args.read_timeout_ms)
            .await
        {
            Ok(()) => Ok(CallToolResult::structured(json!(self.manager.available()))),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 向串口发送数据（只发不等回复），返回实际写入字节数。
    #[tool(description = "向串口发送数据并立即返回（不等待回复）；如需发送+读取请用 uart_exchange。")]
    async fn uart_write(&self, Parameters(args): Parameters<WriteArgs>) -> Result<CallToolResult, McpError> {
        let mode = args.mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_mode(&mode) {
            return Err(McpError::invalid_params(e, None));
        }
        let data = match encode_send(&args.data, &mode) {
            Ok(d) => d,
            Err(e) => return Err(McpError::invalid_params(e, None)),
        };
        match self.manager.write(&data).await {
            Ok(written) => Ok(CallToolResult::structured(json!({
                "written": written,
                "mode": mode,
            }))),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }

    /// 拉取串口上行缓冲：出现新数据后持续 idle_ms 无新字节（默认 300ms，视为一次响应结束）、
    /// 未读字节数达 max_bytes、或总等待超时 timeout_ms（默认 5000ms）时返回全部未读数据。
    /// 返回值含 overflow_delta/overflow_total（缓冲溢出被覆盖丢弃的字节数，>0 表示数据有缺口）。
    #[tool(description = "读取串口上行缓冲（后台持续囤积，按需拉取）。返回 data、bytes、reason（idle/max_bytes/timeout）及溢出统计。")]
    async fn uart_read(&self, Parameters(args): Parameters<ReadArgs>) -> Result<CallToolResult, McpError> {
        let idle_ms = args.idle_ms.unwrap_or(DEFAULT_IDLE_MS);
        let max_bytes = args.max_bytes.unwrap_or(DEFAULT_MAX_BYTES).max(1);
        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let mode = args.mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_mode(&mode) {
            return Err(McpError::invalid_params(e, None));
        }
        match self.manager.read(idle_ms, max_bytes, timeout_ms).await {
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
    #[tool(description = "发送数据并等待回复（uart_write + uart_read 的组合，一步完成）。返回 written、data、reason 及溢出统计。")]
    async fn uart_exchange(&self, Parameters(args): Parameters<ExchangeArgs>) -> Result<CallToolResult, McpError> {
        let mode = args.mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_mode(&mode) {
            return Err(McpError::invalid_params(e, None));
        }
        let data = match encode_send(&args.data, &mode) {
            Ok(d) => d,
            Err(e) => return Err(McpError::invalid_params(e, None)),
        };
        let idle_ms = args.idle_ms.unwrap_or(DEFAULT_IDLE_MS);
        let max_bytes = args.max_bytes.unwrap_or(DEFAULT_MAX_BYTES).max(1);
        let timeout_ms = args.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let read_mode = args.read_mode.unwrap_or_else(|| "hex".into());
        if let Err(e) = parse_mode(&read_mode) {
            return Err(McpError::invalid_params(e, None));
        }
        match self.manager.write(&data).await {
            Ok(written) => match self.manager.read(idle_ms, max_bytes, timeout_ms).await {
                Ok(outcome) => {
                    let (resp, used_mode) = encode_recv(&outcome.data, &read_mode);
                    Ok(CallToolResult::structured(json!({
                        "written": written,
                        "data": resp,
                        "bytes": outcome.data.len(),
                        "mode": used_mode,
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

    /// 查询串口状态：是否打开、当前配置、缓冲未读字节数、累计溢出字节数、读线程错误等。
    #[tool(description = "查询串口运行状态与缓冲统计（open、配置、buffered_bytes、overflow_total、read_error）。")]
    async fn uart_available(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::structured(json!(self.manager.available())))
    }

    /// 清空缓冲中未读取的上行数据。
    #[tool(description = "清空环形缓冲中未读取的上行数据，返回清掉的字节数。")]
    async fn uart_clear(&self) -> Result<CallToolResult, McpError> {
        let cleared = self.manager.clear();
        Ok(CallToolResult::structured(json!({ "cleared": cleared })))
    }

    /// 关闭串口：停止并回收后台读线程，释放端口句柄。
    #[tool(description = "关闭串口并释放端口（后续可重新 uart_open）。")]
    async fn uart_close(&self) -> Result<CallToolResult, McpError> {
        match self.manager.close().await {
            Ok(()) => Ok(CallToolResult::structured(json!({ "closed": true }))),
            Err(e) => Ok(CallToolResult::structured_error(json!({ "error": e }))),
        }
    }
}

#[tool_handler]
impl ServerHandler for Ser2Mcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(INSTRUCTIONS.to_string())
    }
}
