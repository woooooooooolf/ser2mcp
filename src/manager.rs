//! 串口管理器：打开/配置/事件驱动读线程/写/读取/期待匹配/文件发送（uart_send_file 系列）。
//! 支持同时打开多个串口，以端口名为句柄；工具调用全局串行化（AI 回合制调用天然串行）。
//!
//! 架构：
//! ```text
//! 上行：串口 ──► 事件驱动读线程(生产者) ──► RingBuf(有界环形缓冲) ──► uart_read/uart_exchange/uart_expect(消费者)
//! 下行：uart_write / uart_send_file ──► 写句柄（io_lock 临界区内循环写 + flush）──► 串口
//! ```
//! - 读线程只做"读串口 → 写缓冲"，永不阻塞在向 host 发送上；
//! - 缓冲写满后覆盖最旧数据并累计溢出计数，数据缺口可被上层检测；
//! - 写/配置/期待/文件发送经 `io_lock` 串行化；read/available/clear 不持有锁，直接操作缓冲；
//! - 文件发送每片检查点检测：取消标志（uart_send_cancel / uart_close）、客户端取消令牌、
//!   端口是否仍打开、读线程致命错误（设备物理断开等，返回 reason="device_error"）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};
use tokio_util::sync::CancellationToken;

pub use crate::ring::MAX_BUFFER_SIZE;
use crate::ring::RingBuf;

/// 默认波特率（115200）。
pub const DEFAULT_BAUDRATE: u32 = 115200;
/// 默认串口读超时（毫秒）。
///
/// 事件驱动/非阻塞读线程（见 `reader` 模块）下，该值只是 `read()` 调用的安全上限
/// （检测异常阻塞/超时），**不再决定读写延迟**；延迟由事件等待（Unix `poll` /
/// Windows 1ms 轮询）与 `idle_ms` 决定。默认取 500ms，可容纳板端命令执行时间
/// 较长的情形；如遇异常仍可通过 `uart_open` / `uart_configure` 的
/// `read_timeout_ms` 调整。
pub const DEFAULT_READ_TIMEOUT_MS: u64 = 500;
/// 默认上行环形缓冲大小（1 MiB），写满覆盖最旧数据并计数溢出。
pub const DEFAULT_BUFFER_SIZE: usize = 1024 * 1024; // 1 MiB
/// 默认空闲判定阈值（毫秒）：收到最后一个字节后持续该时长无新数据
/// 且驱动侧无残留字节，视为一次响应结束。
pub const DEFAULT_IDLE_MS: u64 = 300;
/// 默认单次拉取触发上限（64 KiB）：未读字节数达到该值立即返回。
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024;
/// 默认总等待超时（毫秒）。
pub const DEFAULT_TIMEOUT_MS: u64 = 5000;
/// 读取/交换工具允许的最大总等待时间（毫秒，5 分钟）。
pub const MAX_READ_TIMEOUT_MS: u64 = 300_000;
/// `uart_expect` / `uart_expect_send` 的 `timeout_ms` 上限（毫秒，5 分钟）。
///
/// expect 持有 `io_lock` 直到超时返回，期间其它工具调用全部排队；
/// 上限防止 LLM 传入任意大值导致工具面长时间不可用。
pub const MAX_EXPECT_TIMEOUT_MS: u64 = 300_000;
/// `uart_send_file` 的 `gap_ms` 上限（毫秒，1 分钟）。
///
/// 发送期间持有 `io_lock`，过大的片间间隔会让其它工具调用长时间排队；
/// 上限防止 LLM 传入任意大值导致工具面不可用。
pub const MAX_SEND_GAP_MS: u64 = 60_000;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// 规范化后的串口运行时配置。
#[derive(Debug, Clone)]
pub struct PortConfig {
    /// 波特率（bps）。
    pub baudrate: u32,
    /// 数据位（5-8）。
    pub data_bits: DataBits,
    /// 校验位。
    pub parity: Parity,
    /// 停止位（1 或 2）。
    pub stop_bits: StopBits,
    /// 流控方式。
    pub flow_control: FlowControl,
    /// 读线程 `read()` 的安全上限（毫秒），不影响读写延迟。
    pub read_timeout_ms: u64,
    /// 上行环形缓冲大小（字节）。
    pub buffer_size: usize,
}

impl Default for PortConfig {
    fn default() -> Self {
        Self {
            baudrate: DEFAULT_BAUDRATE,
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow_control: FlowControl::None,
            read_timeout_ms: DEFAULT_READ_TIMEOUT_MS,
            buffer_size: DEFAULT_BUFFER_SIZE,
        }
    }
}

