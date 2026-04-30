use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn execute_rejects_missing_timeout() {
    let store = rs_exec::background_tasks::BackgroundTaskStore::new();
    let state = Arc::new(rs_exec::runner::AppState {
        store,
        active: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    });
    let result = rs_exec::rpc::handle_rpc(&state, "execute", &json!({
        "code": "console.log(1)", "runtime": "nodejs", "workingDirectory": "."
    })).await;
    let err = result.expect_err("missing timeoutMs must be rejected");
    assert!(err.to_string().contains("timeoutMs required"), "expected mandatory-timeout error, got: {}", err);
}

#[tokio::test]
async fn execute_rejects_zero_timeout() {
    let store = rs_exec::background_tasks::BackgroundTaskStore::new();
    let state = Arc::new(rs_exec::runner::AppState {
        store,
        active: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    });
    let result = rs_exec::rpc::handle_rpc(&state, "execute", &json!({
        "code": "console.log(1)", "runtime": "nodejs", "workingDirectory": ".",
        "timeoutMs": 0
    })).await;
    let err = result.expect_err("zero timeoutMs must be rejected");
    assert!(err.to_string().contains("timeoutMs required"), "expected mandatory-timeout error, got: {}", err);
}
