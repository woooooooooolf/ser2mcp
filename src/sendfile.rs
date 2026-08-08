//! 文件流式发送：分块读取 + 可选 base64 编码（每 76 字符换行）+ 耗时估算。
//!
//! 设计原则（透明原则）：只负责"把文件字节发出去"，不解析数据格式、
//! 不做 CRC/固件协议、不主动发 EOF——需要时由模型在宿主侧或对话内组织。

use std::fs::File;
use std::io::Read;

use base64::{Engine as _, engine::general_purpose::STANDARD};

/// base64 每行字符数（MIME 惯例；对端 `cat > file` 在 icanon 模式下按行读）。
pub const BASE64_LINE_WIDTH: usize = 76;
/// 默认分片大小（字节）：保守默认值（宁小勿大）。
/// 模型应依据对端 tty 缓冲限制与波特率显式覆盖（见工具描述）。
pub const DEFAULT_CHUNK_SIZE: usize = 256;

/// 发送编码模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    /// 原样按字节发送（默认）。
    Text,
    /// 服务器 base64 编码后发送，每 76 字符自动插入 `\n`，文件末尾补 `\n`。
    Base64,
}

impl SendMode {
    /// 解析编码模式字符串（text / base64，大小写不敏感）。
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "base64" => Ok(Self::Base64),
            other => Err(format!("mode 必须是 text 或 base64，收到 {other:?}")),
        }
    }

    /// 模式的规范化名称（text / base64）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Base64 => "base64",
        }
    }
}

/// 耗时/字节数估算结果（`uart_send_estimate` 返回值）。
#[derive(Debug, serde::Serialize)]
pub struct Estimate {
    /// 原始文件字节数。
    pub file_size: u64,
    /// 编码模式（text/base64）。
    pub mode: String,
    /// 分片大小（原始字节）。
    pub chunk_size: usize,
    /// 片间间隔（毫秒）。
    pub gap_ms: u64,
    /// 波特率。
    pub baudrate: u32,
    /// 预计写入串口的字节数（base64 模式含换行）。
    pub est_sent_bytes: u64,
    /// 预计发送片数。
    pub est_chunks: u64,
    /// 预计耗时（毫秒）。
    pub est_time_ms: u64,
    /// 估算公式说明。
    pub formula: String,
}

/// 按波特率估算发送耗时与字节数（8N1：每字节 10 bit；未计片间 flush 开销，
/// 实际耗时通常略高于估算值）。
pub fn estimate(
    mode: SendMode,
    file_size: u64,
    chunk_size: usize,
    gap_ms: u64,
    baudrate: u32,
) -> Estimate {
    let chunks = file_size.div_ceil(chunk_size.max(1) as u64);
    let (sent_bytes, formula) = match mode {
        SendMode::Text => (
            file_size,
            "text: sent = file_size; time = sent × 10 / baudrate + chunks × gap_ms",
        ),
        SendMode::Base64 => {
            let enc = file_size.div_ceil(3) * 4;
            let newlines = enc.div_ceil(BASE64_LINE_WIDTH as u64);
            (
                enc + newlines,
                "base64: sent = ceil(file_size/3)×4 + 换行数; time = sent × 10 / baudrate + chunks × gap_ms",
            )
        }
    };
    let time_ms = sent_bytes * 10 * 1000 / (baudrate.max(1) as u64) + chunks * gap_ms;
    Estimate {
        file_size,
        mode: mode.as_str().into(),
        chunk_size,
        gap_ms,
        baudrate,
        est_sent_bytes: sent_bytes,
        est_chunks: chunks,
        est_time_ms: time_ms,
        formula: formula.into(),
    }
}

/// 分片迭代器：从文件流式读取并编码。
///
/// - `Text` 模式：每片为 `chunk_size` 原始字节（EOF 短片除外）。
/// - `Base64` 模式：每片为 `chunk_size` 原始字节编码后的 base64 文本，按
///   `BASE64_LINE_WIDTH` 行宽跨片连续断行（`\n`），文件末尾补 `\n` 保证
///   对端 icanon 行读能收到最后一行。
///
/// 读取文件失败时产出 `Err` 后终止（`None`），错误信息含路径。
pub struct ChunkIter {
    file: File,
    mode: SendMode,
    chunk_size: usize,
    /// base64 模式下当前行已输出的字符数（跨片连续）。
    line_pos: usize,
    /// base64 模式下尚未凑满 3 字节的尾部，跨原始文件分片保留。
    base64_tail: Vec<u8>,
    buf: Vec<u8>,
    done: bool,
}

