#[link(wasm_import_module = "env")]
extern "C" {
    pub fn host_kv_get(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32) -> u64;
    pub fn host_kv_put(ns_ptr: *const u8, ns_len: u32, key_ptr: *const u8, key_len: u32, val_ptr: *const u8, val_len: u32) -> u32;
    pub fn host_kv_query(ns_ptr: *const u8, ns_len: u32, query_ptr: *const u8, query_len: u32) -> u64;
    pub fn host_exec_js(code_ptr: *const u8, code_len: u32, opts_ptr: *const u8, opts_len: u32) -> u64;
    pub fn host_log(level: u32, msg_ptr: *const u8, msg_len: u32) -> u32;
    pub fn host_now_ms() -> i64;
}

#[inline]
pub fn unpack_u64(v: u64) -> (u32, u32) {
    ((v & 0xFFFF_FFFF) as u32, (v >> 32) as u32)
}

unsafe fn take_bytes(packed: u64) -> Vec<u8> {
    let (ptr, len) = unpack_u64(packed);
    if ptr == 0 || len == 0 {
        return Vec::new();
    }
    Vec::from_raw_parts(ptr as *mut u8, len as usize, len as usize)
}

pub fn log(msg: &str) {
    let _ = unsafe { host_log(1, msg.as_ptr(), msg.len() as u32) };
}

pub fn now_ms() -> i64 {
    unsafe { host_now_ms() }
}

pub fn kv_get(namespace: &str, key: &str) -> Option<Vec<u8>> {
    let packed = unsafe {
        host_kv_get(
            namespace.as_ptr(),
            namespace.len() as u32,
            key.as_ptr(),
            key.len() as u32,
        )
    };
    let bytes = unsafe { take_bytes(packed) };
    if bytes.is_empty() { None } else { Some(bytes) }
}

pub fn kv_put(namespace: &str, key: &str, val: &[u8]) -> bool {
    let r = unsafe {
        host_kv_put(
            namespace.as_ptr(),
            namespace.len() as u32,
            key.as_ptr(),
            key.len() as u32,
            val.as_ptr(),
            val.len() as u32,
        )
    };
    r != 0
}

pub fn kv_query(namespace: &str, query: &str) -> Vec<u8> {
    let packed = unsafe {
        host_kv_query(
            namespace.as_ptr(),
            namespace.len() as u32,
            query.as_ptr(),
            query.len() as u32,
        )
    };
    unsafe { take_bytes(packed) }
}

pub fn exec_js(code: &str, opts_json: &str) -> (u32, Vec<u8>) {
    let packed = unsafe {
        host_exec_js(
            code.as_ptr(),
            code.len() as u32,
            opts_json.as_ptr(),
            opts_json.len() as u32,
        )
    };
    let bytes = unsafe { take_bytes(packed) };
    let status = if bytes.is_empty() { 1 } else { 0 };
    (status, bytes)
}
