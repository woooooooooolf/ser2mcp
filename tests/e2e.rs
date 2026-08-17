//! 端到端集成测试：以子进程方式启动 ser2mcp 二进制，通过真实 MCP 协议
//! （JSON-RPC over stdio）验证工具注册、参数校验与错误路径。
//!
//! 不依赖真实串口硬件（打开不存在的端口用于验证错误路径）；
//! 回环硬件自测见 README。

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

async fn connect() -> Result<rmcp::service::RunningService<rmcp::RoleClient, TestClient>, RmcpError>
{
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

fn structured_error_of(r: &CallToolResult) -> Option<String> {
    r.structured_content
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(|e| e.as_str())
        .map(String::from)
}

#[tokio::test]
async fn e2e_tool_registration_and_errors() {
    let client = connect().await.expect("连接 ser2mcp 失败");

    // 1. 工具注册：应包含全部 14 个工具
    let tools = client
        .list_tools(Default::default())
        .await
        .expect("list_tools 失败");
    let mut names: Vec<String> = tools.tools.iter().map(|t| t.name.to_string()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "uart_available".to_string(),
            "uart_clear".to_string(),
            "uart_close".to_string(),
            "uart_configure".to_string(),
            "uart_exchange".to_string(),
            "uart_expect".to_string(),
            "uart_expect_send".to_string(),
            "uart_list_ports".to_string(),
            "uart_open".to_string(),
            "uart_read".to_string(),
            "uart_send_cancel".to_string(),
            "uart_send_estimate".to_string(),
            "uart_send_file".to_string(),
            "uart_write".to_string(),
        ],
        "工具清单不完整"
    );
    // 工具应带 description 与参数 schema
    let open_tool = tools
        .tools
        .iter()
        .find(|t| t.name == "uart_open")
        .expect("缺少 uart_open");
    assert!(open_tool.description.is_some());
    assert!(!open_tool.input_schema.is_empty());

    // 带参工具的 Schema 应与运行时校验一致：拒绝未知字段。
    for tool in tools
        .tools
        .iter()
        .filter(|tool| tool.name != "uart_list_ports")
    {
        assert_eq!(
            tool.input_schema.get("additionalProperties"),
            Some(&json!(false)),
            "{} 的 Schema 应显式拒绝未知字段: {:?}",
            tool.name,
            tool.input_schema
        );
    }
    let configure_tool = tools
        .tools
        .iter()
        .find(|tool| tool.name == "uart_configure")
        .expect("缺少 uart_configure");
    assert!(
        configure_tool.input_schema["properties"]
            .get("buffer_size")
            .is_none(),
        "buffer_size 只能在 uart_open 设置"
    );
    let expect_send_tool = tools
        .tools
        .iter()
        .find(|tool| tool.name == "uart_expect_send")
        .expect("缺少 uart_expect_send");
    assert!(
        expect_send_tool.input_schema["properties"]
            .get("newline")
            .is_some(),
        "uart_expect_send Schema 应暴露 newline"
    );
    assert!(
        expect_send_tool.input_schema["properties"]
            .get("match_scope")
            .is_some(),
        "uart_expect_send Schema 应暴露 match_scope"
    );
    assert!(
        expect_send_tool.input_schema["properties"]
            .get("ignore_ansi")
            .is_some(),
        "uart_expect_send Schema 应暴露 ignore_ansi"
    );
    let expect_tool = tools
        .tools
        .iter()
        .find(|t| t.name == "uart_expect")
        .expect("缺少 uart_expect");
    assert!(
        expect_tool.input_schema["properties"]
            .get("match_scope")
            .is_some(),
        "uart_expect Schema 应暴露 match_scope"
    );
    assert!(
        expect_tool.input_schema["properties"]
            .get("ignore_ansi")
            .is_some(),
        "uart_expect Schema 应暴露 ignore_ansi"
    );
    let send_file_tool = tools
        .tools
        .iter()
        .find(|t| t.name == "uart_send_file")
        .expect("缺少 uart_send_file");
    assert!(
        send_file_tool.input_schema["properties"]
            .get("max_duration_ms")
            .is_some(),
        "uart_send_file Schema 应暴露 max_duration_ms"
    );

    // 2. uart_list_ports：无硬件也应成功（返回数组，可能为空）
    let r = call(&client, "uart_list_ports", json!({}))
        .await
        .expect("list_ports 调用失败");
    assert!(!r.is_error.unwrap_or(false));
    let ports = r
        .structured_content
        .as_ref()
        .and_then(|v| v.get("ports"))
        .expect("应返回 ports 数组");
    assert!(ports.is_array());

    // 3. uart_available：未打开时 open=false
    let r = call(
        &client,
        "uart_available",
        json!({"port": "COM_SER2MCP_NONEXISTENT"}),
    )
    .await
    .expect("available 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["open"], json!(false));
    assert_eq!(v["pending"], json!(false));

    // 4. uart_read 未打开 → 工具级错误（结构化 error）
    let r = call(
        &client,
        "uart_read",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "timeout_ms": 100}),
    )
    .await
    .expect("read 调用失败");
    assert!(r.is_error.unwrap_or(false));
    assert!(
        structured_error_of(&r)
            .unwrap_or_default()
            .contains("未打开")
    );

    // 5. uart_write 未打开 → 工具级错误
    let r = call(
        &client,
        "uart_write",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "data": "41"}),
    )
    .await
    .expect("write 调用失败");
    assert!(r.is_error.unwrap_or(false));

    // 6. 参数校验：非法 hex → 协议级 invalid_params（call_tool 返回 Err）
    let r = call(
        &client,
        "uart_write",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "data": "zz"}),
    )
    .await;
    assert!(r.is_err(), "非法 hex 应触发 invalid_params: {r:?}");

    // 7. 参数校验：非法 mode → 协议级错误
    let r = call(
        &client,
        "uart_read",
        json!({
            "port": "COM_SER2MCP_NONEXISTENT",
            "read_mode": "base64",
            "timeout_ms": 100
        }),
    )
    .await;
    assert!(r.is_err(), "非法 read_mode 应触发 invalid_params");

    // 8. uart_open 不存在的端口 → 工具级错误
    let r = call(
        &client,
        "uart_open",
        json!({"port": "COM_SER2MCP_NONEXISTENT"}),
    )
    .await
    .expect("open 调用失败");
    assert!(r.is_error.unwrap_or(false), "打开不存在的端口应报错: {r:?}");
    assert!(structured_error_of(&r).is_some());

    // 9. 打开失败后状态仍为未打开
    let r = call(
        &client,
        "uart_available",
        json!({"port": "COM_SER2MCP_NONEXISTENT"}),
    )
    .await
    .expect("available 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["open"], json!(false));

    // 10. uart_close 未打开 → 工具级错误
    let r = call(
        &client,
        "uart_close",
        json!({"port": "COM_SER2MCP_NONEXISTENT"}),
    )
    .await
    .expect("close 调用失败");
    assert!(r.is_error.unwrap_or(false));

    // 11. uart_expect 未打开 → 工具级错误
    let r = call(
        &client,
        "uart_expect",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41 42", "timeout_ms": 100}),
    )
    .await
    .expect("expect 调用失败");
    assert!(r.is_error.unwrap_or(false));
    assert!(
        structured_error_of(&r)
            .unwrap_or_default()
            .contains("未打开")
    );

    // 12. uart_expect_send 未打开 → 工具级错误
    let r = call(
        &client,
        "uart_expect_send",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41 42", "reply": "0D 0A", "timeout_ms": 100}),
    )
    .await
    .expect("expect_send 调用失败");
    assert!(r.is_error.unwrap_or(false));

    // 13. 参数校验：空 pattern → 协议级 invalid_params
    let r = call(
        &client,
        "uart_expect",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": ""}),
    )
    .await;
    assert!(r.is_err(), "空 pattern 应触发 invalid_params: {r:?}");

    // 14. 参数校验：非法 hex pattern → 协议级 invalid_params
    let r = call(
        &client,
        "uart_expect",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "zz"}),
    )
    .await;
    assert!(r.is_err(), "非法 hex pattern 应触发 invalid_params");

    // 15. 参数校验：uart_expect_send 空 reply → 协议级 invalid_params
    let r = call(
        &client,
        "uart_expect_send",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41 42", "reply": ""}),
    )
    .await;
    assert!(r.is_err(), "空 reply 应触发 invalid_params: {r:?}");

    // 16. 参数校验：非法 pattern_mode → 协议级错误
    let r = call(
        &client,
        "uart_expect",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41 42", "pattern_mode": "base64"}),
    )
    .await;
    assert!(r.is_err(), "非法 pattern_mode 应触发 invalid_params");

    // match_scope 只接受 buffer/new；合法的 new 应继续进入端口检查。
    let r = call(
        &client,
        "uart_expect",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41 42", "match_scope": "future"}),
    )
    .await;
    assert!(r.is_err(), "非法 match_scope 应触发 invalid_params");
    let r = call(
        &client,
        "uart_expect",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41 42", "match_scope": "new", "ignore_ansi": true, "timeout_ms": 100}),
    )
    .await
    .expect("合法 match_scope/ignore_ansi 应进入端口检查");
    assert!(r.is_error.unwrap_or(false));

    // 17. 参数校验：timeout_ms 超上限 → 协议级 invalid_params
    let r = call(
        &client,
        "uart_expect",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41 42", "timeout_ms": 999999999}),
    )
    .await;
    assert!(r.is_err(), "timeout_ms 超上限应触发 invalid_params");

    // 18. 资源参数超上限必须在进入串口/文件操作前拒绝
    let r = call(
        &client,
        "uart_open",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "buffer_size": 16 * 1024 * 1024 + 1}),
    )
    .await;
    assert!(r.is_err(), "buffer_size 超上限应触发 invalid_params");

    let r = call(
        &client,
        "uart_send_estimate",
        json!({"path": "missing.bin", "chunk_size": 1024 * 1024 + 1}),
    )
    .await;
    assert!(r.is_err(), "chunk_size 超上限应触发 invalid_params");

    let r = call(
        &client,
        "uart_read",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "timeout_ms": 300001}),
    )
    .await;
    assert!(
        r.is_err(),
        "uart_read timeout_ms 超上限应触发 invalid_params"
    );

    let r = call(
        &client,
        "uart_exchange",
        json!({
            "port": "COM_SER2MCP_NONEXISTENT",
            "data": "x",
            "mode": "text",
            "timeout_ms": 300001
        }),
    )
    .await;
    assert!(
        r.is_err(),
        "uart_exchange timeout_ms 超上限应触发 invalid_params"
    );

    client.cancel().await.expect("关闭客户端失败");
}