impl ChunkIter {
    /// 创建分片迭代器（`chunk_size` 最小为 1）。
    pub fn new(file: File, mode: SendMode, chunk_size: usize) -> Self {
        Self {
            file,
            mode,
            chunk_size: chunk_size.max(1),
            line_pos: 0,
            base64_tail: Vec::with_capacity(2),
            buf: Vec::with_capacity(chunk_size),
            done: false,
        }
    }

    /// 读满 `chunk_size` 字节；EOF 时返回已读部分，全空（文件读完）返回 `None`。
    fn read_full(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.buf.clear();
        let mut tmp = [0u8; 4096];
        loop {
            if self.buf.len() >= self.chunk_size {
                break;
            }
            let want = (self.chunk_size - self.buf.len()).min(tmp.len());
            let n = self
                .file
                .read(&mut tmp[..want])
                .map_err(|e| format!("读取文件失败: {e}"))?;
            if n == 0 {
                break; // EOF
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
        if self.buf.is_empty() {
            Ok(None)
        } else {
            Ok(Some(std::mem::take(&mut self.buf)))
        }
    }

    /// 对不含 padding 的完整 base64 输入编码并按行宽断行。
    fn wrap_base64(&mut self, enc: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(enc.len() + enc.len() / BASE64_LINE_WIDTH + 1);
        for b in enc.bytes() {
            if self.line_pos >= BASE64_LINE_WIDTH {
                out.push(b'\n');
                self.line_pos = 0;
            }
            out.push(b);
            self.line_pos += 1;
        }
        out
    }

    /// base64 编码一个原始分片，同时把 1-2 个尾部字节保留到下一分片。
    /// 这样 padding 只会出现在整个文件的末尾，而不会出现在分片中间。
    fn encode_chunk(&mut self, raw: &[u8]) -> Vec<u8> {
        let mut input = std::mem::take(&mut self.base64_tail);
        input.extend_from_slice(raw);
        let complete_len = input.len() / 3 * 3;
        self.base64_tail = input[complete_len..].to_vec();
        if complete_len == 0 {
            return Vec::new();
        }
        let enc = STANDARD.encode(&input[..complete_len]);
        self.wrap_base64(&enc)
    }

    /// 编码文件尾部并补齐最后一行换行。
    fn finish_base64(&mut self) -> Vec<u8> {
        let mut out = if self.base64_tail.is_empty() {
            Vec::new()
        } else {
            let tail = std::mem::take(&mut self.base64_tail);
            let enc = STANDARD.encode(tail);
            self.wrap_base64(&enc)
        };
        if self.line_pos > 0 {
            out.push(b'\n');
            self.line_pos = 0;
        }
        out
    }
}

impl Iterator for ChunkIter {
    type Item = Result<Vec<u8>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            let raw = match self.read_full() {
                Ok(Some(v)) => v,
                Ok(None) => {
                    self.done = true;
                    return match self.mode {
                        SendMode::Text => None,
                        SendMode::Base64 => {
                            let tail = self.finish_base64();
                            (!tail.is_empty()).then_some(Ok(tail))
                        }
                    };
                }
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            };
            let chunk = match self.mode {
                SendMode::Text => raw,
                SendMode::Base64 => self.encode_chunk(&raw),
            };
            if !chunk.is_empty() {
                return Some(Ok(chunk));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 进程内唯一计数器：并行测试在同一纳秒创建临时文件时避免命名冲突
    /// （as_nanos 命名在并行下可能碰撞，导致 create(true) 覆盖损坏数据）。
    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    /// 创建临时文件（写入数据后指针回到开头），返回 (File, path) 供测试后清理。
    fn tempfile_with(data: &[u8]) -> (File, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "ser2mcp-sendfile-test-{}-{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let mut f = std::fs::File::options()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        f.seek(std::io::SeekFrom::Start(0)).unwrap();
        (f, path)
    }

    fn chunks_of(data: &[u8], mode: SendMode, chunk_size: usize) -> Vec<Vec<u8>> {
        let (f, path) = tempfile_with(data);
        let out: Vec<Vec<u8>> = ChunkIter::new(f, mode, chunk_size)
            .map(|c| c.unwrap())
            .collect();
        let _ = std::fs::remove_file(&path);
        out
    }

    #[test]
    fn text_chunks_split_by_size() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let chunks = chunks_of(&data, SendMode::Text, 256);
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].len(), 256);
        assert_eq!(chunks[3].len(), 1000 - 3 * 256);
        let joined: Vec<u8> = chunks.into_iter().flatten().collect();
        assert_eq!(joined, data);
    }

