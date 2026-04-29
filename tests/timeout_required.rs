
use serde_json::json;
use std::sync::Arc;
use rs_exec::*;

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
    assert!(result.is_err(), "expected error");
    let msg = result.err().unwrap().to_string();
    assert!(msg.contains("timeoutMs required"), "got: {}", msg);
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
    assert!(result.is_err());
}