#[tokio::test]
async fn e2e_text_escaped_and_newline_params() {
    let client = connect().await.expect("连接 ser2mcp 失败");

    // 1. 发送侧拒绝 text-escaped（仅返回侧可用）→ 协议级 invalid_params
    let r = call(
        &client,
        "uart_write",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "data": "x", "mode": "text-escaped"}),
    )
    .await;
    assert!(
        r.is_err(),
        "发送侧 text-escaped 应触发 invalid_params: {r:?}"
    );

    // 2. 非法 newline 值 → 协议级 invalid_params
    let r = call(
        &client,
        "uart_write",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "data": "x", "newline": "cr"}),
    )
    .await;
    assert!(r.is_err(), "非法 newline 应触发 invalid_params: {r:?}");

    // 3. read_mode=text-escaped 为合法值：未打开端口应报"未打开"（工具级错误）而非模式错误
    let r = call(
        &client,
        "uart_read",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "read_mode": "text-escaped", "timeout_ms": 100}),
    )
    .await
    .expect("read 调用失败");
    assert!(r.is_error.unwrap_or(false));
    assert!(
        structured_error_of(&r)
            .unwrap_or_default()
            .contains("未打开"),
        "read_mode=text-escaped 应合法，收到参数错误: {r:?}"
    );

    // 4. newline=crlf 为合法值：同样应报"未打开"而非参数错误
    let r = call(
        &client,
        "uart_exchange",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "data": "x", "mode": "text", "newline": "crlf", "timeout_ms": 100}),
    )
    .await
    .expect("exchange 调用失败");
    assert!(r.is_error.unwrap_or(false));
    assert!(
        structured_error_of(&r)
            .unwrap_or_default()
            .contains("未打开"),
        "newline=crlf 应合法，收到参数错误: {r:?}"
    );

    // 5. expect 的 data 支持 newline：合法值应报"未打开"而非参数错误
    let r = call(
        &client,
        "uart_expect",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41 42", "data": "x", "mode": "text", "newline": "lf", "timeout_ms": 100}),
    )
    .await
    .expect("expect 调用失败");
    assert!(r.is_error.unwrap_or(false));
    assert!(
        structured_error_of(&r)
            .unwrap_or_default()
            .contains("未打开"),
        "expect data 的 newline=lf 应合法，收到参数错误: {r:?}"
    );

    // 6. 非法 read_mode → 协议级 invalid_params（含新模式的明确文案）
    let r = call(
        &client,
        "uart_read",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "read_mode": "base64", "timeout_ms": 100}),
    )
    .await;
    assert!(r.is_err(), "非法 read_mode 应触发 invalid_params");

    // 7. expect_send 的 reply 支持 newline：合法值应进入端口检查，不能被静默忽略。
    let r = call(
        &client,
        "uart_expect_send",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41", "reply": "x", "reply_mode": "text", "newline": "crlf"}),
    )
    .await
    .expect("expect_send 调用失败");
    assert!(r.is_error.unwrap_or(false));
    assert!(
        structured_error_of(&r)
            .unwrap_or_default()
            .contains("未打开"),
        "expect_send newline=crlf 应合法，收到参数错误: {r:?}"
    );

    // 8. 空 reply 不能靠 newline 绕过非空约束。
    let r = call(
        &client,
        "uart_expect_send",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41", "reply": "", "reply_mode": "text", "newline": "lf"}),
    )
    .await;
    assert!(r.is_err(), "空 reply + newline 仍应触发 invalid_params");

    // 9. expect_send 的 match_scope 同样只接受 buffer/new。
    let r = call(
        &client,
        "uart_expect_send",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41", "reply": "42", "match_scope": "future"}),
    )
    .await;
    assert!(
        r.is_err(),
        "非法 expect_send match_scope 应触发 invalid_params"
    );

    // 10. 所有参数对象都应拒绝未知字段，防止拼错或传给不支持的工具时静默忽略。
    let r = call(
        &client,
        "uart_configure",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "buffer_size": 65536}),
    )
    .await
    .expect("未知字段应返回工具级参数错误");
    assert!(
        r.is_error.unwrap_or(false),
        "configure 不支持的 buffer_size 应触发 invalid_params"
    );

    let r = call(
        &client,
        "uart_expect_send",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41", "reply": "42", "newline_typo": "crlf"}),
    )
    .await
    .expect("未知字段应返回工具级参数错误");
    assert!(r.is_error.unwrap_or(false), "未知字段应触发 invalid_params");

    client.cancel().await.expect("关闭客户端失败");
}

