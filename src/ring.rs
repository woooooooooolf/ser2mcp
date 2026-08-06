//! 有界环形缓冲：串口上行数据的囤积区。
//!
//! 事件驱动读线程（生产者）持续 push，AI 侧工具调用（消费者）按需 take_all。
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

    /// 在未读区中查找 pattern 首次出现的位置（相对未读区起点的偏移），未命中返回 `None`。
    ///
    /// 用于 `uart_expect` 的内容匹配：跨多次 `push` 分片到达的 pattern
    /// （读线程单次 `read()` 可能只搬入几个字节）也能命中；环形 wrap 由内部展开处理。
    /// 空 pattern 定义为命中在偏移 0（调用方应先行拦截空模式）。
    ///
    /// 生产路径使用原子的 [`Self::find_and_take`]（查找与消费在同一临界区内）；
    /// 本方法为独立的只读查找 API，供测试与组合使用。
    #[allow(dead_code)]
    pub fn find(&self, pattern: &[u8]) -> Option<usize> {
        let inner = self.inner.lock().unwrap();
        find_in(&inner, pattern)
    }

    /// 在未读区中查找 pattern；命中且 `consume=true` 时在同一临界区内原子取走
    /// "截至 pattern 结尾"的内容（读线程无法在查找与取走之间插入覆盖）。
    ///
    /// 返回 `(命中偏移, 取走的数据, 累计溢出计数)`；未命中时取走的数据为空。
    pub fn find_and_take(&self, pattern: &[u8], consume: bool) -> (Option<usize>, Vec<u8>, u64) {
        let mut inner = self.inner.lock().unwrap();
        let pos = find_in(&inner, pattern);
        let out = match pos {
            Some(p) if consume => take_prefix_inner(&mut inner, p + pattern.len()),
            _ => Vec::new(),
        };
        (pos, out, inner.overflow_total)
    }

    /// 取走未读区前 n 字节（超出当前未读长度时取走全部），返回数据与**累计**溢出字节数。
    ///
    /// 供 `uart_expect` 的 `consume=true` 语义使用：命中后取走"截至 pattern 结尾"的内容，
    /// pattern 之后的字节保留在缓冲中，后续 `uart_read` 仍可取走。
    ///
    /// 生产路径使用原子的 [`Self::find_and_take`]；本方法为独立的消费 API，供测试与组合使用。
    #[allow(dead_code)]
    pub fn take_prefix(&self, n: usize) -> (Vec<u8>, u64) {
        let mut inner = self.inner.lock().unwrap();
        let out = take_prefix_inner(&mut inner, n);
        (out, inner.overflow_total)
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

/// 在环形缓冲未读区中查找 pattern 首次出现的位置（相对未读区起点的偏移）。
/// 空 pattern 返回 `Some(0)`；未命中返回 `None`。调用方需已持有锁。
fn find_in(inner: &RingBuffer, pattern: &[u8]) -> Option<usize> {
    if pattern.is_empty() {
        return Some(0);
    }
    if pattern.len() > inner.len {
        return None;
    }
    let tail = inner.tail();
    let first = (inner.capacity - tail).min(inner.len);
    let (seg1, seg2) = (
        &inner.data[tail..tail + first],
        &inner.data[..inner.len - first],
    );
    let m = pattern.len();
    for start in 0..=inner.len - m {
        let mut ok = true;
        for (k, &pb) in pattern.iter().enumerate() {
            let idx = start + k;
            let b = if idx < first {
                seg1[idx]
            } else {
                seg2[idx - first]
            };
            if b != pb {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(start);
        }
    }
    None
}

/// 取走未读区前 n 字节（超出当前未读长度时取走全部），返回数据。调用方需已持有锁。
fn take_prefix_inner(inner: &mut RingBuffer, n: usize) -> Vec<u8> {
    let n = n.min(inner.len);
    let tail = inner.tail();
    let first = (inner.capacity - tail).min(n);
    let mut out = Vec::with_capacity(n);
    out.extend_from_slice(&inner.data[tail..tail + first]);
    if first < n {
        out.extend_from_slice(&inner.data[..n - first]);
    }
    inner.len -= n;
    out
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

    #[test]
    fn find_and_take_atomic_consume() {
        let rb = RingBuf::new(64);
        rb.push(b"U-Boot 2024.01\nZynq> ");
        // consume=true：命中并取走"截至 pattern 结尾"的内容
        let (pos, data, overflow) = rb.find_and_take(b"Zynq> ", true);
        assert_eq!(pos, Some(15));
        assert_eq!(data, b"U-Boot 2024.01\nZynq> ");
        assert_eq!(overflow, 0);
        // pattern 之后的剩余数据仍可读
        rb.push(b"version\n");
        let (rest, _) = rb.take_all();
        assert_eq!(rest, b"version\n");
        // consume=false：命中但不消费
        rb.push(b"Hit any key to stop");
        let (pos2, data2, _) = rb.find_and_take(b"any key", false);
        assert_eq!(pos2, Some(4));
        assert!(data2.is_empty());
        let (rest2, _) = rb.take_all();
        assert_eq!(rest2, b"Hit any key to stop");
        // 未命中：数据不消费
        let (pos3, data3, _) = rb.find_and_take(b"Zynq> ", true);
        assert_eq!(pos3, None);
        assert!(data3.is_empty());
        // 跨 wrap 的原子消费
        let rb2 = RingBuf::new(8);
        rb2.push(b"ABCD");
        rb2.push(b"EFGH");
        rb2.take_prefix(4); // 剩 EFGH？见下：head=0 len=8 → 取走 ABCD，head=0 len=4
        rb2.push(b"XY"); // 逻辑 EFGHXY（跨 wrap）
        let (pos4, data4, _) = rb2.find_and_take(b"GHX", true);
        assert_eq!(pos4, Some(2));
        assert_eq!(data4, b"EFGHX");
        let (rest3, _) = rb2.take_all();
        assert_eq!(rest3, b"Y");
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

    #[test]
    fn find_basic() {
        let rb = RingBuf::new(16);
        // 空缓冲
        assert_eq!(rb.find(b"AB"), None);
        rb.push(b"hello world");
        assert_eq!(rb.find(b"hello"), Some(0));
        assert_eq!(rb.find(b"world"), Some(6));
        assert_eq!(rb.find(b"lo wo"), Some(3));
        // 未命中
        assert_eq!(rb.find(b"zzz"), None);
        // pattern 比未读区长
        assert_eq!(rb.find(b"hello world!"), None);
        // 空 pattern：定义为命中偏移 0
        assert_eq!(rb.find(b""), Some(0));
    }

    #[test]
    fn find_crosses_push_boundary() {
        // pattern 分两次 push 到达（模拟读线程分批搬入）
        let rb = RingBuf::new(64);
        rb.push(b"Hit any key");
        rb.push(b" to stop autoboot");
        assert_eq!(rb.find(b"key to"), Some(8));
        // pattern 完全由第二次 push 提供
        assert_eq!(rb.find(b"autoboot"), Some(20));
        // 跨两次 push 且 pattern 较长
        rb.push(b"\r\nZynq> ");
        assert_eq!(rb.find(b"autoboot\r\nZynq"), Some(20));
    }

    #[test]
    fn find_across_wrap() {
        let rb = RingBuf::new(8);
        rb.push(b"ABCD"); // head=4 len=4
        rb.push(b"EFGH"); // head=0 len=8（满）
        rb.take_prefix(4); // 取走 ABCD → head=0 len=4
        rb.push(b"XY"); // head=2 len=6：物理 data[0..2]=XY + data[4..8]=EFGH
        // 逻辑序跨 wrap：EFGH(4..8) + XY(0..2) = "EFGHXY"
        assert_eq!(rb.find(b"EFGH"), Some(0));
        assert_eq!(rb.find(b"XY"), Some(4));
        assert_eq!(rb.find(b"GHX"), Some(2)); // pattern 跨 wrap 边界
        assert_eq!(rb.find(b"AB"), None);
        // 跨 wrap 且跨 push 边界
        rb.push(b"Z"); // head=3 len=7：逻辑 EFGHXYZ
        assert_eq!(rb.find(b"XYZ"), Some(4));
        assert_eq!(rb.find(b"HXY"), Some(3));
    }

    #[test]
    fn find_after_overflow_eviction() {
        let rb = RingBuf::new(4);
        rb.push(b"1234");
        rb.push(b"5678"); // 全部覆盖
        assert_eq!(rb.find(b"12"), None);
        assert_eq!(rb.find(b"5678"), Some(0));
    }

    #[test]
    fn take_prefix_basic() {
        let rb = RingBuf::new(16);
        rb.push(b"hello world");
        // 取走 0 字节
        let (empty, overflow) = rb.take_prefix(0);
        assert!(empty.is_empty());
        assert_eq!(overflow, 0);
        // 取走部分（截至 pattern 结尾）
        let (data, overflow) = rb.take_prefix(5);
        assert_eq!(data, b"hello");
        assert_eq!(overflow, 0);
        // 剩余数据仍在缓冲，且偏移相对新起点（剩余为 " world"）
        assert_eq!(rb.find(b"world"), Some(1));
        let (rest, _) = rb.take_all();
        assert_eq!(rest, b" world");
        // 取走超过未读长度 → 取走全部
        let rb2 = RingBuf::new(8);
        rb2.push(b"AB");
        let (all, _) = rb2.take_prefix(100);
        assert_eq!(all, b"AB");
    }

    #[test]
    fn take_prefix_across_wrap() {
        let rb = RingBuf::new(8);
        rb.push(b"ABCD"); // head=4 len=4
        rb.push(b"EFGH"); // head=0 len=8（满）
        rb.take_prefix(4); // 取走 ABCD → head=0 len=4
        rb.push(b"XY"); // head=2 len=6：物理 data[0..2]=XY + data[4..8]=EFGH
        // 逻辑序 wrap：EFGH(4..8) + XY(0..2)
        let (data, _) = rb.take_prefix(5); // 取走 EFGHX，跨 wrap
        assert_eq!(data, b"EFGHX");
        let (rest, _) = rb.take_all();
        assert_eq!(rest, b"Y");
        // 取走全部（n >= len）也应正确（上一段已消费 "Y"）
        rb.push(b"AB"); // head=2→write 2 字节，head=4 len=2
        let (all, _) = rb.take_prefix(100);
        assert_eq!(all, b"AB");
    }
}
