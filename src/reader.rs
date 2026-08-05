//! 事件驱动/非阻塞读线程（平台适配层）。
//!
//! 目标：用非阻塞/事件驱动读取替代“固定超时阻塞读”，避免 USB 转串口驱动
//! 按读超时边界成批交付数据带来的额外延迟（该现象实测于手头的 CH340 / CP210x）。
//!
//! 平台差异（因此需要适配层，无法三平台共用同一实现）：
//! - Unix（Linux / macOS）：`serialport::TTYPort` 暴露 fd，可用 `poll(2)` 做真正的
//!   事件驱动等待（无轮询开销），并用自建管道（self-pipe）唤醒停止。
//! - Windows：`serialport` 以非 OVERLAPPED 句柄打开 COM 口（`FILE_ATTRIBUTE_NORMAL`、
//!   share=0），无法在上层使用可中断的 `WaitCommEvent`（overlapped 需要 OVERLAPPED
//!   句柄；blocking 版无法在关闭时被干净地打断）。因此采用非阻塞模型：短等待 +
//!   `bytes_to_read()` 门控，仅在确有数据时才调用 `read()`，并配合
//!   `timeBeginPeriod(1)` 获得毫秒级唤醒粒度。

use std::io;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serialport::SerialPort;

use crate::ring::RingBuf;

/// 单次读取块大小。
const READ_CHUNK: usize = 4096;

#[cfg(unix)]
pub(crate) type NativePort = serialport::TTYPort;
#[cfg(windows)]
pub(crate) type NativePort = serialport::COMPort;

/// 读线程停止令牌：调用 [`ReaderStop::signal`] 可中断事件等待并让读线程尽快退出。
pub(crate) struct ReaderStop {
    imp: imp::Stop,
    flag: Arc<AtomicBool>,
}

impl ReaderStop {
    pub(crate) fn signal(&self) {
        self.flag.store(true, Ordering::Relaxed);
        self.imp.signal();
    }
}

/// 事件驱动/非阻塞读线程：串口 → 环形缓冲。
pub(crate) struct EventReader {
    port: NativePort,
    waiter: imp::Waiter,
    buffer: Arc<RingBuf>,
    stop: Arc<AtomicBool>,
    chunk: [u8; READ_CHUNK],
}

impl EventReader {
    /// 创建读线程与停止令牌。`port` 应为独立克隆的串口句柄。
    pub(crate) fn new(
        port: NativePort,
        buffer: Arc<RingBuf>,
        stop: Arc<AtomicBool>,
    ) -> io::Result<(Self, ReaderStop)> {
        let (waiter, stop_imp) = imp::Waiter::new(&port)?;
        Ok((
            Self {
                port,
                waiter,
                buffer,
                stop: stop.clone(),
                chunk: [0u8; READ_CHUNK],
            },
            ReaderStop {
                imp: stop_imp,
                flag: stop,
            },
        ))
    }