/// 创建临时文件（写入内容），返回 (路径, 清理句柄)；Drop 时删除。
struct TempFile {
    path: std::path::PathBuf,
}

impl TempFile {
    fn new(name: &str, content: &[u8]) -> Self {
        // 进程内唯一计数器避免并行测试临时文件命名冲突（as_nanos 可能碰撞）。
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ser2mcp-e2e-{name}-{}-{}",
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

/// uart_send_estimate / uart_send_file / uart_send_cancel 的参数校验与错误路径
/// （不依赖真实串口硬件）。
#[tokio::test]
async fn e2e_send_file_params_and_errors() {
    let client = connect().await.expect("连接 ser2mcp 失败");
    let tmp = TempFile::new("send", b"hello ser2mcp send file 0123456789");

    // 1. uart_send_estimate 正常路径：返回估算字段
    let r = call(
        &client,
        "uart_send_estimate",
        json!({"path": tmp.path, "mode": "text", "chunk_size": 64, "baudrate": 115200}),
    )
    .await
    .expect("estimate 调用失败");
    assert!(!r.is_error.unwrap_or(false), "estimate 应成功: {r:?}");
    let v = r.structured_content.expect("应有结构化返回");
    let file_size = tmp.path.metadata().unwrap().len();
    assert_eq!(v["file_size"], json!(file_size));
    assert_eq!(v["est_sent_bytes"], json!(file_size));
    assert_eq!(v["mode"], json!("text"));
    assert_eq!(v["est_chunks"], json!(file_size.div_ceil(64)));
    assert!(v["est_time_ms"].as_u64().unwrap() > 0);

    // 2. uart_send_estimate base64 模式：sent 字节应膨胀（含换行）
    let r = call(
        &client,
        "uart_send_estimate",
        json!({"path": tmp.path, "mode": "base64", "chunk_size": 64}),
    )
    .await
    .expect("estimate 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["mode"], json!("base64"));
    let sent = v["est_sent_bytes"].as_u64().unwrap();
    assert!(sent > file_size, "base64 应膨胀: {sent} vs {file_size}");
    assert_eq!(v["est_chunks"], json!(2), "编码片后还应有末尾换行片");

    // 3. uart_send_estimate 文件不存在 → 协议级 invalid_params
    let r = call(
        &client,
        "uart_send_estimate",
        json!({"path": "Z:\\ser2mcp\\definitely\\missing\\file.bin"}),
    )
    .await;
    assert!(r.is_err(), "文件不存在应触发 invalid_params: {r:?}");

    // 4. uart_send_estimate path 是目录 → 协议级错误
    let r = call(&client, "uart_send_estimate", json!({"path": "."})).await;
    assert!(r.is_err(), "目录应触发 invalid_params: {r:?}");

    // 5. uart_send_estimate chunk_size=0 / 非法 mode / baudrate=0 → 协议级错误
    for args in [
        json!({"path": tmp.path, "chunk_size": 0}),
        json!({"path": tmp.path, "mode": "hex"}),
        json!({"path": tmp.path, "baudrate": 0}),
    ] {
        let r = call(&client, "uart_send_estimate", args).await;
        assert!(r.is_err(), "非法参数应触发 invalid_params: {r:?}");
    }

    // 6. uart_send_file 未打开端口 + 合法文件 → 工具级错误（"未打开"）
    let r = call(
        &client,
        "uart_send_file",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "path": tmp.path, "max_duration_ms": 1000}),
    )
    .await
    .expect("send_file 调用失败");
    assert!(r.is_error.unwrap_or(false), "未打开端口应报错: {r:?}");
    assert!(
        structured_error_of(&r)
            .unwrap_or_default()
            .contains("未打开")
    );

    // 7. uart_send_file 文件不存在 / 目录 / chunk_size=0 / gap_ms 超上限 / 非法 mode
    //    → 协议级 invalid_params（文件校验先于端口校验）
    for (args, why) in [
        (
            json!({"port": "COM_SER2MCP_NONEXISTENT", "path": "Z:\\ser2mcp\\missing.bin"}),
            "文件不存在",
        ),
        (
            json!({"port": "COM_SER2MCP_NONEXISTENT", "path": "."}),
            "目录",
        ),
        (
            json!({"port": "COM_SER2MCP_NONEXISTENT", "path": tmp.path, "chunk_size": 0}),
            "chunk_size=0",
        ),
        (
            json!({"port": "COM_SER2MCP_NONEXISTENT", "path": tmp.path, "gap_ms": 999999999}),
            "gap_ms 超上限",
        ),
        (
            json!({"port": "COM_SER2MCP_NONEXISTENT", "path": tmp.path, "mode": "hex"}),
            "非法 mode",
        ),
        (
            json!({"port": "COM_SER2MCP_NONEXISTENT", "path": tmp.path, "max_duration_ms": 0}),
            "max_duration_ms=0",
        ),
    ] {
        let r = call(&client, "uart_send_file", args).await;
        assert!(r.is_err(), "{why} 应触发 invalid_params: {r:?}");
    }

    // 8. uart_send_cancel 未打开端口 → 工具级错误
    let r = call(
        &client,
        "uart_send_cancel",
        json!({"port": "COM_SER2MCP_NONEXISTENT"}),
    )
    .await
    .expect("send_cancel 调用失败");
    assert!(r.is_error.unwrap_or(false));

    client.cancel().await.expect("关闭客户端失败");
}
