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

    // 1. 工具注册：应包含全部 9 个工具
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
    let r = call(&client, "uart_available", json!({}))
        .await
        .expect("available 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["open"], json!(false));

    // 4. uart_read 未打开 → 工具级错误（结构化 error）
    let r = call(&client, "uart_read", json!({"timeout_ms": 100}))
        .await
        .expect("read 调用失败");
    assert!(r.is_error.unwrap_or(false));
    assert!(
        structured_error_of(&r)
            .unwrap_or_default()
            .contains("未打开")
    );

    // 5. uart_write 未打开 → 工具级错误
    let r = call(&client, "uart_write", json!({"data": "41"}))
        .await
        .expect("write 调用失败");
    assert!(r.is_error.unwrap_or(false));

    // 6. 参数校验：非法 hex → 协议级 invalid_params（call_tool 返回 Err）
    let r = call(&client, "uart_write", json!({"data": "zz"})).await;
    assert!(r.is_err(), "非法 hex 应触发 invalid_params: {r:?}");

    // 7. 参数校验：非法 mode → 协议级错误
    let r = call(
        &client,
        "uart_read",
        json!({"mode": "base64", "timeout_ms": 100}),
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
    let r = call(&client, "uart_available", json!({}))
        .await
        .expect("available 调用失败");
    let v = r.structured_content.expect("应有结构化返回");
    assert_eq!(v["open"], json!(false));

    client.cancel().await.expect("关闭客户端失败");
}