/// 解析校验：数据位（5-8）。
pub fn parse_data_bits(v: u8) -> Result<DataBits, String> {
    match v {
        5 => Ok(DataBits::Five),
        6 => Ok(DataBits::Six),
        7 => Ok(DataBits::Seven),
        8 => Ok(DataBits::Eight),
        _ => Err(format!("data_bits 仅支持 5-8，收到 {v}")),
    }
}

/// 解析校验：停止位（1 或 2）。
pub fn parse_stop_bits(v: u8) -> Result<StopBits, String> {
    match v {
        1 => Ok(StopBits::One),
        2 => Ok(StopBits::Two),
        _ => Err(format!("stop_bits 仅支持 1 或 2，收到 {v}")),
    }
}

/// 解析校验：校验位（none/even/odd）。
pub fn parse_parity(s: &str) -> Result<Parity, String> {
    match s.to_ascii_lowercase().as_str() {
        "none" => Ok(Parity::None),
        "even" => Ok(Parity::Even),
        "odd" => Ok(Parity::Odd),
        _ => Err(format!("parity 仅支持 none/even/odd，收到 {s:?}")),
    }
}

/// 解析校验：流控（none/software/hardware）。
pub fn parse_flow_control(s: &str) -> Result<FlowControl, String> {
    match s.to_ascii_lowercase().as_str() {
        "none" => Ok(FlowControl::None),
        "software" | "xon_xoff" | "xon/xoff" => Ok(FlowControl::Software),
        "hardware" | "rts_cts" | "rts/cts" => Ok(FlowControl::Hardware),
        _ => Err(format!(
            "flow_control 仅支持 none/software/hardware，收到 {s:?}"
        )),
    }
}

/// 解析校验：波特率（合理范围 50-4,000,000）。
pub fn parse_baudrate(v: u32) -> Result<u32, String> {
    if !(50..=4_000_000).contains(&v) {
        return Err(format!("baudrate 超出合理范围 (50-4,000,000)，收到 {v}"));
    }
    Ok(v)
}

/// 串口信息（`uart_list_ports` 返回值）。
#[derive(Debug, serde::Serialize)]
pub struct PortInfo {
    /// 串口名，如 `COM3`（Windows）或 `/dev/ttyUSB0`（Linux/macOS）。
    pub name: String,
    /// 端口类型：`usb` / `bluetooth` / `pci` / `unknown`。
    pub port_type: String,
    /// 人类可读描述（USB 设备为 vid/pid/序列号/产品名）。
    pub description: String,
}

/// 读取结果（`uart_read` / `uart_exchange` 返回值）。
#[derive(Debug)]
pub struct ReadOutcome {
    /// 本次取走的全部未读字节。
    pub data: Vec<u8>,
    /// 返回原因（idle / max_bytes / timeout）。
    pub reason: ReadReason,
    /// 自上次读取以来因缓冲溢出被覆盖丢弃的字节数（>0 表示数据有缺口）。
    pub overflow_delta: u64,
    /// 自打开以来累计的溢出字节数。
    pub overflow_total: u64,
    /// 取走后缓冲中剩余的未读字节数。
    pub buffered: usize,
}

/// 读取返回原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadReason {
    /// 空闲判定：出现新数据且持续 idle_ms 无新字节（视为一次响应结束）。
    Idle,
    /// 未读字节数达到 max_bytes 上限（防缓冲堆积）。
    MaxBytes,
    /// 达到 timeout_ms 总超时（无数据或持续有数据但未空闲）。
    Timeout,
}

/// 期待匹配结果（`uart_expect` / `uart_expect_send` 返回值）。
#[derive(Debug)]
pub struct ExpectOutcome {
    /// 是否在超时前匹配到 pattern。
    pub matched: bool,
    /// `consume=true` 且命中时取走的"截至 pattern 结尾"的内容；否则为空。
    pub data: Vec<u8>,
    /// 本次调用实际写入的字节数（`uart_expect` 传 `data` 时 / `uart_expect_send` 的
    /// `reply`；超时未命中时不发送 reply，该字段为发送侧的字节数）。
    pub written: usize,
    /// 返回原因（matched / timeout）。
    pub reason: ExpectReason,
    /// 自上次消费以来因缓冲溢出被覆盖丢弃的字节数。`consume=false` 时不消费数据，
    /// 不改变消费状态，该字段恒为 0（后续 `uart_read` 的增量语义不受影响）。
    pub overflow_delta: u64,
    /// 自打开以来累计的溢出字节数。
    pub overflow_total: u64,
    /// 调用结束后缓冲中剩余的未读字节数。
    pub buffered: usize,
}

