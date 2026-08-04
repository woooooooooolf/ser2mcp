//! 延迟探针：通过真实 MCP 协议（JSON-RPC over stdio）驱动 ser2mcp，
//! 访问真实串口设备（例如 COM27 上的 Linux 开发板），测量各工具的往返延迟，
//! 用于评估“AI 助手通过 ser2mcp 访问串口设备”的端到端延迟是否可接受。
//!
//! 用法:
//! ```bash
//! cargo build --release                     # 先构建 ser2mcp 本体
//! cargo run --release --example latency_probe -- COM27 [baudrate] [idle_ms] [iters]
//! cargo run --release --example latency_probe -- bench  COM27 [baudrate] [idle_ms] [iters] [read_timeout_ms]
//! # read_timeout_ms 缺省时使用 ser2mcp 服务端默认值，也可显式传入（如 100 / 1000）做对比
//! cargo run --release --example latency_probe -- benchw COM27 [iters]            # 只测 uart_write 路径
//! # 默认: COM27 115200 idle_ms=300 iters=5
//! ```
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use rmcp::{
    ClientHandler, RmcpError, ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
    transport::TokioChildProcess,
};
use serde_json::{Value, json};
use tokio::process::Command;

#[derive(Clone, Default)]
struct TestClient;

impl ClientHandler for TestClient {}

fn server_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("CARGO_BIN_EXE_ser2mcp") {
        return PathBuf::from(p);
    }
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let name = format!("ser2mcp{suffix}");
    let release = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join(&name);
    if release.exists() {
        return release;
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join(name)
}

async fn connect() -> Result<rmcp::service::RunningService<rmcp::RoleClient, TestClient>, RmcpError>
{
    let bin = server_bin();
    println!("启动 ser2mcp: {}", bin.display());
    let mut cmd = Command::new(bin);
    cmd.env("RUST_LOG", "error");
    let client = TestClient
        .serve(
            TokioChildProcess::new(cmd)
                .map_err(RmcpError::transport_creation::<TokioChildProcess>)?,
        )
        .await?;
    Ok(client)
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, TestClient>,
    name: &str,
    args: Value,
) -> Result<CallToolResult, rmcp::service::ServiceError> {
    client
        .call_tool(
            CallToolRequestParams::new(name.to_string())
                .with_arguments(args.as_object().expect("args 必须是对象").clone()),
        )
        .await
}

fn stats(label: &str, samples: &[f64]) {
    if samples.is_empty() {
        println!("{label}: (无样本)");
        return;
    }
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let min = s[0];
    let max = s[s.len() - 1];
    let avg = s.iter().sum::<f64>() / s.len() as f64;
    let mid = s[s.len() / 2];
    let p95 = if s.len() >= 5 {
        s[((s.len() as f64) * 0.95) as usize - 1]
    } else {
        max
    };
    println!(
        "{label}: n={} min={:.1}ms median={:.1}ms avg={:.1}ms p95={:.1}ms max={:.1}ms",
        s.len(),
        min,
        mid,
        avg,
        p95,
        max
    );
}

fn short_text(v: &Value, max: usize) -> String {
    let t = v
        .get("data")
        .and_then(|d| d.as_str())
        .unwrap_or("(无 data)")
        .replace('\r', "\\r")
        .replace('\n', "\\n");
    if t.len() <= max {
        t
    } else {
        format!("{}...({}B)", &t[..max], t.len())
    }
}

