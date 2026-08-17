//! 有界环形缓冲：串口上行数据的囤积区。
//!
//! 事件驱动读线程（生产者）持续 push，AI 侧工具调用（消费者）按需 take_all 取走，
//! 或经 find_and_take 做"等待匹配输出"（`uart_expect` 系列）的条件查找与消费。
//! 缓冲有上限；写满后**覆盖最旧数据**（串口调试场景最新数据通常最重要），
//! 并累计 `overflow_total` 计数，使数据缺口可被上层检测而非静默丢失。
//!
//! 并发模型：`RingBuf` 内部用 `std::sync::Mutex` 保护环形区（临界区极短），
//! 写入后通过 `tokio::sync::Notify` 唤醒等待中的读取者。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::Notify;

/// 环形缓冲允许的最大容量，避免外部参数导致进程级 OOM。
pub const MAX_BUFFER_SIZE: usize = 16 * 1024 * 1024;
/// 条件匹配允许的最大 pattern 大小，避免搜索占用过多 CPU。
pub const MAX_PATTERN_SIZE: usize = 64 * 1024;

/// 环形缓冲（线程安全包装）。
#[derive(Debug)]
pub struct RingBuf {
    inner: Mutex<RingBuffer>,
    notify: Notify,
}

impl RingBuf {
    /// 创建指定容量的环形缓冲；容量会限制在 1..=`MAX_BUFFER_SIZE`。
    pub fn new(capacity: usize) -> Arc<Self> {
        let capacity = capacity.clamp(1, MAX_BUFFER_SIZE);
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

    /// 当前未读字节数、累计溢出字节数和单调写入版本。
    ///
    /// 每次非空 [`Self::push`] 都会推进版本；消费者可据此区分调用前已有的历史
    /// 缓冲与调用后新到达的上行数据，而无需丢弃历史内容。
    pub fn stats_with_revision(&self) -> (usize, u64, u64) {
        let inner = self.inner.lock().unwrap();
        (inner.len, inner.overflow_total, inner.write_revision)
    }

    /// 下一个写入字节的单调位置。
    ///
    /// 位置按收到的原始字节数推进，不受消费、清空或覆盖影响。调用方可在一次操作
    /// 开始时记录水位，随后只允许水位之后开始的 pattern 触发匹配。
    pub fn write_position(&self) -> u64 {
        let inner = self.inner.lock().unwrap();
        inner.write_position
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
        self.find_and_take_from(pattern, consume, None)
    }

    /// 在未读区中查找 pattern，可选限制 pattern 的起始字节不得早于单调写入位置
    /// `min_position`。限制只影响匹配候选；`consume=true` 时仍按 FIFO 语义取走
    /// 未读区起点至 pattern 结尾，因此返回内容可能包含水位之前的历史前缀。
    pub fn find_and_take_from(
        &self,
        pattern: &[u8],
        consume: bool,
        min_position: Option<u64>,
    ) -> (Option<usize>, Vec<u8>, u64) {
        let mut inner = self.inner.lock().unwrap();
        let pos = match min_position {
            Some(position) => find_in_from(&inner, pattern, position),
            None => find_in(&inner, pattern),
        };
        let out = match pos {
            Some(p) if consume => take_prefix_inner(&mut inner, p + pattern.len()),
            _ => Vec::new(),
        };
        (pos, out, inner.overflow_total)
    }

    /// 忽略缓冲中的 ANSI 转义/控制序列后查找可见字节 pattern。
    ///
    /// ANSI 处理只影响匹配候选，不修改原始缓冲：`consume=true` 时仍按 FIFO
    /// 语义取走未读区起点至“最后一个匹配可见字节”的全部原始数据，返回内容
    /// 因而保留 ANSI 字节。`min_position` 与 [`Self::find_and_take_from`] 相同，
    /// 可限制 pattern 的首个可见字节不得早于调用时水位。
    pub fn find_and_take_ignoring_ansi(
        &self,
        pattern: &[u8],
        consume: bool,
        min_position: Option<u64>,
    ) -> (Option<usize>, Vec<u8>, u64) {
        let mut inner = self.inner.lock().unwrap();
        let start_offset = min_position.map_or(0, |position| {
            let buffer_start = inner.write_position.saturating_sub(inner.len as u64);
            position.saturating_sub(buffer_start).min(inner.len as u64) as usize
        });
        let matched = find_ignoring_ansi_from_offset(&inner, pattern, start_offset);
        let pos = matched.map(|(start, _)| start);
        let out = match matched {
            Some((_, end)) if consume => take_prefix_inner(&mut inner, end),
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
    /// 每次非空写入推进一次的单调版本（自然回绕时 wrapping 比较仍可判断变化）。
    write_revision: u64,
    /// 下一个写入字节的单调位置；消费、清空和覆盖均不回退。
    write_position: u64,
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
            write_revision: 0,
            write_position: 0,
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
        self.write_revision = self.write_revision.wrapping_add(1);
        // u64 足以覆盖任何现实串口寿命；饱和可避免理论上的整数回绕把新数据
        // 错认成历史数据。
        self.write_position = self.write_position.saturating_add(n as u64);
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
    find_in_from_offset(inner, pattern, 0)
}

/// 从单调写入位置 `min_position` 开始查找。pattern 的首字节必须位于水位之后，
/// 因而不会把历史尾部与新数据头部拼接成一次命中。
fn find_in_from(inner: &RingBuffer, pattern: &[u8], min_position: u64) -> Option<usize> {
    let buffer_start = inner.write_position.saturating_sub(inner.len as u64);
    let start_offset = min_position
        .saturating_sub(buffer_start)
        .min(inner.len as u64) as usize;
    find_in_from_offset(inner, pattern, start_offset)
}

/// 在环形缓冲的逻辑未读区中，从 `start_offset` 开始执行 KMP 查找。
fn find_in_from_offset(inner: &RingBuffer, pattern: &[u8], start_offset: usize) -> Option<usize> {
    let start_offset = start_offset.min(inner.len);
    if pattern.is_empty() {
        return Some(start_offset);
    }
    if pattern.len() > MAX_PATTERN_SIZE || pattern.len() > inner.len.saturating_sub(start_offset) {
        return None;
    }
    let tail = inner.tail();
    let first = (inner.capacity - tail).min(inner.len);
    let (seg1, seg2) = (
        &inner.data[tail..tail + first],
        &inner.data[..inner.len - first],
    );
    let m = pattern.len();

    // KMP 前缀表使跨环形缓冲边界的搜索保持 O(n + m)，避免重复比较退化。
    let mut prefix = vec![0usize; m];
    let mut prefix_len = 0;
    for i in 1..m {
        while prefix_len > 0 && pattern[i] != pattern[prefix_len] {
            prefix_len = prefix[prefix_len - 1];
        }
        if pattern[i] == pattern[prefix_len] {
            prefix_len += 1;
        }
        prefix[i] = prefix_len;
    }

    let byte_at = |idx: usize| {
        if idx < first {
            seg1[idx]
        } else {
            seg2[idx - first]
        }
    };
    let mut matched = 0;
    for pos in start_offset..inner.len {
        let byte = byte_at(pos);
        while matched > 0 && byte != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if byte == pattern[matched] {
            matched += 1;
            if matched == m {
                return Some(pos + 1 - m);
            }
        }
    }
    None
}

/// ANSI 匹配扫描状态。这里处理终端常见的 CSI、OSC 及由 ST 结束的字符串类
/// 控制序列；其它两字节 ESC 序列也作为不可见控制序列跳过。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnsiState {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
    StString,
    StStringEscape,
}

/// 忽略 ANSI 序列后执行 KMP 查找，返回原始缓冲中的 `(起点, 终点开区间)`。
/// 扫描从缓冲起点开始，以便 `start_offset` 落在跨水位 ANSI 序列中时仍能正确
/// 识别状态；只有原始位置不早于水位的可见字节才参与 pattern 匹配。
fn find_ignoring_ansi_from_offset(
    inner: &RingBuffer,
    pattern: &[u8],
    start_offset: usize,
) -> Option<(usize, usize)> {
    let start_offset = start_offset.min(inner.len);
    if pattern.is_empty() {
        return Some((start_offset, start_offset));
    }
    if pattern.len() > MAX_PATTERN_SIZE {
        return None;
    }

    let tail = inner.tail();
    let first = (inner.capacity - tail).min(inner.len);
    let (seg1, seg2) = (
        &inner.data[tail..tail + first],
        &inner.data[..inner.len - first],
    );
    let byte_at = |idx: usize| {
        if idx < first {
            seg1[idx]
        } else {
            seg2[idx - first]
        }
    };

    let m = pattern.len();
    let mut prefix = vec![0usize; m];
    let mut prefix_len = 0;
    for i in 1..m {
        while prefix_len > 0 && pattern[i] != pattern[prefix_len] {
            prefix_len = prefix[prefix_len - 1];
        }
        if pattern[i] == pattern[prefix_len] {
            prefix_len += 1;
        }
        prefix[i] = prefix_len;
    }

    let mut state = AnsiState::Ground;
    let mut matched = 0usize;
    let mut recent_positions = VecDeque::with_capacity(m);
    for pos in 0..inner.len {
        let byte = byte_at(pos);
        let visible = match state {
            AnsiState::Ground => match byte {
                0x1B => {
                    state = AnsiState::Escape;
                    None
                }
                0x9B => {
                    state = AnsiState::Csi;
                    None
                }
                0x9D => {
                    state = AnsiState::Osc;
                    None
                }
                0x90 | 0x98 | 0x9E | 0x9F => {
                    state = AnsiState::StString;
                    None
                }
                0x80..=0x9F => None,
                _ => Some(byte),
            },
            AnsiState::Escape => {
                state = match byte {
                    b'[' => AnsiState::Csi,
                    b']' => AnsiState::Osc,
                    b'P' | b'X' | b'^' | b'_' => AnsiState::StString,
                    0x1B => AnsiState::Escape,
                    0x20..=0x2F => AnsiState::EscapeIntermediate,
                    _ => AnsiState::Ground,
                };
                None
            }
            AnsiState::EscapeIntermediate => {
                state = match byte {
                    0x1B => AnsiState::Escape,
                    0x30..=0x7E => AnsiState::Ground,
                    _ => AnsiState::EscapeIntermediate,
                };
                None
            }
            AnsiState::Csi => {
                state = match byte {
                    0x1B => AnsiState::Escape,
                    0x40..=0x7E => AnsiState::Ground,
                    _ => AnsiState::Csi,
                };
                None
            }
            AnsiState::Osc => {
                state = match byte {
                    0x07 | 0x9C => AnsiState::Ground,
                    0x1B => AnsiState::OscEscape,
                    _ => AnsiState::Osc,
                };
                None
            }
            AnsiState::OscEscape => {
                state = match byte {
                    b'\\' => AnsiState::Ground,
                    0x1B => AnsiState::OscEscape,
                    _ => AnsiState::Osc,
                };
                None
            }
            AnsiState::StString => {
                state = match byte {
                    0x9C => AnsiState::Ground,
                    0x1B => AnsiState::StStringEscape,
                    _ => AnsiState::StString,
                };
                None
            }
            AnsiState::StStringEscape => {
                state = match byte {
                    b'\\' => AnsiState::Ground,
                    0x1B => AnsiState::StStringEscape,
                    _ => AnsiState::StString,
                };
                None
            }
        };

        let Some(byte) = visible.filter(|_| pos >= start_offset) else {
            continue;
        };
        recent_positions.push_back(pos);
        if recent_positions.len() > m {
            recent_positions.pop_front();
        }
        while matched > 0 && byte != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if byte == pattern[matched] {
            matched += 1;
            if matched == m {
                return Some((*recent_positions.front().unwrap(), pos + 1));
            }
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
        assert_eq!(rb.stats_with_revision(), (0, 0, 0));
        rb.push(b"AB");
        assert_eq!(rb.stats_with_revision(), (2, 0, 1));
        rb.push(b"CD");
        assert_eq!(rb.stats_with_revision(), (4, 0, 2));
        let (data, overflow) = rb.take_all();
        assert_eq!(data, b"ABCD");
        assert_eq!(overflow, 0);
        // 取走后为空
        let (data2, _) = rb.take_all();
        assert!(data2.is_empty());
        // 消费不推进写入版本，便于 exchange 区分历史数据和新上行数据。
        assert_eq!(rb.stats_with_revision(), (0, 0, 2));
    }

    #[test]
    fn empty_push_does_not_advance_revision() {
        let rb = RingBuf::new(8);
        rb.push(b"");
        assert_eq!(rb.stats_with_revision(), (0, 0, 0));
        assert_eq!(rb.write_position(), 0);
        rb.push(b"x");
        assert_eq!(rb.stats_with_revision(), (1, 0, 1));
        assert_eq!(rb.write_position(), 1);
        rb.clear();
        assert_eq!(rb.stats_with_revision(), (0, 0, 1));
        assert_eq!(rb.write_position(), 1);
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
    fn find_from_position_ignores_history_and_cross_boundary_match() {
        let rb = RingBuf::new(32);
        rb.push(b"OLD-MARK|AB");
        let watermark = rb.write_position();

        // 历史中的完整 pattern 不得命中；边界两侧的 AB + CD 也不得拼接命中。
        let (pos, data, _) = rb.find_and_take_from(b"OLD-MARK", true, Some(watermark));
        assert_eq!(pos, None);
        assert!(data.is_empty());
        rb.push(b"CD|NEW-MARK");
        let (pos, data, _) = rb.find_and_take_from(b"ABCD", true, Some(watermark));
        assert_eq!(pos, None);
        assert!(data.is_empty());

        // 新数据中的 pattern 可以命中；FIFO 消费仍返回水位之前的历史前缀。
        let (pos, data, _) = rb.find_and_take_from(b"NEW-MARK", true, Some(watermark));
        assert_eq!(pos, Some(14));
        assert_eq!(data, b"OLD-MARK|ABCD|NEW-MARK");
    }

    #[test]
    fn find_from_position_survives_overflow_and_clear() {
        let rb = RingBuf::new(8);
        rb.push(b"history");
        let watermark = rb.write_position();
        rb.push(b"xxTARGET"); // 历史和新数据前缀被覆盖，只保留 xxTARGET。
        let (pos, _, overflow) = rb.find_and_take_from(b"TARGET", false, Some(watermark));
        assert_eq!(pos, Some(2));
        assert_eq!(overflow, 7);

        rb.clear();
        let watermark = rb.write_position();
        rb.push(b"TARGET");
        let (pos, data, _) = rb.find_and_take_from(b"TARGET", true, Some(watermark));
        assert_eq!(pos, Some(0));
        assert_eq!(data, b"TARGET");
    }

    #[test]
    fn find_ignoring_ansi_matches_visible_text_and_preserves_raw_bytes() {
        let rb = RingBuf::new(128);
        let raw = b"\x1b[31mAB\x1b[0m\x1b[32mCD\x1b[0m\n";
        rb.push(raw);

        // 原始字节匹配保持默认行为，不会跨颜色控制序列拼接。
        assert_eq!(rb.find(b"ABCD"), None);
        let (pos, data, overflow) = rb.find_and_take_ignoring_ansi(b"ABCD", true, None);
        assert_eq!(pos, Some(5));
        assert_eq!(data, b"\x1b[31mAB\x1b[0m\x1b[32mCD");
        assert_eq!(overflow, 0);
        // pattern 末字节之后的 reset/newline 仍按 FIFO 语义留给后续读取。
        assert_eq!(rb.take_all().0, b"\x1b[0m\n");
    }

    #[test]
    fn find_ignoring_ansi_skips_osc_and_st_strings() {
        let rb = RingBuf::new(256);
        rb.push(b"A\x1b]0;window-title\x07B\x1bPprivate-data\x1b\\C");
        let (pos, data, _) = rb.find_and_take_ignoring_ansi(b"ABC", true, None);
        assert_eq!(pos, Some(0));
        assert_eq!(data, b"A\x1b]0;window-title\x07B\x1bPprivate-data\x1b\\C");
    }

    #[test]
    fn find_ignoring_ansi_across_ring_wrap() {
        let rb = RingBuf::new(16);
        rb.push(b"0123456789");
        rb.take_all(); // 让后续 ANSI 数据跨物理环形边界。
        rb.push(b"\x1b[31mAB\x1b[0mCD");
        let (pos, data, _) = rb.find_and_take_ignoring_ansi(b"ABCD", true, None);
        assert_eq!(pos, Some(5));
        assert_eq!(data, b"\x1b[31mAB\x1b[0mCD");
    }

    #[test]
    fn find_ignoring_ansi_respects_new_data_watermark() {
        let rb = RingBuf::new(128);
        rb.push(b"\x1b[31mAB\x1b[0m");
        let watermark = rb.write_position();
        rb.push(b"\x1b[32mCD\x1b[0m");

        // `new` 不允许历史 AB 与新 CD 跨水位组成一次可见匹配。
        let (pos, data, _) = rb.find_and_take_ignoring_ansi(b"ABCD", true, Some(watermark));
        assert_eq!(pos, None);
        assert!(data.is_empty());

        // 新数据自己的 pattern 可命中，消费仍包含历史原始前缀。
        let (pos, data, _) = rb.find_and_take_ignoring_ansi(b"CD", true, Some(watermark));
        assert_eq!(pos, Some(16));
        assert_eq!(data, b"\x1b[31mAB\x1b[0m\x1b[32mCD");
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
