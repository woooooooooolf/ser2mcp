//! 真实硬件回环测试（TX-RX 短接）：验证 `uart_send_file` 在 MCP 工具层的端到端行为。
//!
//! 需要硬件：一个 TX-RX 物理回环的串口（如 COM28）。
//!
//! ```bash
//! SER2MCP_LOOPBACK_PORT=COM28 cargo test --test loopback -- --ignored --nocapture
//! ```
//!
//! 覆盖（单测试函数顺序执行，避免多测试争用同一串口）：
//! - `uart_expect_send` 为 reply 追加 CRLF，并验证回环字节
//! - `match_scope=new` 忽略历史 pattern，且新数据仍可命中
//! - `ignore_ansi=true` 可跨颜色控制序列匹配可见文本，同时保留原始返回字节
//! - 历史缓冲存在时，`uart_exchange` 仍等到并返回本次新响应
//! - `uart_clear` 与 exchange 并发时，空结果只能以 timeout 返回
//! - text 模式发送 64KiB 确定性伪随机文件 → 读回逐字节比对
//! - base64 模式发送 → 读回解码比对（每行 ≤ 76 字符）
//! - 1KiB 小缓冲发送 64KiB → `uart_send_file` 直接报告上行覆盖增量
//! - `uart_send_cancel` 并发中止传输（reason=cancelled + 部分进度）
//! - `max_duration_ms` 在显式时限到达后自动停止（reason=duration_limit）
//! - `uart_close` 并发中断传输并关闭端口
//! - 原始 JSON-RPC `notifications/cancelled` 通知中止传输（客户端取消路径）

use std::path::PathBuf;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rmcp::{
    ClientHandler, RmcpError, ServiceExt,
    model::{CallToolRequestParams, CallToolResponse, CallToolResult},
    transport::TokioChildProcess,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};

use ser2mcp::hex;

#[derive(Clone, Default)]
struct TestClient;

impl ClientHandler for TestClient {}

async fn connect() -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, TestClient>> {
    let bin = env!("CARGO_BIN_EXE_ser2mcp");
    let client = TestClient
        .serve(
            TokioChildProcess::new(Command::new(bin))
                .map_err(RmcpError::transport_creation::<TokioChildProcess>)?,
        )
        .await?;
    Ok(client)
}

async fn call(
    client: &rmcp::Peer<rmcp::RoleClient>,
    name: &str,
    args: Value,
) -> Result<CallToolResult, rmcp::service::ServiceError> {
    match client
        .call_tool_once(
            CallToolRequestParams::new(name.to_string())
                .with_arguments(args.as_object().expect("args 必须是对象").clone()),
        )
        .await?
    {
        CallToolResponse::Complete(r) => Ok(r),
        other => panic!("意外响应类型: {other:?}"),
    }
}

fn loopback_port() -> String {
    std::env::var("SER2MCP_LOOPBACK_PORT")
        .expect("需要环境变量 SER2MCP_LOOPBACK_PORT 指定回环串口（如 COM28）")
}

/// 确定性伪随机数据（无需 rand 依赖）。
fn random_file(size: usize) -> Vec<u8> {
    (0..size)
        .map(|i| ((i.wrapping_mul(31) + 7) % 251) as u8)
        .collect()
}

struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn new(name: &str, content: &[u8]) -> Self {
        // 进程内唯一计数器避免并行测试临时文件命名冲突（as_nanos 可能碰撞）。
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ser2mcp-loopback-{name}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path, content).expect("写临时文件失败");
        Self { path }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 拉取回环缓冲中的全部数据（发送内容应原样返回）。
async fn read_all_hex(client: &rmcp::Peer<rmcp::RoleClient>, port: &str) -> String {
    let r = call(
        client,
        "uart_read",
        json!({"port": port, "read_mode": "hex", "max_bytes": 1_000_000, "idle_ms": 500, "timeout_ms": 15_000}),
    )
    .await
    .expect("uart_read 调用失败");
    assert!(!r.is_error.unwrap_or(false), "uart_read 报错: {r:?}");
    r.structured_content
        .as_ref()
        .and_then(|v| v.get("data"))
        .and_then(|d| d.as_str())
        .expect("应返回 hex data")
        .to_string()
}

async fn open_port(client: &rmcp::Peer<rmcp::RoleClient>, port: &str) {
    open_port_with_buffer(client, port, 1024 * 1024).await;
}

