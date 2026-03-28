use std::{
    env, fs,
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    time::Duration,
};
use serde_json::{json, Value};

fn port_file() -> PathBuf {
    env::temp_dir().join("glootie-runner.port")
}

fn get_port() -> anyhow::Result<u16> {
    Ok(fs::read_to_string(port_file())?.trim().parse::<u16>()?)
}

fn http_post(port: u16, path: &str, body: &str, timeout_ms: u64) -> anyhow::Result<String> {
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse()?,
        Duration::from_millis(timeout_ms.min(5000)),
    )?;
    stream.set_read_timeout(Some(Duration::from_millis(timeout_ms)))?;
    stream.set_write_timeout(Some(Duration::from_millis(5000)))?;
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path, port, body.len(), body
    );
    stream.write_all(req.as_bytes())?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp)?;
    let body_start = resp.find("\r\n\r\n").map(|i| i + 4).unwrap_or(resp.len());
    Ok(resp[body_start..].to_string())
}

fn http_get(port: u16, path: &str) -> anyhow::Result<u16> {
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse()?,
        Duration::from_millis(2000),
    )?;
    stream.set_read_timeout(Some(Duration::from_millis(2000)))?;
    let req = format!("GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n", path, port);
    stream.write_all(req.as_bytes())?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp)?;
    let status = resp.split_whitespace().nth(1).and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
    Ok(status)
}

pub fn rpc_call_sync(port: u16, method: &str, params: Value, timeout_ms: u64) -> anyhow::Result<Value> {
    let body = json!({ "method": method, "params": params }).to_string();
    let resp = http_post(port, "/rpc", &body, timeout_ms)?;
    let val: Value = serde_json::from_str(&resp)?;
    if let Some(e) = val.get("error") {
        let msg = e.get("message").and_then(|v| v.as_str()).unwrap_or(&e.to_string()).to_string();
        return Err(anyhow::anyhow!("{}", msg));
    }
    Ok(val["result"].clone())
}

pub async fn rpc_call(method: &str, params: Value, timeout_ms: u64) -> anyhow::Result<Value> {
    let port = get_port()?;
    let method = method.to_string();
    tokio::task::spawn_blocking(move || rpc_call_sync(port, &method, params, timeout_ms)).await?
}

pub async fn health_check() -> bool {
    let Ok(port) = get_port() else { return false };
    tokio::task::spawn_blocking(move || http_get(port, "/health").map(|s| s == 200).unwrap_or(false))
        .await
        .unwrap_or(false)
}