/// 期待匹配返回原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectReason {
    /// 在超时前匹配到 pattern。
    Matched,
    /// 达到 timeout_ms 总超时仍未匹配（数据不消费，留在缓冲供诊断）。
    Timeout,
}

/// 运行时状态快照（`uart_available` 返回值）。
#[derive(Debug, serde::Serialize)]
pub struct AvailableInfo {
    /// 串口是否已打开。
    pub open: bool,
    /// 已打开的串口名（未打开时为 `None`）。
    pub port: Option<String>,
    /// 当前波特率。
    pub baudrate: Option<u32>,
    /// 当前数据位（5-8）。
    pub data_bits: Option<u8>,
    /// 当前校验位（none/even/odd）。
    pub parity: Option<String>,
    /// 当前停止位（1 或 2）。
    pub stop_bits: Option<u8>,
    /// 当前流控（none/software/hardware）。
    pub flow_control: Option<String>,
    /// 当前读超时（毫秒）。
    pub read_timeout_ms: Option<u64>,
    /// 环形缓冲容量（字节）。
    pub buffer_size: Option<usize>,
    /// 缓冲中未读字节数。
    pub buffered_bytes: usize,
    /// 累计溢出字节数（缓冲写满被覆盖丢弃）。
    pub overflow_total: u64,
    /// 读线程的致命错误（端口被拔等），正常时为 `None`。
    pub read_error: Option<String>,
    /// 文件发送状态（`uart_send_file` 进行中/最近一次结果）。
    pub send: SendProgress,
}

/// 文件发送的进度快照（`uart_available` 的 `send` 字段）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SendProgress {
    /// 是否正在发送文件。
    pub active: bool,
    /// 最近一次发送的结束原因：completed / cancelled / error；无发送记录时为 `None`。
    pub last_reason: Option<String>,
    /// 已写入串口的字节数（base64 模式含换行）。
    pub sent_bytes: u64,
    /// 本次发送的原始文件字节数（base64 模式为编码前字节数）。
    pub total_bytes: u64,
    /// 已完成的发送片数。
    pub chunks: u64,
}

/// 发送会话的共享状态：进度快照 + 取消标志 + 完成通知（供 `uart_close` 中断）。
/// 每个活动端口一个；随端口生命周期存在，`uart_open` 时新建。
struct SendState {
    progress: Mutex<SendProgress>,
    cancel: AtomicBool,
    done: tokio::sync::Notify,
}

impl SendState {
    fn new() -> Self {
        Self {
            progress: Mutex::new(SendProgress::default()),
            cancel: AtomicBool::new(false),
            done: tokio::sync::Notify::new(),
        }
    }

    fn snapshot(&self) -> SendProgress {
        self.progress.lock().unwrap().clone()
    }

    fn is_active(&self) -> bool {
        self.progress.lock().unwrap().active
    }

    /// 请求中止当前发送（发送循环在下一个检查点退出）。幂等。
    fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// 发送循环进入：重置取消标志（上一会话的 cancel 不影响新会话）并标记进行中。
    fn begin(&self, total_bytes: u64) {
        self.cancel.store(false, Ordering::SeqCst);
        let mut p = self.progress.lock().unwrap();
        p.active = true;
        p.last_reason = None;
        p.sent_bytes = 0;
        p.total_bytes = total_bytes;
        p.chunks = 0;
    }

    /// 发送循环退出：记录结束原因并唤醒等待者（`uart_close` 中断路径）。
    fn finish(&self, reason: &str, sent_bytes: u64, chunks: u64) {
        {
            let mut p = self.progress.lock().unwrap();
            p.active = false;
            p.last_reason = Some(reason.to_string());
            p.sent_bytes = sent_bytes;
            p.chunks = chunks;
        }
        self.done.notify_one();
    }

    fn update(&self, sent_bytes: u64, chunks: u64) {
        let mut p = self.progress.lock().unwrap();
        p.sent_bytes = sent_bytes;
        p.chunks = chunks;
    }

    /// 等待当前发送退出。仅应在已 `cancel()` 且 `is_active()` 为 true 时调用；
    /// tokio Notify 会保留已发出的通知（permit），不会错过退出瞬间。
    async fn wait_done(&self) {
        self.done.notified().await;
    }
}

/// `uart_send_file` 的返回统计。
#[derive(Debug, serde::Serialize)]
pub struct SendFileOutcome {
    /// 结束原因：completed（全部发完）/ cancelled（被 `uart_send_cancel`、
    /// `uart_close` 或客户端取消通知中止）/ device_error（读线程致命错误，
    /// 如串口设备物理断开）。
    pub reason: String,
    /// 原始文件字节数（base64 模式下为编码前字节数）。
    pub raw_bytes: u64,
    /// 实际写入串口的字节数（base64 模式含换行）。
    pub sent_bytes: u64,
    /// 已完成的发送片数。
    pub chunks: u64,
    /// 总耗时（毫秒）。
    pub elapsed_ms: u64,
    /// 发送期间上行缓冲溢出增量（对账诊断用；发送文件场景通常为 0）。
    pub overflow_delta: u64,
    /// 累计溢出字节数。
    pub overflow_total: u64,
    /// 读线程致命错误（`reason="device_error"` 时非空；正常为 `None`）。
    pub device_error: Option<String>,
}