    #[test]
    fn base64_roundtrip_with_newlines() {
        // 覆盖：非 3 倍数长度 + 跨行边界（76 字符 = 57 原始字节）。
        let data: Vec<u8> = (0..500u32).map(|i| (i % 251) as u8).collect();
        let out: Vec<u8> = chunks_of(&data, SendMode::Base64, 57)
            .into_iter()
            .flatten()
            .collect();
        let s = String::from_utf8(out.clone()).unwrap();
        // 每行 ≤ 76 字符，以 \n 结尾。
        assert!(s.ends_with('\n'));
        for line in s.lines() {
            assert!(line.len() <= BASE64_LINE_WIDTH);
        }
        // 解码（去掉换行后）与原始一致。
        let stripped: Vec<u8> = out.into_iter().filter(|&b| b != b'\n').collect();
        let decoded = STANDARD.decode(stripped).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn base64_roundtrip_with_non_multiple_of_three_chunks() {
        // 默认 chunk_size=256 不是 3 的倍数，padding 只能出现在整个文件末尾。
        let data: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
        let out: Vec<u8> = chunks_of(&data, SendMode::Base64, 256)
            .into_iter()
            .flatten()
            .collect();
        assert!(out.ends_with(b"\n"));
        let stripped: Vec<u8> = out.into_iter().filter(|&b| b != b'\n').collect();
        let decoded = STANDARD.decode(stripped).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn base64_line_width_respected_across_chunks() {
        // chunk_size=76 原始字节 → 编码 104 字符：第一片 76 字符行满插 \n，余 28 字符续行。
        let data = vec![0x41u8; 76];
        let chunks = chunks_of(&data, SendMode::Base64, 76);
        assert_eq!(chunks.len(), 2); // 编码块 + 末尾补 \n 片
        let out: Vec<u8> = chunks.into_iter().flatten().collect();
        let s = String::from_utf8(out).unwrap();
        assert!(s.ends_with('\n'));
        assert_eq!(s.matches('\n').count(), 2); // 行满换行 + 末尾补换行
        let stripped: Vec<u8> = s.bytes().filter(|&b| b != b'\n').collect();
        let decoded = STANDARD.decode(stripped).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn empty_file_produces_nothing() {
        let (f, path) = tempfile_with(b"");
        assert_eq!(ChunkIter::new(f, SendMode::Base64, 256).count(), 0);
        let (f, path2) = tempfile_with(b"");
        assert_eq!(ChunkIter::new(f, SendMode::Text, 256).count(), 0);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&path2);
    }

    #[test]
    fn estimate_formula_sane() {
        let e = estimate(SendMode::Text, 1152, 256, 0, 115200);
        assert_eq!(e.est_sent_bytes, 1152);
        assert_eq!(e.est_chunks, 5);
        // 1152 字节 × 10 bit / 115200 = 0.1s = 100ms
        assert_eq!(e.est_time_ms, 100);
        let e = estimate(SendMode::Base64, 3, 256, 0, 115200);
        // 3 字节 → 4 字符 base64，1 行换行
        assert_eq!(e.est_sent_bytes, 5);
        assert_eq!(e.est_time_ms, 0); // 5*10/115200*1000 = 0.43 → 0
    }
}