async fn exchange_once(
    client: &rmcp::service::RunningService<rmcp::RoleClient, TestClient>,
    port: &str,
    data: &str,
    idle_ms: u64,
    timeout_ms: u64,
) -> Result<(f64, Value), String> {
    let t0 = Instant::now();
    let r = call(
        client,
        "uart_exchange",
        json!({
            "port": port,
            "data": data,
            "mode": "text",
            "idle_ms": idle_ms,
            "timeout_ms": timeout_ms,
            "read_mode": "text",
        }),
    )
    .await
    .map_err(|e| format!("call_tool 失败: {e:?}"))?;
    let elapsed = t0.elapsed().as_secs_f64() * 1000.0;
    if r.is_error.unwrap_or(false) {
        return Err(format!("工具返回错误: {r:?}"));
    }
    let v = r.structured_content.clone().unwrap_or(json!({}));
    Ok((elapsed, v))
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().map(|s| s.as_str()) == Some("bench") {
        bench_mode(&args[1..]).await;
        return;
    }
    if args.first().map(|s| s.as_str()) == Some("benchw") {
        bench_write_mode(&args[1..]).await;
        return;
    }

    let port = args.first().cloned().unwrap_or_else(|| "COM27".to_string());
    let baudrate: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(115200);
    let idle_ms: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(300);
    let iters: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5);

    println!("=== ser2mcp 延迟探针 ===");
    println!("端口={port} 波特率={baudrate} idle_ms={idle_ms} iters={iters}");

    let client = connect().await.expect("连接 ser2mcp 失败");

    // 1. 协议/工具层开销
    let t0 = Instant::now();
    let tools = client
        .list_tools(Default::default())
        .await
        .expect("list_tools 失败");
    println!(
        "list_tools: {:.1}ms, {} 个工具",
        t0.elapsed().as_secs_f64() * 1000.0,
        tools.tools.len()
    );

    let t0 = Instant::now();
    let r = call(&client, "uart_list_ports", json!({}))
        .await
        .expect("uart_list_ports 失败");
    println!(
        "uart_list_ports: {:.1}ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
    if let Some(v) = &r.structured_content {
        println!("  ports: {v}");
    }

    // 2. 打开串口
    let t0 = Instant::now();
    let r = call(
        &client,
        "uart_open",
        json!({
            "port": port,
            "baudrate": baudrate,
            "buffer_size": 1048576,
            "discard_on_open": true,
        }),
    )
    .await
    .expect("uart_open 失败");
    println!(
        "uart_open: {:.1}ms, is_error={}",
        t0.elapsed().as_secs_f64() * 1000.0,
        r.is_error.unwrap_or(false)
    );
    if let Some(v) = &r.structured_content {
        println!("  {v}");
    }
    if r.is_error.unwrap_or(false) {
        eprintln!("打开串口失败，中止。");
        return;
    }

    // 3. 状态查询（无串口等待，近似 MCP 协议净开销）
    let mut avail = Vec::new();
    for _ in 0..iters {
        let t0 = Instant::now();
        let r = call(&client, "uart_available", json!({"port": port}))
            .await
            .expect("uart_available 失败");
        avail.push(t0.elapsed().as_secs_f64() * 1000.0);
        if let Some(v) = &r.structured_content {
            println!("  uart_available: {v}");
            break;
        }
    }
    stats("uart_available (协议净开销)", &avail);

    // 4. 功能测试：通过串口向 Linux 开发板发命令并测量往返延迟
    //    注意：命令必须带 \r（回车）才能让 shell 执行；否则只回显不执行。
    let commands = [
        ("echo ser2mcp_latency_ok\r", "ser2mcp_latency_ok"),
        ("uname -a\r", "Linux"),
        ("ls /\r", "/"),
        ("cat /proc/cpuinfo | head -3\r", "processor"),
    ];
    for (cmd, marker) in commands {
        let mut samples = Vec::new();
        println!("\n--- uart_exchange: `{cmd}` (idle_ms={idle_ms}) ---");
        for _ in 0..iters {
            match exchange_once(&client, &port, cmd, idle_ms, 5000).await {
                Ok((elapsed, v)) => {
                    samples.push(elapsed);
                    let data = v.get("data").and_then(|d| d.as_str()).unwrap_or_default();
                    let ok = data.contains(marker);
                    println!(
                        "  {} {:.1}ms bytes={} reason={} overflow={} data={}",
                        if ok { "PASS" } else { "FAIL" },
                        elapsed,
                        v.get("bytes").unwrap_or(&json!(0)),
                        v.get("reason").unwrap_or(&json!("?")),
                        v.get("overflow_delta").unwrap_or(&json!(0)),
                        short_text(&v, 80),
                    );
                }
                Err(e) => println!("  ERROR: {e}"),
            }
        }
        stats(&format!("uart_exchange `{cmd}` 汇总"), &samples);
    }

    // 5. 激进 idle 参数对比：短命令 + 长输出命令
    for (label, cmd, marker, idle) in [
        ("短命令 echo ok @ idle=50ms", "echo ok\r", "ok", 50),
        ("长输出 uname -a @ idle=50ms", "uname -a\r", "Linux", 50),
        ("短命令 echo ok @ idle=30ms", "echo ok\r", "ok", 30),
        ("长输出 uname -a @ idle=30ms", "uname -a\r", "Linux", 30),
    ] {
        let mut samples = Vec::new();
        println!("\n--- 对比: {label} ---");
        for _ in 0..iters {
            match exchange_once(&client, &port, cmd, idle, 5000).await {
                Ok((elapsed, v)) => {
                    samples.push(elapsed);
                    let data = v.get("data").and_then(|d| d.as_str()).unwrap_or_default();
                    let ok = data.contains(marker);
                    println!(
                        "  {} {:.1}ms bytes={} reason={} data={}",
                        if ok { "PASS" } else { "FAIL" },
                        elapsed,
                        v.get("bytes").unwrap_or(&json!(0)),
                        v.get("reason").unwrap_or(&json!("?")),
                        short_text(&v, 70),
                    );
                }
                Err(e) => println!("  ERROR: {e}"),
            }
        }
        stats(&format!("{label} 汇总"), &samples);
    }

    // 6. 只写（fire-and-forget）与无数据读取的延迟
    let t0 = Instant::now();
    let r = call(
        &client,
        "uart_write",
        json!({"port": port, "data": "echo fire_and_forget\r", "mode": "text"}),
    )
    .await
    .expect("uart_write 失败");
    println!(
        "\nuart_write (只发不等): {:.1}ms, {r:?}",
        t0.elapsed().as_secs_f64() * 1000.0
    );

    let t0 = Instant::now();
    let r = call(
        &client,
        "uart_read",
        json!({"port": port, "idle_ms": 100, "timeout_ms": 1500, "mode": "text"}),
    )
    .await
    .expect("uart_read 失败");
    let elapsed = t0.elapsed().as_secs_f64() * 1000.0;
    println!("uart_read (排空上一条命令): {elapsed:.1}ms, {r:?}");
    if let Some(v) = &r.structured_content {
        println!("  data={}", short_text(v, 120));
    }

    // 7. 关闭
    let t0 = Instant::now();
    let r = call(&client, "uart_close", json!({"port": port}))
        .await
        .expect("uart_close 失败");
    println!(
        "uart_close: {:.1}ms, {r:?}",
        t0.elapsed().as_secs_f64() * 1000.0
    );

    client.cancel().await.expect("关闭客户端失败");
    println!("\n=== 探针完成 ===");
}

