//! 串口管理器：打开/配置/后台读线程/写/读取。
//! 支持同时打开多个串口，以端口名为句柄；工具调用全局串行化（AI 回合制调用天然串行）。
//!
//! 架构：
//! ```text
//! 串口 ──► 后台读线程(生产者) ──► RingBuf(有界环形缓冲) ──► uart_read/uart_exchange(消费者)
//! ```
//! - 读线程只做"读串口 → 写缓冲"，永不阻塞在向 host 发送上；
//! - 缓冲写满后覆盖最旧数据并累计溢出计数，数据缺口可被上层检测；
//! - 所有写/读/配置操作经 `io_lock` 串行化，保证 AI 回合制调用下语义清晰。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};

use crate::ring::RingBuf;

/// 默认波特率（115200）。
pub const DEFAULT_BAUDRATE: u32 = 115200;
/// 默认串口读超时（毫秒），也是读线程的最长阻塞时间。
pub const DEFAULT_READ_TIMEOUT_MS: u64 = 100;
/// 默认上行环形缓冲大小（1 MiB），写满覆盖最旧数据并计数溢出。
pub const DEFAULT_BUFFER_SIZE: usize = 1024 * 1024; // 1 MiB
/// 默认空闲判定阈值（毫秒）：出现新数据后持续该时长无新字节视为一次响应结束。
pub const DEFAULT_IDLE_MS: u64 = 300;
/// 默认单次拉取触发上限（64 KiB）：未读字节数达到该值立即返回。
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024;
/// 默认总等待超时（毫秒）。
pub const DEFAULT_TIMEOUT_MS: u64 = 5000;
const READ_CHUNK: usize = 4096;
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
    /// 读线程的串口读超时（毫秒）。
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
}

/// 活动串口连接（仅存在于 `SerialManager.inner` 的 Some 分支中）。
struct ActivePort {
    port: Arc<Mutex<Box<dyn SerialPort>>>,
    config: PortConfig,
    port_name: String,
    buffer: Arc<RingBuf>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    read_error: Arc<Mutex<Option<String>>>,
    /// 上次读取时的累计溢出计数（用于计算增量），随端口生命周期存在。
    last_overflow: Arc<Mutex<u64>>,
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

    /// 打开串口并启动后台读线程。同一端口重复打开会报错（先 close 再 open）。
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
        let port = builder
            .data_bits(data_bits)
            .parity(parity)
            .stop_bits(stop_bits)
            .flow_control(flow_control)
            .timeout(Duration::from_millis(read_timeout_ms))
            .open()
            .map_err(|e| format!("打开 {port_name} 失败: {e}"))?;

        let port = Arc::new(Mutex::new(port));
        if discard_on_open {
            let guard = port.lock().unwrap();
            let _ = guard.clear(serialport::ClearBuffer::Input);
        }

        let buffer = RingBuf::new(buffer_size);
        let stop = Arc::new(AtomicBool::new(false));
        let read_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        // 后台读线程：串口 → 环形缓冲。
        let reader_port = port.clone();
        let reader_buffer = buffer.clone();
        let reader_stop = stop.clone();
        let reader_error = read_error.clone();
        let reader = std::thread::Builder::new()
            .name("ser2mcp-reader".into())
            .spawn(move || {
                let mut chunk = [0u8; READ_CHUNK];
                loop {
                    if reader_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let n = {
                        let mut guard = match reader_port.lock() {
                            Ok(g) => g,
                            Err(_) => break, // 锁中毒：直接退出
                        };
                        match guard.read(&mut chunk) {
                            Ok(n) => n,
                            Err(e) => match e.kind() {
                                // 读超时/端口暂时忙：正常现象，继续循环
                                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                                    continue;
                                }
                                // 致命错误：记录并退出读线程
                                other => {
                                    *reader_error.lock().unwrap() = Some(format!("{other}: {e}"));
                                    break;
                                }
                            },
                        }
                    };
                    if n > 0 {
                        reader_buffer.push(&chunk[..n]);
                    }
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
                stop,
                reader: Some(reader),
                read_error,
                last_overflow: Arc::new(Mutex::new(0)),
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
        let mut port = ap.port.lock().unwrap();
        let mut written = 0;
        while written < data.len() {
            let n = port
                .write(&data[written..])
                .map_err(|e| format!("写入失败: {e}"))?;
            written += n;
        }
        port.flush().map_err(|e| format!("flush 失败: {e}"))?;
        Ok(written)
    }

    /// 拉取缓冲：等待"空闲判定 / 达到 max_bytes / 超时"三者之一后返回全部未读数据。
    pub async fn read(
        &self,
        port_name: &str,
        idle_ms: u64,
        max_bytes: usize,
        timeout_ms: u64,
    ) -> Result<ReadOutcome, String> {
        let _guard = self.io_lock.lock().await;
        let (buffer, last_overflow) = {
            let ports = self.ports.lock().unwrap();
            let ap = ports
                .get(port_name)
                .ok_or_else(|| format!("端口 {port_name} 未打开，请先调用 uart_open"))?;
            (ap.buffer.clone(), ap.last_overflow.clone())
        };
        let idle = Duration::from_millis(idle_ms);
        let timeout = Duration::from_millis(timeout_ms);
        let start = Instant::now();

        let reason = loop {
            let (cur_len, _) = buffer.stats();
            let age = buffer.last_write_age();
            if cur_len > 0 && age >= idle {
                break ReadReason::Idle;
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

    /// 关闭串口（异步：先停读线程并 join，再释放端口句柄）。
    pub async fn close(&self, port_name: &str) -> Result<(), String> {
        let ap = self
            .ports
            .lock()
            .unwrap()
            .remove(port_name)
            .ok_or_else(|| format!("端口 {port_name} 未打开，请先调用 uart_open"))?;
        ap.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = ap.reader {
            // 读线程至多 read_timeout 内返回；此处短暂阻塞可接受。
            let _ = handle.join();
        }
        // 读线程已退出，此处 drop ap 释放端口句柄（Windows 上安全）。
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

fn stop_bits_to_u8(v: StopBits) -> u8 {
    match v {
        StopBits::One => 1,
        StopBits::Two => 2,
    }
}