/// 活动串口连接（仅存在于 `SerialManager.inner` 的 Some 分支中）。
struct ActivePort {
    port: Arc<Mutex<Box<dyn SerialPort>>>,
    config: PortConfig,
    port_name: String,
    buffer: Arc<RingBuf>,
    /// 读线程停止令牌（可中断事件等待）。
    stop: crate::reader::ReaderStop,
    reader: Option<JoinHandle<()>>,
    read_error: Arc<Mutex<Option<String>>>,
    /// 上次读取时的累计溢出计数（用于计算增量），随端口生命周期存在。
    last_overflow: Arc<Mutex<u64>>,
    /// 文件发送会话状态（进度/取消/完成通知）。
    send: Arc<SendState>,
}

/// 串口管理器。
#[derive(Default)]
pub struct SerialManager {
    ports: Mutex<HashMap<String, ActivePort>>,
    /// 串行化 write/read/configure 工具调用。
    io_lock: tokio::sync::Mutex<()>,
}

impl SerialManager {
    /// 创建串口管理器（默认关闭状态）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 枚举本机可用串口。
    pub fn list_ports(&self) -> Result<Vec<PortInfo>, String> {
        let ports = serialport::available_ports().map_err(|e| format!("枚举串口失败: {e}"))?;
        Ok(ports
            .into_iter()
            .map(|p| PortInfo {
                name: p.port_name,
                port_type: match &p.port_type {
                    serialport::SerialPortType::UsbPort(_info) => "usb".into(),
                    serialport::SerialPortType::BluetoothPort => "bluetooth".into(),
                    serialport::SerialPortType::PciPort => "pci".into(),
                    serialport::SerialPortType::Unknown => "unknown".into(),
                },
                description: match &p.port_type {
                    serialport::SerialPortType::UsbPort(info) => format!(
                        "vid={:04x} pid={:04x} serial={} product={}",
                        info.vid,
                        info.pid,
                        info.serial_number.as_deref().unwrap_or(""),
                        info.product.as_deref().unwrap_or("")
                    ),
                    _ => String::new(),
                },
            })
            .collect())
    }