async fn bench_mode(args: &[String]) {
    let port = args.first().cloned().unwrap_or_else(|| "COM27".to_string());
    let baudrate: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(115200);
    let idle_ms: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(300);
    let iters: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
    let read_timeout_ms: Option<u64> = args.get(4).and_then(|s| s.parse().ok());

    println!(
        "=== bench: 端口={port} 波特率={baudrate} idle_ms={idle_ms} iters={iters} read_timeout_ms={} ===",
        read_timeout_ms.map(|v| v.to_string()).unwrap_or_else(|| {
            format!("默认({}ms)", ser2mcp::manager::DEFAULT_READ_TIMEOUT_MS)
        })
    );

    let client = connect().await.expect("连接 ser2mcp 失败");
    let mut open_args = json!({
        "port": port,
        "baudrate": baudrate,
        "buffer_size": 1048576,
        "discard_on_open": true,
    });
    if let Some(v) = read_timeout_ms {
        open_args["read_timeout_ms"] = json!(v);
    }
    let r = call(&client, "uart_open", open_args)
        .await
        .expect("uart_open 失败");
    if r.is_error.unwrap_or(false) {
        eprintln!("打开串口失败: {r:?}");
        return;
    }

    let cmd = "echo ok\r";
    let mut samples = Vec::new();
    let mut fails = 0usize;
    for i in 0..iters {
        match exchange_once(&client, &port, cmd, idle_ms, 5000).await {
            Ok((elapsed, v)) => {
                let data = v.get("data").and_then(|d| d.as_str()).unwrap_or_default();
                if data.contains("ok") {
                    samples.push(elapsed);
                    println!(
                        "  [{i:>2}] {elapsed:8.1}ms  bytes={} {}",
                        v.get("bytes").unwrap_or(&json!(0)),
                        if elapsed > 1000.0 { "<-- >1s" } else { "" }
                    );
                } else {
                    fails += 1;
                    println!(
                        "  [{i:>2}] {elapsed:8.1}ms  FAIL data={}",
                        short_text(&v, 60)
                    );
                }
            }
            Err(e) => {
                fails += 1;
                println!("  [{i:>2}] ERROR {e}");
            }
        }
    }
    stats(&format!("bench `{cmd}` (idle={idle_ms}ms)"), &samples);
    let spikes = samples.iter().filter(|t| **t > 1000.0).count();
    println!(
        ">1s 尖峰: {spikes}/{} ({:.1}%), 失败: {fails}",
        samples.len(),
        spikes as f64 / samples.len().max(1) as f64 * 100.0
    );

    let _ = call(&client, "uart_close", json!({"port": port}))
        .await
        .expect("uart_close 失败");
    client.cancel().await.expect("关闭客户端失败");
}

async fn bench_write_mode(args: &[String]) {
    let port = args.first().cloned().unwrap_or_else(|| "COM27".to_string());
    let iters: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(30);

    println!("=== benchw: 端口={port} iters={iters}（仅 uart_write，不含 idle 等待）===");
    let client = connect().await.expect("连接 ser2mcp 失败");
    let r = call(
        &client,
        "uart_open",
        json!({
            "port": port,
            "baudrate": 115200,
            "buffer_size": 1048576,
            "discard_on_open": true,
        }),
    )
    .await
    .expect("uart_open 失败");
    if r.is_error.unwrap_or(false) {
        eprintln!("打开串口失败: {r:?}");
        return;
    }

    let mut samples = Vec::new();
    for i in 0..iters {
        let t0 = Instant::now();
        let r = call(
            &client,
            "uart_write",
            json!({"port": port, "data": "echo ok\r", "mode": "text"}),
        )
        .await
        .expect("uart_write 失败");
        let elapsed = t0.elapsed().as_secs_f64() * 1000.0;
        samples.push(elapsed);
        let written = r
            .structured_content
            .as_ref()
            .and_then(|v| v.get("written"))
            .cloned()
            .unwrap_or(json!(0));
        println!("  [{i:>2}] {elapsed:8.1}ms  written={written}");
    }
    stats("uart_write 汇总（MCP+写入净开销）", &samples);

    let _ = call(&client, "uart_close", json!({"port": port}))
        .await
        .expect("uart_close 失败");
    client.cancel().await.expect("关闭客户端失败");
}
