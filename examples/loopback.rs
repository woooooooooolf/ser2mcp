//! 回环自测工具：验证 ser2mcp 与真实串口硬件的联通。
//!
//! 用法：
//! ```bash
//! # 枚举本机串口
//! cargo run --release --example loopback -- --list
//!
//! # 对指定串口做回环测试（TX-RX 短接时发送内容应原样返回）
//! cargo run --release --example loopback -- COM3 115200
//! ```
//!
//! 回环测试流程：打开串口 → 发送测试数据（hex）→ 按 idle 语义拉取 →
//! 校验返回数据与发送一致（允许回显造成的前缀重复，按包含关系匹配）。

use ser2mcp::hex;
use ser2mcp::manager::{self, SerialManager};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--list") | Some("-l") => list_ports(),
        Some(port) => {
            let baudrate = args
                .get(2)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(manager::DEFAULT_BAUDRATE);
            loopback(port, baudrate);
        }
        None => {
            eprintln!("用法:");
            eprintln!("  {0} --list               # 枚举串口", args[0]);
            eprintln!(
                "  {0} COM3 [波特率]        # 回环测试（TX-RX 短接）",
                args[0]
            );
            std::process::exit(2);
        }
    }
}

fn list_ports() {
    let mgr = SerialManager::new();
    match mgr.list_ports() {
        Ok(ports) if ports.is_empty() => {
            println!("未发现可用串口。");
            std::process::exit(1);
        }
        Ok(ports) => {
            println!("可用串口:");
            for p in &ports {
                println!("  {}  [{}]  {}", p.name, p.port_type, p.description);
            }
        }
        Err(e) => {
            eprintln!("枚举失败: {e}");
            std::process::exit(1);
        }
    }
}

fn loopback(port: &str, baudrate: u32) {
    let mgr = SerialManager::new();
    println!("打开 {port} @ {baudrate} ...");
    if let Err(e) = mgr.open(
        port,
        baudrate,
        serialport::DataBits::Eight,
        serialport::Parity::None,
        serialport::StopBits::One,
        serialport::FlowControl::None,
        manager::DEFAULT_READ_TIMEOUT_MS,
        manager::DEFAULT_BUFFER_SIZE,
        true,
    ) {
        eprintln!("打开失败: {e}");
        std::process::exit(1);
    }
    println!("已打开，发送测试数据并等待回显（idle=500ms, timeout=3s）...");

    // 测试负载：递增字节序列，覆盖非 ASCII 二进制
    let payload: Vec<u8> = (0..=255u8).collect();
    let hex_payload = hex::encode(&payload);

    let rt = tokio::runtime::Runtime::new().expect("创建运行时失败");
    let outcome = rt.block_on(async {
        let _ = mgr.write(port, &payload).await?;
        mgr.read(port, 500, 64 * 1024, 3000).await
    });

    match outcome {
        Ok(o) => {
            println!(
                "收到 {} 字节, reason={:?}, overflow_delta={}",
                o.data.len(),
                o.reason,
                o.overflow_delta
            );
            if o.data.is_empty() {
                eprintln!("FAIL: 3 秒内未收到任何数据（检查 TX-RX 是否短接/波特率是否匹配）");
                std::process::exit(1);
            }
            // 校验：返回数据应包含发送内容（回环可能把发送内容原样回显，含开头）
            let sent_hex = &hex_payload;
            let recv_hex = hex::encode(&o.data);
            let matched =
                recv_hex.len() >= sent_hex.len() && recv_hex[..sent_hex.len()] == *sent_hex;
            if matched {
                println!("PASS: 回环数据与发送一致（前 {} 字节）", payload.len());
                println!("发送: {sent_hex}");
                if o.data.len() > payload.len() {
                    println!("额外回显: {}", &recv_hex[sent_hex.len()..]);
                }
            } else {
                eprintln!("FAIL: 回环数据与发送不一致");
                eprintln!("发送: {sent_hex}");
                eprintln!("收到: {recv_hex}");
                std::process::exit(1);
            }
            let _ = rt.block_on(mgr.close(port));
        }
        Err(e) => {
            eprintln!("读取失败: {e}");
            std::process::exit(1);
        }
    }
}