    /// 打开串口并启动事件驱动读线程。同一端口重复打开会报错（先 close 再 open）。
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &self,
        port_name: &str,
        baudrate: u32,
        data_bits: DataBits,
        parity: Parity,
        stop_bits: StopBits,
        flow_control: FlowControl,
        read_timeout_ms: u64,
        buffer_size: usize,
        discard_on_open: bool,
    ) -> Result<(), String> {
        // 同一端口重复打开会报错，避免误覆盖其它会话。
        if self.ports.lock().unwrap().contains_key(port_name) {
            return Err(format!("端口 {port_name} 已打开，请先调用 uart_close"));
        }

        let builder = serialport::new(port_name, baudrate);
        // 用 open_native() 拿到具体端口类型（TTYPort/COMPort），
        // 以便为事件驱动/非阻塞读线程提供底层 fd/句柄。
        let native = builder
            .data_bits(data_bits)
            .parity(parity)
            .stop_bits(stop_bits)
            .flow_control(flow_control)
            .timeout(Duration::from_millis(read_timeout_ms))
            .open_native()
            .map_err(|e| format!("打开 {port_name} 失败: {e}"))?;

        let reader_native = native
            .try_clone_native()
            .map_err(|e| format!("克隆串口读句柄失败: {e}"))?;
        let port = Arc::new(Mutex::new(Box::new(native) as Box<dyn SerialPort>));
        if discard_on_open {
            let guard = port.lock().unwrap();
            let _ = guard.clear(serialport::ClearBuffer::Input);
        }

        let buffer = RingBuf::new(buffer_size);
        let stop = Arc::new(AtomicBool::new(false));
        let read_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        // 事件驱动读线程：串口 → 环形缓冲（详见 reader 模块）。
        // 使用独立句柄，避免与命令操作（write/configure）争用同一 Mutex。
        let (event_reader, reader_stop) =
            crate::reader::EventReader::new(reader_native, buffer.clone(), stop.clone())
                .map_err(|e| format!("初始化读线程失败: {e}"))?;
        let reader_error = read_error.clone();
        let reader = std::thread::Builder::new()
            .name("ser2mcp-reader".into())
            .spawn(move || {
                if let Some(e) = event_reader.run() {
                    *reader_error.lock().unwrap() = Some(e);
                }
            })
            .map_err(|e| format!("启动读线程失败: {e}"))?;

        let mut ports = self.ports.lock().unwrap();
        ports.insert(
            port_name.to_string(),
            ActivePort {
                port,
                config: PortConfig {
                    baudrate,
                    data_bits,
                    parity,
                    stop_bits,
                    flow_control,
                    read_timeout_ms,
                    buffer_size,
                },
                port_name: port_name.to_string(),
                buffer,
                stop: reader_stop,
                reader: Some(reader),
                read_error,
                last_overflow: Arc::new(Mutex::new(0)),
                send: Arc::new(SendState::new()),
            },
        );
        Ok(())
    }

    /// 运行时重配置（仅更新传入的项）。
    #[allow(clippy::too_many_arguments)]
    pub async fn configure(
        &self,
        port_name: &str,
        baudrate: Option<u32>,
        data_bits: Option<DataBits>,
        parity: Option<Parity>,
        stop_bits: Option<StopBits>,
        flow_control: Option<FlowControl>,
        read_timeout_ms: Option<u64>,
    ) -> Result<(), String> {
        let _guard = self.io_lock.lock().await;
        let mut ports = self.ports.lock().unwrap();
        let ap = ports
            .get_mut(port_name)
            .ok_or_else(|| format!("端口 {port_name} 未打开，请先调用 uart_open"))?;
        {
            let mut port = ap.port.lock().unwrap();
            if let Some(v) = baudrate {
                port.set_baud_rate(v)
                    .map_err(|e| format!("设置波特率失败: {e}"))?;
                ap.config.baudrate = v;
            }
            if let Some(v) = data_bits {
                port.set_data_bits(v)
                    .map_err(|e| format!("设置数据位失败: {e}"))?;
                ap.config.data_bits = v;
            }
            if let Some(v) = parity {
                port.set_parity(v)
                    .map_err(|e| format!("设置校验位失败: {e}"))?;
                ap.config.parity = v;
            }
            if let Some(v) = stop_bits {
                port.set_stop_bits(v)
                    .map_err(|e| format!("设置停止位失败: {e}"))?;
                ap.config.stop_bits = v;
            }
            if let Some(v) = flow_control {
                port.set_flow_control(v)
                    .map_err(|e| format!("设置流控失败: {e}"))?;
                ap.config.flow_control = v;
            }
            if let Some(v) = read_timeout_ms {
                port.set_timeout(Duration::from_millis(v))
                    .map_err(|e| format!("设置读超时失败: {e}"))?;
                ap.config.read_timeout_ms = v;
            }
        }
        Ok(())
    }

    /// 写入数据（只发不等），返回实际写入字节数。
    pub async fn write(&self, port_name: &str, data: &[u8]) -> Result<usize, String> {
        let _guard = self.io_lock.lock().await;
        let ports = self.ports.lock().unwrap();
        let ap = ports
            .get(port_name)
            .ok_or_else(|| format!("端口 {port_name} 未打开，请先调用 uart_open"))?;
        write_locked(&ap.port, data)
    }

    /// 流式发送文件内容（分片 + 可选片间间隔），全程持有 `io_lock`，期间其它
    /// 写/配置/期待工具调用排队；`uart_available` 不受影响，可随时查询进度。
    ///
    /// `chunks` 为已编码（base64 含换行）的待发送分片迭代器；`total_bytes` 为
    /// 原始文件字节数。每个检查点（每片写入前）检测取消标志（`uart_send_cancel`
    /// / `uart_close` / 客户端取消令牌 `ct`）与端口是否仍打开：
    /// 被中止时返回 `reason="cancelled"`（非错误），调用方可用 `sent_bytes`
    /// 与对端对账后决定是否重发。分片读取失败（`Err`）时终止并返回错误，
    /// 错误信息含已发送进度。
    pub async fn send_file(
        &self,
        port_name: &str,
        chunks: impl Iterator<Item = Result<Vec<u8>, String>>,
        total_bytes: u64,
        gap_ms: u64,
        ct: Option<&CancellationToken>,
    ) -> Result<SendFileOutcome, String> {
        let _guard = self.io_lock.lock().await;
        let (port, last_overflow, send) = {
            let ports = self.ports.lock().unwrap();
            let ap = ports
                .get(port_name)
                .ok_or_else(|| format!("端口 {port_name} 未打开，请先调用 uart_open"))?;
            (ap.port.clone(), ap.last_overflow.clone(), ap.send.clone())
        };
        let overflow_before = *last_overflow.lock().unwrap();
        let started = Instant::now();
        send.begin(total_bytes);
        let mut sent_bytes = 0u64;
        let mut chunks_done = 0u64;
        let mut reason = "completed";
        let mut device_error: Option<String> = None;
        let mut chunks = chunks;
        loop {
            let chunk = match chunks.next() {
                None => break,
                Some(Ok(c)) => c,
                Some(Err(e)) => {
                    send.finish("error", sent_bytes, chunks_done);
                    return Err(format!(
                        "{e}（已发送 {sent_bytes} 字节 / {chunks_done} 片）"
                    ));
                }
            };
            // 检查点：取消标志（uart_send_cancel / uart_close）、客户端取消令牌
            // （notifications/cancelled）、端口已被关闭，或读线程致命错误
            // （设备物理断开/硬件故障——写侧可能仍假成功，据此中止避免"发完假象"）。
            let (port_gone, reader_fault) = {
                let ports = self.ports.lock().unwrap();
                match ports.get(port_name) {
                    None => (true, None),
                    Some(ap) => (false, ap.read_error.lock().unwrap().clone()),
                }
            };
            if send.is_cancelled() || ct.is_some_and(|c| c.is_cancelled()) || port_gone {
                reason = "cancelled";
                break;
            }
            if let Some(e) = reader_fault {
                reason = "device_error";
                device_error = Some(e);
                break;
            }
            match write_locked(&port, &chunk) {
                Ok(n) => {
                    sent_bytes += n as u64;
                    chunks_done += 1;
                    send.update(sent_bytes, chunks_done);
                }
                Err(e) => {
                    send.finish("error", sent_bytes, chunks_done);
                    return Err(format!(
                        "写入失败: {e}（已发送 {sent_bytes} 字节 / {chunks_done} 片）"
                    ));
                }
            }
            if gap_ms > 0 {
                let sleep = tokio::time::sleep(Duration::from_millis(gap_ms));
                if let Some(ct) = ct {
                    // 片间间隔期间也要响应取消（gap 可能较大）。
                    tokio::select! {
                        _ = ct.cancelled() => { reason = "cancelled"; break; }
                        _ = sleep => {}
                    }
                } else {
                    sleep.await;
                }
            }
        }
        send.finish(reason, sent_bytes, chunks_done);
        let overflow_total = *last_overflow.lock().unwrap();
        Ok(SendFileOutcome {
            reason: reason.into(),
            raw_bytes: total_bytes,
            sent_bytes,
            chunks: chunks_done,
            elapsed_ms: started.elapsed().as_millis() as u64,
            overflow_delta: overflow_total.saturating_sub(overflow_before),
            overflow_total,
            device_error,
        })
    }

    /// 请求中止当前 `uart_send_file` 传输（无传输时为 no-op）。
    /// 返回调用前的进度快照，供调用方判断是否真的中止了传输。
    pub fn cancel_send(&self, port_name: &str) -> Result<SendProgress, String> {
        let ports = self.ports.lock().unwrap();
        let ap = ports
            .get(port_name)
            .ok_or_else(|| format!("端口 {port_name} 未打开，请先调用 uart_open"))?;
        let snap = ap.send.snapshot();
        if snap.active {
            ap.send.cancel();
        }
        Ok(snap)
    }

    /// 等待串口输出中出现 pattern（内容匹配，跨分片/wrap）；`data` 传入时先发送再等待。
    /// 命中或超时后返回。详见 `ExpectOutcome`。
    pub async fn expect(
        &self,
        port_name: &str,
        data: Option<&[u8]>,
        pattern: &[u8],
        timeout_ms: u64,
        consume: bool,
    ) -> Result<ExpectOutcome, String> {
        self.expect_inner(port_name, pattern, timeout_ms, consume, data, None)
            .await
    }

    /// 等待串口输出中出现 pattern，命中后在同一临界区内**立即**发送 `reply`
    /// （消除"expect 返回 → 再调 write"之间的往返延迟）。超时未命中时不发送。
    pub async fn expect_send(
        &self,
        port_name: &str,
        pattern: &[u8],
        reply: &[u8],
        timeout_ms: u64,
        consume: bool,
    ) -> Result<ExpectOutcome, String> {
        self.expect_inner(port_name, pattern, timeout_ms, consume, None, Some(reply))
            .await
    }

    /// 内部实现：可选先发送 `send` → 等待 pattern → 命中后可选发送 `reply`。
    /// 整个流程在同一 `io_lock` 临界区内（原子，无并发工具调用插入）。
    #[allow(clippy::too_many_arguments)]
    async fn expect_inner(
        &self,
        port_name: &str,
        pattern: &[u8],
        timeout_ms: u64,
        consume: bool,
        send: Option<&[u8]>,
        reply: Option<&[u8]>,
    ) -> Result<ExpectOutcome, String> {
        let _guard = self.io_lock.lock().await;
        let (buffer, last_overflow, port) = {
            let ports = self.ports.lock().unwrap();
            let ap = ports
                .get(port_name)
                .ok_or_else(|| format!("端口 {port_name} 未打开，请先调用 uart_open"))?;
            (ap.buffer.clone(), ap.last_overflow.clone(), ap.port.clone())
        };

        let mut written = 0;
        if let Some(send) = send {
            written = write_locked(&port, send)?;
        }

        let timeout = Duration::from_millis(timeout_ms);
        let start = Instant::now();
        let (matched, data, overflow_total) = loop {
            // find_and_take 在同一临界区内完成查找与消费（读线程无法插入覆盖）。
            let (pos, taken, ovf) = buffer.find_and_take(pattern, consume);
            if pos.is_some() {
                break (true, taken, ovf);
            }
            if start.elapsed() >= timeout {
                break (false, Vec::new(), ovf);
            }
            tokio::select! {
                _ = buffer.notified() => {}
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        };

        if matched {
            if let Some(reply) = reply {
                written = write_locked(&port, reply)?;
            }
        }

        // 消费状态：仅"命中且 consume=true"视为消费（更新溢出基线），
        // 后续 uart_read 的 overflow_delta 增量语义不受影响。
        // - 超时未命中：只读计算 delta 报告自上次消费以来的数据缺口（帮助识别
        //   "缓冲溢出覆盖了 pattern" 导致的超时），但不更新基线、不消费数据。
        // - 命中但 consume=false：不消费、不更新基线，delta 无意义恒为 0。
        let (overflow_delta, overflow_total) = if matched && consume {
            let mut last = last_overflow.lock().unwrap();
            let delta = overflow_total.saturating_sub(*last);
            *last = overflow_total;
            (delta, overflow_total)
        } else if matched {
            (0, overflow_total)
        } else {
            let last = last_overflow.lock().unwrap();
            let delta = overflow_total.saturating_sub(*last);
            (delta, overflow_total)
        };
        let (buffered, _) = buffer.stats();

        Ok(ExpectOutcome {
            matched,
            data,
            written,
            reason: if matched {
                ExpectReason::Matched
            } else {
                ExpectReason::Timeout
            },
            overflow_delta,
            overflow_total,
            buffered,
        })
    }

    /// 拉取缓冲：等待"空闲判定 / 达到 max_bytes / 超时"三者之一后返回全部未读数据。
    ///
    /// 空闲判定（跨平台）增强：除环形缓冲 `idle_ms` 无新写入外，还要求串口驱动
    /// 侧无可读字节（`bytes_to_read() == 0`）。这避免读线程在"驱动缓冲排空后、
    /// 剩余数据仍在线路/USB 传输中"的窗口期（Windows 实测可达数百 ms）被误判为
    /// 响应结束，从而把"数据流中"当作"响应已结束"。Unix 读线程为 poll 事件驱动，
    /// 该检查同样安全（更保守，不会误判提前返回）。
    pub async fn read(
        &self,
        port_name: &str,
        idle_ms: u64,
        max_bytes: usize,
        timeout_ms: u64,
    ) -> Result<ReadOutcome, String> {
        let _guard = self.io_lock.lock().await;
        let (buffer, last_overflow, port) = {
            let ports = self.ports.lock().unwrap();
            let ap = ports
                .get(port_name)
                .ok_or_else(|| format!("端口 {port_name} 未打开，请先调用 uart_open"))?;
            (ap.buffer.clone(), ap.last_overflow.clone(), ap.port.clone())
        };
        let idle = Duration::from_millis(idle_ms);
        let timeout = Duration::from_millis(timeout_ms);
        let start = Instant::now();

        let reason = loop {
            let (cur_len, _) = buffer.stats();
            let age = buffer.last_write_age();
            if cur_len > 0 && age >= idle {
                // 串口驱动缓冲是否仍有未搬入环形缓冲的数据（读线程尚未读完）。
                // 端口拔出/驱动故障时传播错误，避免把故障误判为"响应结束"。
                let drv_empty = {
                    let p = port.lock().unwrap();
                    p.bytes_to_read()
                        .map_err(|e| format!("查询串口可读字节数失败: {e}"))?
                        == 0
                };
                if drv_empty {
                    break ReadReason::Idle;
                }
            }
            if cur_len >= max_bytes {
                break ReadReason::MaxBytes;
            }
            if start.elapsed() >= timeout {
                break ReadReason::Timeout;
            }
            // 等待新数据或周期性复查
            tokio::select! {
                _ = buffer.notified() => {}
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        };

        let (data, overflow_total) = buffer.take_all();
        let mut last = last_overflow.lock().unwrap();
        let overflow_delta = overflow_total.saturating_sub(*last);
        *last = overflow_total;
        let (buffered, _) = buffer.stats();

        Ok(ReadOutcome {
            data,
            reason,
            overflow_delta,
            overflow_total,
            buffered,
        })
    }

    /// 运行状态快照。
    pub fn available(&self, port_name: &str) -> AvailableInfo {
        let ports = self.ports.lock().unwrap();
        match ports.get(port_name) {
            None => AvailableInfo {
                open: false,
                port: None,
                baudrate: None,
                data_bits: None,
                parity: None,
                stop_bits: None,
                flow_control: None,
                read_timeout_ms: None,
                buffer_size: None,
                buffered_bytes: 0,
                overflow_total: 0,
                read_error: None,
                send: SendProgress::default(),
            },
            Some(ap) => {
                let (buffered, overflow_total) = ap.buffer.stats();
                let cfg = &ap.config;
                AvailableInfo {
                    open: true,
                    port: Some(ap.port_name.clone()),
                    baudrate: Some(cfg.baudrate),
                    data_bits: Some(data_bits_to_u8(cfg.data_bits)),
                    parity: Some(format!("{:?}", cfg.parity).to_lowercase()),
                    stop_bits: Some(stop_bits_to_u8(cfg.stop_bits)),
                    flow_control: Some(format!("{:?}", cfg.flow_control).to_lowercase()),
                    read_timeout_ms: Some(cfg.read_timeout_ms),
                    buffer_size: Some(cfg.buffer_size),
                    buffered_bytes: buffered,
                    overflow_total,
                    read_error: ap.read_error.lock().unwrap().clone(),
                    send: ap.send.snapshot(),
                }
            }
        }
    }

    /// 清空缓冲中的未读数据，返回清掉的字节数。
    pub fn clear(&self, port_name: &str) -> Result<usize, String> {
        let ports = self.ports.lock().unwrap();
        let ap = ports
            .get(port_name)
            .ok_or_else(|| format!("端口 {port_name} 未打开，请先调用 uart_open"))?;
        let (bytes, _) = ap.buffer.stats();
        ap.buffer.clear();
        Ok(bytes)
    }

    /// 关闭串口：先请求中止进行中的文件发送并等待其退出（最多 30s 兑底），
    /// 再停止读线程并释放端口句柄。发送循环在下一个检查点退出，最坏多写一片。
    pub async fn close(&self, port_name: &str) -> Result<(), String> {
        let send = {
            let ports = self.ports.lock().unwrap();
            ports.get(port_name).map(|ap| ap.send.clone())
        };
        if let Some(send) = send {
            if send.is_active() {
                send.cancel();
                tokio::select! {
                    _ = send.wait_done() => {}
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {
                        return Err(format!(
                            "端口 {port_name} 的文件发送未在 30s 内退出，关闭中止"
                        ));
                    }
                }
            }
        }
        let ap = self
            .ports
            .lock()
            .unwrap()
            .remove(port_name)
            .ok_or_else(|| format!("端口 {port_name} 未打开，请先调用 uart_open"))?;
        ap.stop.signal();
        if let Some(handle) = ap.reader {
            // 停止信号会中断事件等待，join 很快返回；此处短暂阻塞可接受。
            let _ = handle.join();
        }
        // 读线程已退出，此处 drop ap 释放端口句柄。
        Ok(())
    }
}

fn data_bits_to_u8(v: DataBits) -> u8 {
    match v {
        DataBits::Five => 5,
        DataBits::Six => 6,
        DataBits::Seven => 7,
        DataBits::Eight => 8,
    }
}

/// 写入数据（只发不等），返回实际写入字节数。调用方需已持有 `io_lock`（或处于
/// `expect_inner` 的临界区内）且端口已打开。
fn write_locked(port: &Arc<Mutex<Box<dyn SerialPort>>>, data: &[u8]) -> Result<usize, String> {
    let mut p = port.lock().unwrap();
    let mut written = 0;
    while written < data.len() {
        let n = p
            .write(&data[written..])
            .map_err(|e| format!("写入失败: {e}"))?;
        written += n;
    }
    p.flush().map_err(|e| format!("flush 失败: {e}"))?;
    Ok(written)
}

fn stop_bits_to_u8(v: StopBits) -> u8 {
    match v {
        StopBits::One => 1,
        StopBits::Two => 2,
    }
}
