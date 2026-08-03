//! 有界环形缓冲：串口上行数据的囤积区。
//!
//! 后台读线程（生产者）持续 push，AI 侧工具调用（消费者）按需 take_all。
//! 缓冲有上限；写满后**覆盖最旧数据**（串口调试场景最新数据通常最重要），
//! 并累计 `overflow_total` 计数，使数据缺口可被上层检测而非静默丢失。
//!
//! 并发模型：`RingBuf` 内部用 `std::sync::Mutex` 保护环形区（临界区极短），
//! 写入后通过 `tokio::sync::Notify` 唤醒等待中的读取者。

use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::Notify;

/// 环形缓冲（线程安全包装）。
#[derive(Debug)]
pub struct RingBuf {
    inner: Mutex<RingBuffer>,
    notify: Notify,
}

impl RingBuf {
    /// 创建指定容量的环形缓冲。
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RingBuffer::new(capacity)),
            notify: Notify::new(),
        })
    }

    /// 写入数据。满则覆盖最旧数据并累计溢出计数，随后唤醒等待者。
    pub fn push(&self, data: &[u8]) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.push(data);
        }
        self.notify.notify_one();
    }

    /// 取走全部未读数据，返回数据与**累计**溢出字节数。
    pub fn take_all(&self) -> (Vec<u8>, u64) {
        let mut inner = self.inner.lock().unwrap();
        inner.take_all()
    }

    /// 当前未读字节数与累计溢出字节数。
    pub fn stats(&self) -> (usize, u64) {
        let inner = self.inner.lock().unwrap();
        (inner.len, inner.overflow_total)
    }

    /// 清空未读数据（溢出计数保留）。
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.clear();
    }

    /// 等待下一次写入（或超时由调用方负责）。
    pub fn notified(&self) -> impl Future<Output = ()> + '_ {
        self.notify.notified()
    }

    /// 距最近一次写入经过的时间（用于空闲判定）。
    pub fn last_write_age(&self) -> std::time::Duration {
        let inner = self.inner.lock().unwrap();
        inner.last_write.elapsed()
    }
}

/// 环形缓冲核心（无锁，由外部 Mutex 保护）。
#[derive(Debug)]
struct RingBuffer {
    data: Vec<u8>,
    /// 写游标（下一个写入位置）。
    head: usize,
    /// 当前有效字节数。
    len: usize,
    capacity: usize,
    /// 累计被覆盖丢弃的字节数。
    overflow_total: u64,
    /// 最近一次写入时间（用于空闲判定）。
    last_write: Instant,
}

impl RingBuffer {
    fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            data: vec![0u8; capacity],
            head: 0,
            len: 0,
            capacity,
            overflow_total: 0,
            last_write: Instant::now(),
        }
    }

    /// 逻辑读游标（有效区起点）。
    fn tail(&self) -> usize {
        (self.head + self.capacity - self.len) % self.capacity
    }

    fn push(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let n = data.len();
        if n > self.capacity {
            // 单次写入超过容量：只能保留最后 capacity 字节，其余全部溢出。
            let data = &data[n - self.capacity..];
            self.overflow_total += (n - self.capacity) as u64;
            self.len = 0;
            self.write_at_head(data);
        } else {
            // 需要腾出的最旧字节数（仅当总量超出容量时）
            let total = self.len + n;
            if total > self.capacity {
                let need_drop = total - self.capacity;
                self.overflow_total += need_drop as u64;
                self.len -= need_drop;
            }
            self.write_at_head(data);
        }
        self.last_write = Instant::now();
    }

    /// 在 head 处写入 data（要求 len + data.len() <= capacity）。
    fn write_at_head(&mut self, data: &[u8]) {
        let n = data.len();
        let first = (self.capacity - self.head).min(n);
        self.data[self.head..self.head + first].copy_from_slice(&data[..first]);
        if first < n {
            self.data[..n - first].copy_from_slice(&data[first..]);
        }
        self.head = (self.head + n) % self.capacity;
        self.len += n;
    }

    fn take_all(&mut self) -> (Vec<u8>, u64) {
        let out = if self.len == 0 {
            Vec::new()
        } else {
            let tail = self.tail();
            let first = (self.capacity - tail).min(self.len);
            let mut out = Vec::with_capacity(self.len);
            out.extend_from_slice(&self.data[tail..tail + first]);
            if first < self.len {
                out.extend_from_slice(&self.data[..self.len - first]);
            }
            out
        };
        self.len = 0;
        (out, self.overflow_total)
    }

    fn clear(&mut self) {
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_push_take() {
        let rb = RingBuf::new(8);
        rb.push(b"AB");
        rb.push(b"CD");
        let (data, overflow) = rb.take_all();
        assert_eq!(data, b"ABCD");
        assert_eq!(overflow, 0);
        // 取走后为空
        let (data2, _) = rb.take_all();
        assert!(data2.is_empty());
    }

    #[test]
    fn wrap_around() {
        let rb = RingBuf::new(4);
        rb.push(b"AB");
        rb.push(b"CD"); // head 回到 0，环形跨越
        rb.push(b"EF"); // 覆盖 AB
        let (data, overflow) = rb.take_all();
        assert_eq!(data, b"CDEF");
        assert_eq!(overflow, 2);
    }

    #[test]
    fn overflow_counting() {
        let rb = RingBuf::new(4);
        rb.push(b"1234");
        rb.push(b"5678"); // 全部覆盖
        let (data, overflow) = rb.take_all();
        assert_eq!(data, b"5678");
        assert_eq!(overflow, 4);

        // 累计语义
        rb.push(b"XY");
        let (data2, overflow2) = rb.take_all();
        assert_eq!(data2, b"XY");
        assert_eq!(overflow2, 4);
    }

    #[test]
    fn single_push_larger_than_capacity() {
        let rb = RingBuf::new(4);
        rb.push(b"abcdefghij"); // 10 > 4：保留最后 4 字节
        let (data, overflow) = rb.take_all();
        assert_eq!(data, b"ghij");
        assert_eq!(overflow, 6);
    }

    #[test]
    fn stats_and_clear() {
        let rb = RingBuf::new(8);
        rb.push(b"hello");
        let (bytes, overflow) = rb.stats();
        assert_eq!(bytes, 5);
        assert_eq!(overflow, 0);
        rb.clear();
        let (bytes, _) = rb.stats();
        assert_eq!(bytes, 0);
        let (data, _) = rb.take_all();
        assert!(data.is_empty());
    }

    #[tokio::test]
    async fn notify_wakes_reader() {
        let rb = RingBuf::new(8);
        let rb2 = rb.clone();
        let t = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                rb2.notified().await;
            })
            .await
            .expect("notify 应在超时前唤醒");
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        rb.push(b"x");
        t.await.unwrap();
    }
}
