#![allow(dead_code)]
extern "C" {
    pub fn host_kv_get(key_ptr: *const u8, key_len: u32, out_ptr: *mut u8, out_cap: u32) -> u64;
    pub fn host_kv_put(key_ptr: *const u8, key_len: u32, val_ptr: *const u8, val_len: u32) -> u32;
    pub fn host_kv_query(prefix_ptr: *const u8, prefix_len: u32, out_ptr: *mut u8, out_cap: u32) -> u64;
    pub fn host_exec_js(code_ptr: *const u8, code_len: u32, opts_ptr: *const u8, opts_len: u32) -> u64;
    pub fn host_now_ms() -> u64;
    pub fn host_log(msg_ptr: *const u8, msg_len: u32);
}

pub fn pack_u64(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | (lo as u64)
}

pub fn unpack_u64(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, (v & 0xFFFF_FFFF) as u32)
}

pub fn log(msg: &str) {
    unsafe { host_log(msg.as_ptr(), msg.len() as u32); }
}

pub fn now_ms() -> u64 {
    unsafe { host_now_ms() }
}

pub fn kv_get(key: &str) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 65536];
    let packed = unsafe { host_kv_get(key.as_ptr(), key.len() as u32, buf.as_mut_ptr(), buf.len() as u32) };
    let (ok, len) = unpack_u64(packed);
    if ok == 0 { return None; }
    buf.truncate(len as usize);
    Some(buf)
}

pub fn kv_put(key: &str, val: &[u8]) -> bool {
    let r = unsafe { host_kv_put(key.as_ptr(), key.len() as u32, val.as_ptr(), val.len() as u32) };
    r != 0
}

pub fn kv_query(prefix: &str) -> Vec<u8> {
    let mut buf = vec![0u8; 262144];
    let packed = unsafe { host_kv_query(prefix.as_ptr(), prefix.len() as u32, buf.as_mut_ptr(), buf.len() as u32) };
    let (ok, len) = unpack_u64(packed);
    if ok == 0 { return Vec::new(); }
    buf.truncate(len as usize);
    buf
}

pub fn exec_js(code: &str, opts_json: &str) -> (u32, Vec<u8>) {
    let mut buf = vec![0u8; 262144];
    let packed = unsafe { host_exec_js(code.as_ptr(), code.len() as u32, opts_json.as_ptr(), opts_json.len() as u32) };
    let (status, len) = unpack_u64(packed);
    buf.truncate(0);
    if len > 0 {
        let mut out = vec![0u8; len as usize];
        let _ = unsafe { host_kv_get(b"__exec_last".as_ptr(), 11, out.as_mut_ptr(), out.len() as u32) };
        return (status, out);
    }
    (status, buf)
}