async fn open_port_with_buffer(
    client: &rmcp::Peer<rmcp::RoleClient>,
    port: &str,
    buffer_size: usize,
) {
    let r = call(
        client,
        "uart_open",
        json!({"port": port, "baudrate": 115200, "buffer_size": buffer_size}),
    )
    .await
    .expect("uart_open 调用失败");
    assert!(!r.is_error.unwrap_or(false), "uart_open 报错: {r:?}");
}

async fn wait_for_buffered(client: &rmcp::Peer<rmcp::RoleClient>, port: &str, minimum: u64) {
    for _ in 0..100 {
        let r = call(client, "uart_available", json!({"port": port}))
            .await
            .expect("uart_available 调用失败");
        let v = r.structured_content.expect("应有结构化返回");
        if v["buffered_bytes"].as_u64().unwrap_or_default() >= minimum {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("等待回环数据进入缓冲超时");
}

async fn wait_for_send_active(client: &rmcp::Peer<rmcp::RoleClient>, port: &str) {
    for _ in 0..100 {
        let r = call(client, "uart_available", json!({"port": port}))
            .await
            .expect("uart_available 调用失败");
        let v = r.structured_content.expect("应有结构化返回");
        if v["send"]["active"] == json!(true) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("等待文件发送进入 active 状态超时");
}

#[tokio::test]
#[ignore = "需要真实回环硬件（TX-RX 短接）"]
async fn loopback_send_file_all() {
    let port = loopback_port();
    let client = connect().await.expect("连接 ser2mcp 失败");
    let data = random_file(64 * 1024);
    let tmp = TempFile::new("all", &data);

    // ============ 场景 1：expect_send 的 reply newline ============
    open_port(&client, &port).await;
    let marker = "EXPECT-SEND-PATTERN";
    let r = call(
        &client,
        "uart_write",
        json!({"port": port, "data": marker, "mode": "text"}),
    )
    .await
    .expect("uart_write 调用失败");
    assert!(!r.is_error.unwrap_or(false), "uart_write 报错: {r:?}");
    let r = call(
        &client,
        "uart_expect_send",
        json!({
            "port": port,
            "pattern": marker,
            "pattern_mode": "text",
            "reply": "R",
            "reply_mode": "text",
            "newline": "crlf",
            "read_mode": "text-escaped",
            "timeout_ms": 3000
        }),
    )
    .await
    .expect("uart_expect_send 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["matched"], json!(true), "应命中回环 marker: {v:?}");
    assert_eq!(v["written"], json!(3), "reply 应追加 CRLF: {v:?}");
    assert_eq!(v["newline"], json!("crlf"));
    let reply_hex = read_all_hex(&client, &port).await;
    assert_eq!(
        hex::decode(&reply_hex).expect("hex 解码失败"),
        b"R\r\n",
        "expect_send reply 回环字节不一致"
    );

    // ============ 场景 1b：match_scope=new 忽略历史命中，但保留 FIFO 数据语义 ============
    let scope_marker = "SCOPE-NEW-MARK";
    let r = call(
        &client,
        "uart_write",
        json!({"port": port, "data": scope_marker, "mode": "text"}),
    )
    .await
    .expect("预置 match_scope 历史缓冲失败");
    assert!(!r.is_error.unwrap_or(false));
    wait_for_buffered(&client, &port, scope_marker.len() as u64).await;

    let r = call(
        &client,
        "uart_expect_send",
        json!({
            "port": port,
            "pattern": scope_marker,
            "pattern_mode": "text",
            "reply": "X",
            "reply_mode": "text",
            "match_scope": "new",
            "timeout_ms": 150
        }),
    )
    .await
    .expect("match_scope=new expect_send 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(
        v["matched"],
        json!(false),
        "历史 marker 不应触发 new: {v:?}"
    );
    assert_eq!(v["written"], json!(0), "未命中不得发送 reply: {v:?}");
    assert_eq!(v["match_scope"], json!("new"));
    assert_eq!(
        v["pending"],
        json!(true),
        "未消费的历史 marker 应明确标记 pending: {v:?}"
    );

    let r = call(
        &client,
        "uart_expect",
        json!({
            "port": port,
            "data": scope_marker,
            "mode": "text",
            "pattern": scope_marker,
            "pattern_mode": "text",
            "match_scope": "new",
            "read_mode": "text",
            "timeout_ms": 3000
        }),
    )
    .await
    .expect("match_scope=new expect 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["matched"], json!(true), "调用后新 marker 应命中: {v:?}");
    assert_eq!(v["match_scope"], json!("new"));
    assert_eq!(
        v["data"],
        json!(format!("{scope_marker}{scope_marker}")),
        "consume=true 仍应按 FIFO 返回历史前缀和新命中: {v:?}"
    );

    // ============ 场景 1c：可选忽略 ANSI 序列匹配，原始数据保持不变 ============
    let ansi_payload = b"\x1b[31mAB\x1b[0m\x1b[32mCD\x1b[0m|ANSI-TAIL";
    let r = call(
        &client,
        "uart_expect",
        json!({
            "port": port,
            "data": hex::encode(ansi_payload),
            "mode": "hex",
            "pattern": "ABCD",
            "pattern_mode": "text",
            "match_scope": "new",
            "read_mode": "hex",
            "timeout_ms": 150
        }),
    )
    .await
    .expect("原始 ANSI 匹配调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["matched"], json!(false), "原始匹配不应跨 ANSI: {v:?}");
    assert_eq!(v["pending"], json!(true));

    let r = call(
        &client,
        "uart_expect",
        json!({
            "port": port,
            "pattern": "ABCD",
            "pattern_mode": "text",
            "ignore_ansi": true,
            "read_mode": "hex",
            "timeout_ms": 1000
        }),
    )
    .await
    .expect("忽略 ANSI 匹配调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["matched"], json!(true), "应匹配可见 ABCD: {v:?}");
    assert_eq!(v["ignore_ansi"], json!(true));
    assert_eq!(
        hex::decode(v["data"].as_str().expect("应返回 hex data")).unwrap(),
        b"\x1b[31mAB\x1b[0m\x1b[32mCD",
        "忽略 ANSI 只影响匹配，返回应保留原始字节"
    );
    let _ = read_all_hex(&client, &port).await;

    // ============ 场景 2：exchange 不被历史静默缓冲提前收尾 ============
    let old = b"OLD-BUFFER|";
    let new = b"NEW-RESPONSE";
    let r = call(
        &client,
        "uart_write",
        json!({"port": port, "data": String::from_utf8_lossy(old), "mode": "text"}),
    )
    .await
    .expect("预置历史缓冲失败");
    assert!(!r.is_error.unwrap_or(false), "uart_write 报错: {r:?}");
    wait_for_buffered(&client, &port, old.len() as u64).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let r = call(
        &client,
        "uart_exchange",
        json!({
            "port": port,
            "data": String::from_utf8_lossy(new),
            "mode": "text",
            "read_mode": "hex",
            "idle_ms": 200,
            "timeout_ms": 3000
        }),
    )
    .await
    .expect("uart_exchange 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    let exchange_data =
        hex::decode(v["data"].as_str().expect("应返回 hex data")).expect("exchange hex 解码失败");
    assert_eq!([old.as_slice(), new.as_slice()].concat(), exchange_data);
    assert_eq!(v["overflow_delta"], json!(0));
    assert_eq!(v["new_data_observed"], json!(true));

    // ============ 场景 2b：并发 clear 不得制造 bytes=0 的 idle/max_bytes ============
    for round in 0..8 {
        let _ = call(&client, "uart_clear", json!({"port": port}))
            .await
            .expect("竞态测试前 uart_clear 失败");
        let race_client = client.clone();
        let race_port = port.clone();
        let marker = format!("CLEAR-RACE-{round:02}-{}", "X".repeat(128));
        let exchange_task = tokio::spawn(async move {
            call(
                &race_client,
                "uart_exchange",
                json!({
                    "port": race_port,
                    "data": marker,
                    "mode": "text",
                    "read_mode": "hex",
                    "idle_ms": 0,
                    "timeout_ms": 250
                }),
            )
            .await
        });
        // clear 不持有全局 I/O 锁，反复清理可覆盖“状态判定 → 实际消费”的竞态窗口。
        for _ in 0..20 {
            let _ = call(&client, "uart_clear", json!({"port": port}))
                .await
                .expect("并发 uart_clear 失败");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let r = exchange_task
            .await
            .expect("竞态 exchange task 崩溃")
            .expect("竞态 uart_exchange 调用失败");
        let v = r.structured_content.expect("应有结构化返回");
        let bytes = v["bytes"].as_u64().unwrap();
        let reason = v["reason"].as_str().unwrap();
        assert!(
            bytes > 0 || reason == "timeout",
            "空结果只能以 timeout 返回: round={round}, result={v:?}"
        );
    }

    // ============ 场景 3：text 模式往返 ============
    let r = call(
        &client,
        "uart_send_file",
        json!({"port": port, "path": tmp.path, "mode": "text", "chunk_size": 1024}),
    )
    .await
    .expect("send_file 调用失败");
    assert!(!r.is_error.unwrap_or(false), "send_file 报错: {r:?}");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["reason"], json!("completed"), "text 发送应完成: {v:?}");
    assert_eq!(v["raw_bytes"], json!(data.len() as u64));
    assert_eq!(v["sent_bytes"], json!(data.len() as u64));
    assert_eq!(v["chunks"], json!(64));
    assert_eq!(
        v["device_error"],
        json!(null),
        "正常回环不应有设备错误: {v:?}"
    );

    let hex_back = read_all_hex(&client, &port).await;
    let back = hex::decode(&hex_back).expect("hex 解码失败");
    assert_eq!(back.len(), data.len(), "读回长度不一致");
    assert_eq!(back, data, "text 模式回环内容不一致");

    // ============ 场景 4：base64 模式往返 ============
    let _ = call(&client, "uart_clear", json!({"port": port}))
        .await
        .expect("uart_clear 调用失败");
    let r = call(
        &client,
        "uart_send_file",
        json!({"port": port, "path": tmp.path, "mode": "base64", "chunk_size": 57}),
    )
    .await
    .expect("send_file 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["reason"], json!("completed"));
    let sent = v["sent_bytes"].as_u64().unwrap();
    assert!(sent > data.len() as u64, "base64 应膨胀: {sent}");

    let hex_back = read_all_hex(&client, &port).await;
    let b64_text = String::from_utf8(hex::decode(&hex_back).expect("hex 解码失败"))
        .expect("base64 应可读为文本");
    for line in b64_text.lines() {
        assert!(line.len() <= 76, "base64 行超宽: {}", line.len());
    }
    assert!(b64_text.ends_with('\n'), "base64 应以换行结尾");
    let stripped: Vec<u8> = b64_text.bytes().filter(|&b| b != b'\n').collect();
    let decoded = STANDARD.decode(stripped).expect("base64 解码失败");
    assert_eq!(decoded, data, "base64 模式回环解码后不一致");

    // ============ 场景 5：小缓冲发送直接报告溢出 ============
    let _ = call(&client, "uart_close", json!({"port": port}))
        .await
        .expect("uart_close 调用失败");
    open_port_with_buffer(&client, &port, 1024).await;
    let r = call(
        &client,
        "uart_send_file",
        json!({"port": port, "path": tmp.path, "mode": "text", "chunk_size": 1024}),
    )
    .await
    .expect("小缓冲 send_file 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    let send_overflow = v["overflow_delta"].as_u64().unwrap();
    assert!(
        send_overflow > 0,
        "1KiB 缓冲回环 64KiB 应在 send_file 返回中报告覆盖: {v:?}"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let r = call(&client, "uart_available", json!({"port": port}))
        .await
        .expect("uart_available 调用失败");
    let available = r.structured_content.expect("应有结构化返回");
    assert!(
        available["overflow_total"].as_u64().unwrap() >= v["overflow_total"].as_u64().unwrap(),
        "返回后尾部上行数据只能让累计溢出增长: send={v:?}, available={available:?}"
    );
    let _ = call(&client, "uart_close", json!({"port": port}))
        .await
        .expect("uart_close 调用失败");
    open_port(&client, &port).await;

    // ============ 场景 6：uart_send_cancel 并发中止 ============
    let _ = call(&client, "uart_clear", json!({"port": port}))
        .await
        .expect("uart_clear 调用失败");
    let client2 = client.clone();
    let port2 = port.clone();
    let path2 = tmp.path.clone();
    let send_task = tokio::spawn(async move {
        call(
            &client2,
            "uart_send_file",
            json!({"port": port2, "path": path2, "chunk_size": 256, "gap_ms": 100}),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(600)).await;
    let r = call(&client, "uart_send_cancel", json!({"port": port}))
        .await
        .expect("uart_send_cancel 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(
        v["cancelled"],
        json!(true),
        "发送中取消应返回 cancelled=true: {v:?}"
    );
    let r = send_task
        .await
        .expect("send_file task 崩溃")
        .expect("send_file 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(
        v["reason"],
        json!("cancelled"),
        "取消后应返回 cancelled: {v:?}"
    );
    assert!(
        v["sent_bytes"].as_u64().unwrap() < data.len() as u64,
        "应只发送部分: {v:?}"
    );

    let r = call(&client, "uart_available", json!({"port": port}))
        .await
        .expect("uart_available 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["send"]["active"], json!(false));
    assert_eq!(v["send"]["last_reason"], json!("cancelled"));

    // ============ 场景 6b：显式 max_duration_ms 自动止损，默认阻塞语义不变 ============
    let _ = call(&client, "uart_clear", json!({"port": port}))
        .await
        .expect("时限测试前 uart_clear 调用失败");
    let r = call(
        &client,
        "uart_send_file",
        json!({
            "port": port,
            "path": tmp.path,
            "chunk_size": 256,
            "gap_ms": 100,
            "max_duration_ms": 350
        }),
    )
    .await
    .expect("max_duration_ms send_file 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(
        v["reason"],
        json!("duration_limit"),
        "显式时限应在检查点停止: {v:?}"
    );
    assert_eq!(v["max_duration_ms"], json!(350));
    assert!(
        v["sent_bytes"].as_u64().unwrap() > 0,
        "应已有部分进度: {v:?}"
    );
    assert!(
        v["sent_bytes"].as_u64().unwrap() < data.len() as u64,
        "时限停止不应发完整文件: {v:?}"
    );
    let r = call(&client, "uart_available", json!({"port": port}))
        .await
        .expect("时限停止后 uart_available 调用失败");
    let available = r.structured_content.expect("应有结构化返回");
    assert_eq!(available["send"]["active"], json!(false));
    assert_eq!(available["send"]["last_reason"], json!("duration_limit"));

    // ============ 场景 7：uart_close 并发中断，close→write 不产生副作用 ============
    let client3 = client.clone();
    let port3 = port.clone();
    let path3 = tmp.path.clone();
    let send_task = tokio::spawn(async move {
        call(
            &client3,
            "uart_send_file",
            json!({"port": port3, "path": path3, "chunk_size": 4096, "gap_ms": 60000}),
        )
        .await
    });
    wait_for_send_active(&client, &port).await;

    let close_client = client.clone();
    let close_port = port.clone();
    let close_task = tokio::spawn(async move {
        call(&close_client, "uart_close", json!({"port": close_port})).await
    });

    // 在 close 与 write 并发交叠时，write 要么观察到 closing，要么观察到端口已经
    // 移除；两种结果都必须报错，不能抢在最终释放前产生外部副作用或隐式重开。
    tokio::time::sleep(Duration::from_millis(20)).await;

    let r = call(
        &client,
        "uart_write",
        json!({"port": port, "data": "CLOSE-RACE-WRITE", "mode": "text"}),
    )
    .await
    .expect("关闭期间 uart_write 调用失败");
    assert!(
        r.is_error.unwrap_or(false),
        "关闭开始后的 uart_write 必须拒绝执行: {r:?}"
    );
    let error = r
        .structured_content
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        error.contains("正在关闭") || error.contains("未打开"),
        "close/write 交叠时应明确报告关闭中或已关闭: {r:?}"
    );

    let r = close_task
        .await
        .expect("uart_close task 崩溃")
        .expect("uart_close 调用失败");
    assert_eq!(
        r.structured_content.as_ref().and_then(|v| v.get("closed")),
        Some(&json!(true)),
        "close 应成功并中断发送: {r:?}"
    );
    let r = send_task
        .await
        .expect("send_file task 崩溃")
        .expect("send_file 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(
        v["reason"],
        json!("cancelled"),
        "close 中断后 send_file 应返回 cancelled: {v:?}"
    );
    assert!(v["sent_bytes"].as_u64().unwrap() < data.len() as u64);
    let r = call(&client, "uart_available", json!({"port": port}))
        .await
        .expect("uart_available 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["open"], json!(false), "close 后端口应已关闭");

    let r = call(
        &client,
        "uart_write",
        json!({"port": port, "data": "AFTER-CLOSE", "mode": "text"}),
    )
    .await
    .expect("close 返回后的 uart_write 调用失败");
    assert!(
        r.is_error.unwrap_or(false),
        "close 返回后的 uart_write 必须报未打开，不能隐式重开: {r:?}"
    );
    assert!(
        r.structured_content
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("未打开"),
        "close 返回后的错误应明确说明端口未打开: {r:?}"
    );

    // ============ 场景 8：原始 JSON-RPC 取消通知（notifications/cancelled） ============
    raw_cancelled_notification_test(&port, &tmp.path).await;

    client.cancel().await.expect("关闭客户端失败");
}

/// 场景 6：手写 JSON-RPC 帧（Content-Length 头），验证 `notifications/cancelled`
/// 能中止进行中的 `uart_send_file`（rmcp 服务端收到取消通知会 cancel 请求级
/// CancellationToken，发送循环在检查点退出）。
async fn raw_cancelled_notification_test(port: &str, path: &std::path::Path) {
    let bin = env!("CARGO_BIN_EXE_ser2mcp");
    let mut child = Command::new(bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn ser2mcp 失败");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    async fn send_frame(stdin: &mut ChildStdin, v: &Value) {
        // rmcp 3.x 的 stdio transport 是 newline-delimited JSON（非 Content-Length）。
        let mut body = serde_json::to_string(v).unwrap();
        body.push('\n');
        stdin.write_all(body.as_bytes()).await.unwrap();
        stdin.flush().await.unwrap();
    }
    async fn read_frame(reader: &mut BufReader<ChildStdout>) -> Value {
        // 逐行读取，跳过空行，直到解析出合法 JSON（newline-delimited）。
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                return v;
            }
            // 非 JSON 行（理论上不会出现）继续读
        }
    }
    async fn read_frame_timeout(reader: &mut BufReader<ChildStdout>, what: &str) -> Value {
        match tokio::time::timeout(Duration::from_secs(10), read_frame(reader)).await {
            Ok(v) => v,
            Err(_) => panic!("读取 {what} 响应超时（子进程未响应）"),
        }
    }

    // initialize
    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "loopback-raw", "version": "0.0.0"}
            }
        }),
    )
    .await;
    let resp = read_frame_timeout(&mut reader, "initialize").await;
    assert_eq!(resp["id"], json!(1), "initialize 响应: {resp:?}");

    send_frame(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}),
    )
    .await;

    let mut next_id = 2i64;

    // 打开串口
    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "id": next_id, "method": "tools/call",
            "params": {"name": "uart_open", "arguments": {"port": port, "baudrate": 115200}}
        }),
    )
    .await;
    next_id += 1;
    let resp = read_frame_timeout(&mut reader, "uart_open").await;
    assert!(resp.get("error").is_none(), "uart_open 失败: {resp:?}");

    // 发起长发送（gap 拉长，保证取消前仍在发送）
    let send_id = next_id;
    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "id": send_id, "method": "tools/call",
            "params": {
                "name": "uart_send_file",
                "arguments": {"port": port, "path": path, "chunk_size": 256, "gap_ms": 200}
            }
        }),
    )
    .await;
    next_id += 1;

    tokio::time::sleep(Duration::from_millis(600)).await;

    // 发送取消通知（中止 send_id 请求）
    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": {"requestId": send_id, "reason": "loopback-test-cancel"}
        }),
    )
    .await;

    // 查询状态：发送应已中止（active=false, last_reason=cancelled）
    tokio::time::sleep(Duration::from_millis(500)).await;
    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "id": next_id, "method": "tools/call",
            "params": {"name": "uart_available", "arguments": {"port": port}}
        }),
    )
    .await;
    next_id += 1;
    let resp = read_frame_timeout(&mut reader, "uart_available").await;
    let content = resp["result"]["structuredContent"].clone();
    assert_eq!(
        content["send"]["active"],
        json!(false),
        "取消通知后发送应中止: {resp:?}"
    );
    assert_eq!(
        content["send"]["last_reason"],
        json!("cancelled"),
        "应记录 cancelled: {resp:?}"
    );

    // 关闭端口（可能先收到 send_id 的响应，循环读取直到 close 的响应）
    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "id": next_id, "method": "tools/call",
            "params": {"name": "uart_close", "arguments": {"port": port}}
        }),
    )
    .await;
    next_id += 1;
    for _ in 0..4 {
        let resp = read_frame_timeout(&mut reader, "close").await;
        if resp["id"] == json!(next_id - 1) {
            assert!(resp.get("error").is_none(), "uart_close 失败: {resp:?}");
            break;
        }
        assert_eq!(resp["id"], json!(send_id), "意外响应: {resp:?}");
    }

    stdin.shutdown().await.unwrap();
    let _ = child.kill().await;
    let _ = child.wait().await;
}
