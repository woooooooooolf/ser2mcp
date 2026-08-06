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
    if cleaned.len() % 2 != 0 {
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

/// 将字节序列编码为"文本为主、非文本字节转义"的字符串（text-escaped 模式返回）。
///
/// 与 [`is_text`] 的"全有或全无"降级不同，本函数对每个字节独立处理，
/// 整段数据中只要包含可读文本就保留文本，仅将无法安全展示的字节转义：
/// - 合法 UTF-8 的可打印字符（含空格）原样保留；
/// - `\r`、`\n`、`\t` 原样保留（维持行结构，日志/终端输出天然可读）；
/// - 字面反斜杠 `\` 转义为 `\\`（保证转义结果无歧义、可逆）；
/// - 其余控制字符（如 ESC `0x1B` 的 ANSI 颜色码、`0x00` 等）与非法 UTF-8
///   字节转义为 `\xNN`（大写十六进制，如 `\x1B`）。
///
/// 输出始终是合法文本，不会降级为 hex；如需精确原始字节请用 [`encode`]。
///
/// ```
/// assert_eq!(
///     ser2mcp::hex::encode_escaped(b"\x1b[31mBoot ok\x1b[0m\r\n"),
///     "\\x1B[31mBoot ok\\x1B[0m\r\n"
/// );
/// assert_eq!(ser2mcp::hex::encode_escaped("中文".as_bytes()), "中文");
/// assert_eq!(ser2mcp::hex::encode_escaped(b"a\\b\x00c"), "a\\\\b\\x00c");
/// ```
pub fn encode_escaped(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            out.push_str("\\\\");
            i += 1;
        } else if matches!(b, b'\r' | b'\n' | b'\t') {
            out.push(b as char);
            i += 1;
        } else if b < 0x80 {
            if b.is_ascii_control() {
                push_hex_escape(&mut out, b);
            } else {
                out.push(b as char);
            }
            i += 1;
        } else {
            // 多字节 UTF-8：解码一个完整字符
            match std::str::from_utf8(&bytes[i..]) {
                Ok(s) => {
                    let ch = s.chars().next().expect("非空切片必有字符");
                    if ch.is_control() {
                        // C1 控制符（U+0080..U+009F）等：逐字节转义
                        for &cb in ch.encode_utf8(&mut [0u8; 4]).as_bytes() {
                            push_hex_escape(&mut out, cb);
                        }
                    } else {
                        out.push(ch);
                    }
                    i += ch.len_utf8();
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        // 合法前缀按字符处理（与控制字符转义规则一致）
                        let prefix = std::str::from_utf8(&bytes[i..i + valid])
                            .expect("valid_up_to 内必合法");
                        for ch in prefix.chars() {
                            if ch.is_control() {
                                for &cb in ch.encode_utf8(&mut [0u8; 4]).as_bytes() {
                                    push_hex_escape(&mut out, cb);
                                }
                            } else {
                                out.push(ch);
                            }
                        }
                        i += valid;
                    } else {
                        // 非法 UTF-8 起始字节：单字节转义
                        push_hex_escape(&mut out, b);
                        i += 1;
                    }
                }
            }
        }
    }
    out
}

/// 向 `out` 追加 `\xNN` 转义（大写十六进制）。
fn push_hex_escape(out: &mut String, b: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('\\');
    out.push('x');
    out.push(HEX[(b >> 4) as usize] as char);
    out.push(HEX[(b & 0x0F) as usize] as char);
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

    #[test]
    fn escaped_plain_text_unchanged() {
        assert_eq!(encode_escaped(b"AT+OK\r\n"), "AT+OK\r\n");
        assert_eq!(encode_escaped("中文测试".as_bytes()), "中文测试");
        assert_eq!(encode_escaped(b""), "");
        // \r \n \t 保留，空格原样
        assert_eq!(encode_escaped(b"a\tb\r\nc"), "a\tb\r\nc");
    }

    #[test]
    fn escaped_control_bytes() {
        // ESC 与 ANSI 颜色码
        assert_eq!(encode_escaped(b"\x1b[31mBoot\x1b[0m"), "\\x1B[31mBoot\\x1B[0m");
        // NUL 与 0x7F
        assert_eq!(encode_escaped(b"a\x00b\x7f"), "a\\x00b\\x7F");
        // 全字节扫描：除 \r \n \t 与可打印 ASCII 外均转义
        let all: Vec<u8> = (0..=0x7Fu8).collect();
        let s = encode_escaped(&all);
        assert!(!s.contains('\u{1}'));
        assert!(!s.contains('\u{7F}'));
        assert!(s.contains("\\x00"));
        assert!(s.contains("\\x1B"));
    }

    #[test]
    fn escaped_invalid_utf8() {
        // 0xFF 0xFE 非法 UTF-8
        assert_eq!(encode_escaped(&[0xFF, 0xFE]), "\\xFF\\xFE");
        // 截断的多字节序列：0xE4 后缺字节
        assert_eq!(encode_escaped(&[0xE4, 0xB8]), "\\xE4\\xB8");
        // 合法 UTF-8 前缀 + 非法字节混排
        assert_eq!(encode_escaped("中".as_bytes()), "中");
        assert_eq!(encode_escaped(&[0xE4, 0xB8, 0xAD, 0xFF]), "中\\xFF");
    }

    #[test]
    fn escaped_backslash() {
        assert_eq!(encode_escaped(b"a\\b"), "a\\\\b");
        // 反斜杠后跟 hex 样式文本：\\x1B 是字面量而非转义
        assert_eq!(encode_escaped(b"\\x1B"), "\\\\x1B");
    }

    #[test]
    fn escaped_c1_controls() {
        // U+0085 (C1 控制符, UTF-8: C2 85) 应转义
        assert_eq!(encode_escaped(&[0xC2, 0x85]), "\\xC2\\x85");
        // 合法前缀含 C1 + 后跟非法字节：前缀内的 C1 同样应转义
        assert_eq!(encode_escaped(&[0xC2, 0x85, 0xFF]), "\\xC2\\x85\\xFF");
        // 合法前缀含 C1 + 后跟合法文本
        assert_eq!(encode_escaped(&[0xC2, 0x85, b'a']), "\\xC2\\x85a");
    }

    #[test]
    fn escaped_composite() {
        // 模拟真实终端输出：ANSI 颜色 + 文本 + 换行 + 提示符
        let input = b"\x1b[1;34mbin\x1b[m  \x1b[1;36mlib32\x1b[m\r\n# ";
        assert_eq!(
            encode_escaped(input),
            "\\x1B[1;34mbin\\x1B[m  \\x1B[1;36mlib32\\x1B[m\r\n# "
        );
    }
}