    /// 运行读循环，返回致命错误（正常停止返回 `None`）。
    pub(crate) fn run(mut self) -> Option<String> {
        // Windows：提升进程定时器分辨率，使短等待达到毫秒级。
        let _timer = imp::TimerGuard::acquire();

        loop {
            if self.stop.load(Ordering::Relaxed) {
                return None;
            }
            match self.waiter.wait() {
                Ok(true) => {}
                // 收到停止信号（Unix 自建管道）：回到循环顶部，由 stop 标志决定退出。
                Ok(false) => continue,
                Err(e) => return Some(format!("等待串口事件失败: {e}")),
            }
            // 数据可用：仅在确有数据时才 read()，并排空到空为止。
            loop {
                if self.stop.load(Ordering::Relaxed) {
                    return None;
                }
                match self.port.bytes_to_read() {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) => return Some(format!("查询串口可读字节数失败: {e}")),
                }
                match self.port.read(&mut self.chunk) {
                    Ok(0) => break,
                    Ok(n) => self.buffer.push(&self.chunk[..n]),
                    Err(e)
                        if e.kind() == io::ErrorKind::TimedOut
                            || e.kind() == io::ErrorKind::WouldBlock =>
                    {
                        break;
                    }
                    Err(e) => return Some(format!("读取串口失败: {e}")),
                }
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

    use libc::{POLLHUP, POLLIN, POLLNVAL, pollfd};

    /// Unix 无需调整定时器分辨率。
    pub(crate) struct TimerGuard;

    impl TimerGuard {
        pub(crate) fn acquire() -> Self {
            Self
        }
    }

    /// 停止令牌：向自建管道写入一个字节，唤醒 `poll`。
    pub(crate) struct Stop {
        write_fd: OwnedFd,
    }

    impl Stop {
        pub(crate) fn signal(&self) {
            let byte = [1u8];
            unsafe {
                libc::write(self.write_fd.as_raw_fd(), byte.as_ptr().cast(), byte.len());
            }
        }
    }

    /// 事件等待器：`poll` 串口 fd + 停止管道读端。
    pub(crate) struct Waiter {
        read_fd: OwnedFd,
        port_fd: RawFd,
    }

    impl Waiter {
        pub(crate) fn new(port: &super::NativePort) -> io::Result<(Self, Stop)> {
            let mut fds = [0 as RawFd; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok((
                Self {
                    read_fd: unsafe { OwnedFd::from_raw_fd(fds[0]) },
                    port_fd: port.as_raw_fd(),
                },
                Stop {
                    write_fd: unsafe { OwnedFd::from_raw_fd(fds[1]) },
                },
            ))
        }

        /// 返回 `Ok(true)` 表示串口可读；`Ok(false)` 表示收到停止信号。
        pub(crate) fn wait(&mut self) -> io::Result<bool> {
            let mut fds = [
                pollfd {
                    fd: self.port_fd,
                    events: POLLIN,
                    revents: 0,
                },
                pollfd {
                    fd: self.read_fd.as_raw_fd(),
                    events: POLLIN,
                    revents: 0,
                },
            ];
            loop {
                let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
                if r < 0 {
                    let e = io::Error::last_os_error();
                    if e.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(e);
                }
                break;
            }
            if fds[1].revents & (POLLIN | POLLHUP) != 0 {
                // 停止管道有数据（或已关闭）：停止信号。
                return Ok(false);
            }
            if fds[0].revents & POLLIN != 0 {
                return Ok(true);
            }
            if fds[0].revents & (POLLHUP | POLLNVAL) != 0 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "串口 fd 已挂断"));
            }
            Ok(true)
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};

    /// 进程级定时器分辨率守卫：引用计数，首个读者提升到 1ms，末个读者恢复。
    pub(crate) struct TimerGuard;

    static TIMER_REFCNT: OnceLock<Mutex<u32>> = OnceLock::new();

    impl TimerGuard {
        pub(crate) fn acquire() -> Self {
            let mut count = TIMER_REFCNT.get_or_init(|| Mutex::new(0)).lock().unwrap();
            if *count == 0 {
                unsafe {
                    timeBeginPeriod(1);
                }
            }
            *count += 1;
            Self
        }
    }

    impl Drop for TimerGuard {
        fn drop(&mut self) {
            let mut count = TIMER_REFCNT
                .get()
                .expect("TimerGuard 必须先 acquire")
                .lock()
                .unwrap();
            *count -= 1;
            if *count == 0 {
                unsafe {
                    timeEndPeriod(1);
                }
            }
        }
    }

    /// 停止令牌：Windows 轮询模型下由 stop 标志驱动，无需额外唤醒。
    pub(crate) struct Stop;

    impl Stop {
        pub(crate) fn signal(&self) {}
    }

    /// 等待器：毫秒级短等待（配合 `timeBeginPeriod`），由调用方检查 stop 标志。
    pub(crate) struct Waiter;

    impl Waiter {
        pub(crate) fn new(_port: &super::NativePort) -> io::Result<(Self, Stop)> {
            Ok((Self, Stop))
        }

        pub(crate) fn wait(&mut self) -> io::Result<bool> {
            std::thread::park_timeout(Duration::from_millis(1));
            Ok(true)
        }
    }
}
