use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn execute_applies_default_timeout_when_missing() {
    let store = rs_exec::background_tasks::BackgroundTaskStore::new();
    let state = Arc::new(rs_exec::runner::AppState {
        store,
        active: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    });
    let result = rs_exec::rpc::handle_rpc(&state, "execute", &json!({
        "code": "console.log(1)", "runtime": "nodejs", "workingDirectory": "."
    })).await;
    match result {
        Ok(_) => {}
        Err(e) => {
            let msg = e.to_string();
            assert!(!msg.contains("timeoutMs required"), "should not reject missing timeoutMs: {}", msg);
        }
    }
}

#[tokio::test]
async fn execute_applies_default_timeout_when_zero() {
    let store = rs_exec::background_tasks::BackgroundTaskStore::new();
    let state = Arc::new(rs_exec::runner::AppState {
        store,
        active: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    });
    let result = rs_exec::rpc::handle_rpc(&state, "execute", &json!({
        "code": "console.log(1)", "runtime": "nodejs", "workingDirectory": ".",
        "timeoutMs": 0
    })).await;
    if let Err(e) = result {
        assert!(!e.to_string().contains("timeoutMs required"));
    }
}
