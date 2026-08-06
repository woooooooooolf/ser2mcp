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

    // 1. 工具注册：应包含全部 11 个工具
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
            "mode": "base64",
            "timeout_ms": 100
        }),
    )
    .await;
    assert!(r.is_err(), "非法 mode 应触发 invalid_params");

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

    // 17. 参数校验：timeout_ms 超上限 → 协议级 invalid_params
    let r = call(
        &client,
        "uart_expect",
        json!({"port": "COM_SER2MCP_NONEXISTENT", "pattern": "41 42", "timeout_ms": 999999999}),
    )
    .await;
    assert!(r.is_err(), "timeout_ms 超上限应触发 invalid_params");

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
        json!({"port": "COM_SER2MCP_NONEXISTENT", "mode": "text-escaped", "timeout_ms": 100}),
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
        json!({"port": "COM_SER2MCP_NONEXISTENT", "mode": "base64", "timeout_ms": 100}),
    )
    .await;
    assert!(r.is_err(), "非法 read_mode 应触发 invalid_params");

    client.cancel().await.expect("关闭客户端失败");
}
