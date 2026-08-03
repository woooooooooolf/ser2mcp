//! 二进制数据与 hex 字符串的编解码。
//!
//! MCP 的 JSON-RPC 通道只保证 UTF-8 文本，因此串口二进制数据一律以
//! hex 字符串形式在工具参数/返回值中传递（如 `"41 54 0D 0A"`），
//! 文本模式作为便捷选项。

/// 将字节序列编码为大写、空格分隔的 hex 字符串。
///
/// ```
/// assert_eq!(ser2mcp::hex::encode(b"AT\r\n"), "41 54 0D 0A");
/// assert_eq!(ser2mcp::hex::encode(&[]), "");
/// ```
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&format!("{b:02X}"));
    }
    out
}

/// 将 hex 字符串解码为字节序列（宽松解析）。
///
/// 接受以下形式（可混用）：
/// - 空格/逗号/分号/换行分隔的字节组：`"41 54"`、`"41,54"`、`"41;54"`
/// - 连续 hex 串（偶数长度）：`"4154"`、`"41540d0a"`
/// - 带 `0x` 前缀的字节组：`"0x41 0x54"`
///
/// 解析失败返回描述性错误。
///
/// ```
/// assert_eq!(ser2mcp::hex::decode("41 54 0D 0A").unwrap(), b"AT\r\n");
/// assert_eq!(ser2mcp::hex::decode("41540d0a").unwrap(), b"AT\r\n");
/// assert_eq!(ser2mcp::hex::decode("0x41,0x54").unwrap(), b"AT");
/// assert!(ser2mcp::hex::decode("41 5").is_err());
/// assert!(ser2mcp::hex::decode("zz").is_err());
/// ```
pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    let mut cleaned = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            // 跳过分隔符与空白
            ' ' | '\t' | '\r' | '\n' | ',' | ';' => {
                chars.next();
            }
            // 跳过 0x / 0X 前缀
            '0' if matches!(chars.clone().nth(1), Some('x') | Some('X')) => {
                chars.next();
                chars.next();
            }
            c if c.is_ascii_hexdigit() => {
                cleaned.push(c.to_ascii_uppercase());
                chars.next();
            }
            other => {
                return Err(format!(
                    "非法 hex 字符: {other:?}（仅允许 0-9 A-F a-f 及分隔符）"
                ));
            }
        }
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err(format!(
            "hex 字符串长度必须为偶数（当前有效字符数 {}）",
            cleaned.len()
        ));
    }
    let mut out = Vec::with_capacity(cleaned.len() / 2);
    let bytes = cleaned.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = hex_val(bytes[i]);
        let lo = hex_val(bytes[i + 1]);
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'A'..=b'F' => b - b'A' + 10,
        _ => unreachable!("已在 decode 中过滤"),
    }
}

/// 判断字节序列是否为安全可打印文本（用于 text 模式返回）。
///
/// 全部字节位于可打印 ASCII（0x20..=0x7E）或合法 UTF-8 多字节序列时视为文本。
pub fn is_text(bytes: &[u8]) -> bool {
    match std::str::from_utf8(bytes) {
        Ok(s) => s
            .chars()
            .all(|c| !c.is_control() || c == '\n' || c == '\r' || c == '\t'),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_basic() {
        assert_eq!(encode(b"AT\r\n"), "41 54 0D 0A");
        assert_eq!(encode(b""), "");
        assert_eq!(encode(&[0x00, 0xFF, 0x10]), "00 FF 10");
    }

    #[test]
    fn decode_various_forms() {
        assert_eq!(decode("41 54 0D 0A").unwrap(), b"AT\r\n");
        assert_eq!(decode("41540d0a").unwrap(), b"AT\r\n");
        assert_eq!(decode("41,54").unwrap(), b"AT");
        assert_eq!(decode("41;54").unwrap(), b"AT");
        assert_eq!(decode("0x41 0x54").unwrap(), b"AT");
        assert_eq!(decode(" 41\t54\n0D ").unwrap(), b"AT\r");
        assert_eq!(decode("").unwrap(), b"");
        // 大小写混用
        assert_eq!(decode("4a bC dE").unwrap(), b"J\xbc\xde");
    }

    #[test]
    fn decode_invalid() {
        assert!(decode("41 5").is_err(), "奇数长度应报错");
        assert!(decode("zz").is_err(), "非法字符应报错");
        assert!(decode("0x4").is_err());
        assert!(decode("41 5G").is_err());
    }

    #[test]
    fn roundtrip() {
        let data: Vec<u8> = (0..=255u8).collect();
        assert_eq!(decode(&encode(&data)).unwrap(), data);
    }

    #[test]
    fn text_detection() {
        assert!(is_text(b"AT+OK\r\n"));
        assert!(is_text("中文测试".as_bytes()));
        assert!(!is_text(&[0x00, 0x01, 0x02]));
        assert!(!is_text(&[0xFF, 0xFE]));
    }
}
