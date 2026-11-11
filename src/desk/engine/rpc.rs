//! 服务父进程的桌面代理。首次安装后永久禁止退回父进程原生桌面实现。
use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
static REQUESTS: AtomicUsize = AtomicUsize::new(0);
static MEDIA_POLLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(any(test, not(any(target_os = "linux", target_os = "macos"))))]
static FILE_REQUESTS: AtomicUsize = AtomicUsize::new(0);
struct RequestSlot(&'static AtomicUsize);
impl RequestSlot {
    fn acquire(
        counter: &'static AtomicUsize,
        limit: usize,
        message: &'static str,
    ) -> Result<Self, ApiError> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < limit).then(|| active + 1)
            })
            .map(|_| Self(counter))
            .map_err(|_| ApiError::new(429, codes::CAPACITY_EXCEEDED, message).retryable(true))
    }
}
impl Drop for RequestSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

/// 原始媒体与小型元数据分开发送，避免 JSON/base64 放大。
pub struct Reply {
    /// 响应元数据。
    pub value: Value,
    /// 原始编码数据。
    pub bytes: Vec<u8>,
}

/// 单个已验证身份的 worker 代次；实现不得重试命令。
pub trait Transport: Send + Sync {
    /// 仅连接/协议/总期限失效是终态；正常业务错误不能使连接失效。
    fn healthy(&self) -> bool {
        true
    }
    /// 有界同步请求，错误必须关闭该代连接。
    fn request(&self, op: &str, args: Value) -> Result<Reply, ApiError>;
    /// 立即取消正在执行的I/O；服务停止/换会话不等待远端响应。
    fn close(&self);
}

#[derive(Default)]
struct State {
    enabled: bool,
    generation: u64,
    transport: Option<Arc<dyn Transport>>,
    connections: HashMap<i32, u64>,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
fn state() -> &'static Mutex<State> {
    STATE.get_or_init(Default::default)
}

/// 安装/撤销某代 worker。None 是明确不可用，绝不在服务父进程初始化引擎。
pub fn set_transport(transport: Option<Arc<dyn Transport>>) {
    let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
    state.enabled = true;
    state.generation = state.generation.wrapping_add(1);
    let old = std::mem::replace(&mut state.transport, transport);
    state.connections.clear();
    drop(state);
    if let Some(old) = old {
        old.close();
    }
}

/// 当前进程是否已进入服务代理模式。
pub fn enabled() -> bool {
    state().lock().unwrap_or_else(|e| e.into_inner()).enabled
}
/// worker即使仍存活，已损坏的IPC也必须由宿主回收并重建。
pub fn worker_healthy() -> bool {
    state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .transport
        .as_ref()
        .is_some_and(|transport| transport.healthy())
}
fn unavailable() -> ApiError {
    ApiError::new(
        503,
        codes::DESK_ENGINE,
        "desktop worker unavailable or replaced",
    )
}

/// 非服务进程返回 None；服务请求成功后再次核验代次，拒绝迟到结果。
pub fn call(op: &str, args: Value) -> Option<Result<Reply, ApiError>> {
    call_expected(None, op, args)
}
fn call_expected(expected: Option<u64>, op: &str, args: Value) -> Option<Result<Reply, ApiError>> {
    let guard = state().lock().unwrap_or_else(|e| e.into_inner());
    if !guard.enabled {
        return None;
    }
    let generation = guard.generation;
    if expected.is_some_and(|expected| expected != generation) {
        return Some(Err(unavailable()));
    }
    let transport = match guard.transport.clone() {
        Some(t) => t,
        None => return Some(Err(unavailable())),
    };
    if let Some(conn) = args.get("conn").and_then(Value::as_i64) {
        if conn != 0
            && op != "subscribe"
            && guard.connections.get(&(conn as i32)) != Some(&generation)
        {
            return Some(Err(unavailable()));
        }
    }
    drop(guard);
    // 采集轮询可稍后再读；不能占用输入、解码确认和资源回收的名额。
    let slots = if matches!(op, "poll" | "poll_cursor" | "poll_audio") {
        RequestSlot::acquire(&MEDIA_POLLS, 8, "desktop media poll limit")
    } else {
        RequestSlot::acquire(&REQUESTS, 32, "desktop worker request limit")
    };
    let _slot = match slots {
        Ok(slot) => slot,
        Err(err) => return Some(Err(err)),
    };
    let result = transport.request(op, args.clone());
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    if guard.generation != generation {
        return Some(Err(unavailable()));
    }
    if result.is_ok() {
        if op == "subscribe" {
            guard
                .connections
                .insert(args["conn"].as_i64().unwrap_or(0) as i32, generation);
        }
        if op == "unsubscribe_conn" {
            guard
                .connections
                .remove(&(args["conn"].as_i64().unwrap_or(0) as i32));
        }
        if op == "unsubscribe" {
            guard.connections.clear();
        }
    }
    Some(result)
}

fn generation() -> u64 {
    state().lock().unwrap_or_else(|e| e.into_inner()).generation
}
fn call_generation(expected: u64, op: &str, args: Value) -> Result<Reply, ApiError> {
    call_expected(Some(expected), op, args).ok_or_else(unavailable)?
}

/// 建立仅属于当前 worker 的输入句柄，切代后释放也不会打到新用户。
pub fn input() -> Result<Box<dyn InputSink>, ApiError> {
    let generation = generation();
    let reply = call_generation(generation, "input.create", json!({}))?;
    Ok(Box::new(RemoteInput {
        generation,
        id: reply.value.as_u64().ok_or_else(unavailable)?,
    }))
}
struct RemoteInput {
    generation: u64,
    id: u64,
}
impl InputSink for RemoteInput {
    fn mouse(&mut self, event: MouseInput) -> Result<(), ApiError> {
        call_generation(
            self.generation,
            "input.mouse",
            json!({"id":self.id,"event":event}),
        )
        .map(|_| ())
    }
    fn key(&mut self, event: KeyInput) -> Result<(), ApiError> {
        call_generation(
            self.generation,
            "input.key",
            json!({"id":self.id,"event":event}),
        )
        .map(|_| ())
    }
    fn release_all(&mut self) {
        let _ = call_generation(self.generation, "input.release", json!({"id":self.id}));
    }
}
impl Drop for RemoteInput {
    fn drop(&mut self) {
        // release_all 用于失焦/只看切换，句柄必须仍可复用；只有所有者销毁才归还额度。
        let _ = call_generation(self.generation, "input.drop", json!({"id":self.id}));
    }
}

/// 剪贴板对象同样固定在创建它的用户会话代次。
pub fn clipboard() -> Box<dyn crate::desk::Clipboard> {
    Box::new(RemoteClipboard(generation()))
}
struct RemoteClipboard(u64);
impl crate::desk::Clipboard for RemoteClipboard {
    fn read(&mut self) -> Result<Option<crate::desk::ClipboardContent>, ApiError> {
        let result = call_generation(
            self.0,
            "endpoint",
            json!({"conn":0,"value":{"action":"clipboard.read"}}),
        )?;
        if result.value.is_null() {
            Ok(None)
        } else {
            crate::http::clipboard_from_json(&result.value).map(Some)
        }
    }
    fn write(&mut self, content: &crate::desk::ClipboardContent) -> Result<(), ApiError> {
        call_generation(self.0, "endpoint", json!({"conn":0,"value":{"action":"clipboard.write","content":crate::http::clipboard_to_json(content)}})).map(|_| ())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
extern "C" {
    fn yd_engine_file_endpoint(
        conn: i32,
        input: *const u8,
        len: usize,
        out: *mut u8,
        cap: usize,
    ) -> i32;
    fn yd_engine_file_close(conn: i32) -> i32;
}

pub(super) fn close_files(conn: i32) {
    // Linux文件句柄属于普通用户worker，由unsubscribe_conn/worker退出回收。
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let _ = conn;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = unsafe { yd_engine_file_close(conn) };
}
pub(super) fn files(conn: i32, value: &Value) -> Result<Value, ApiError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        return call("endpoint", json!({"conn":conn,"value":value}))
            .ok_or_else(unavailable)?
            .map(|reply| reply.value);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    files_operation(conn, || files_native(conn, value), || close_files(conn))
}
// 原生文件IO由独立任务锁串行，不可持全局代理锁；慢文件系统不能阻塞切代或媒体。
#[cfg(any(test, not(any(target_os = "linux", target_os = "macos"))))]
fn files_operation(
    conn: i32,
    operation: impl FnOnce() -> Result<Value, ApiError>,
    cleanup: impl FnOnce(),
) -> Result<Value, ApiError> {
    let guard = state().lock().unwrap_or_else(|e| e.into_inner());
    if !guard.enabled || guard.connections.get(&conn) != Some(&guard.generation) {
        return Err(unavailable());
    }
    let generation = guard.generation;
    drop(guard);
    // Windows 文件在服务进程执行，不占用 worker 管道，也不与该管道共享名额。
    let _slot = RequestSlot::acquire(&FILE_REQUESTS, 8, "desktop file request limit")?;
    let result = operation();
    let guard = state().lock().unwrap_or_else(|e| e.into_inner());
    let current =
        guard.generation == generation && guard.connections.get(&conn) == Some(&generation);
    drop(guard);
    if !current {
        // conn在父进程内单调分配，不跨代复用；只回收此旧conn，不清空其他代任务。
        cleanup();
        return Err(unavailable());
    }
    result
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn files_native(conn: i32, value: &Value) -> Result<Value, ApiError> {
    let input = serde_json::to_vec(value).map_err(|_| unavailable())?;
    if input.len() > 24 << 20 {
        return Err(ApiError::new(
            413,
            codes::INVALID_REQUEST,
            "file request too large",
        ));
    }
    let mut bytes = vec![0; 24 << 20];
    let count = unsafe {
        yd_engine_file_endpoint(
            conn,
            input.as_ptr(),
            input.len(),
            bytes.as_mut_ptr(),
            bytes.len(),
        )
    };
    if count <= 0 || count as usize > bytes.len() {
        return Err(unavailable());
    }
    let envelope: Value =
        serde_json::from_slice(&bytes[..count as usize]).map_err(|_| unavailable())?;
    super::decode_endpoint(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Fake {
        requests: AtomicUsize,
        closed: AtomicUsize,
    }
    impl Transport for Fake {
        fn request(&self, op: &str, _: Value) -> Result<Reply, ApiError> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            Ok(Reply {
                value: if op == "input.create" {
                    json!(1)
                } else {
                    Value::Null
                },
                bytes: Vec::new(),
            })
        }
        fn close(&self) {
            self.closed.fetch_add(1, Ordering::SeqCst);
        }
    }
    // 永久启用进程代理模式，因此必须单独进程显式执行，不能污染普通引擎回归。
    #[test]
    #[ignore = "run in a dedicated process: changes permanent service proxy mode"]
    fn replaced_worker_rejects_old_connections_inputs_and_clipboard() {
        let first = Arc::new(Fake {
            requests: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
        });
        set_transport(Some(first.clone()));
        call("subscribe", json!({"conn":81})).unwrap().unwrap();
        let mut input = input().unwrap();
        let mut clipboard = clipboard();
        let second = Arc::new(Fake {
            requests: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
        });
        set_transport(Some(second.clone()));
        assert_eq!(first.closed.load(Ordering::SeqCst), 1);
        assert!(call("ack_video", json!({"conn":81,"pts":1}))
            .unwrap()
            .is_err());
        assert!(input.key(KeyInput::Text("must not replay".into())).is_err());
        assert!(clipboard.read().is_err());
        input.release_all();
        assert_eq!(second.requests.load(Ordering::SeqCst), 0);
        set_transport(None);
        assert!(enabled());
        assert!(call("displays", json!({})).unwrap().is_err());
        assert_eq!(second.closed.load(Ordering::SeqCst), 1);
    }
    #[test]
    #[ignore = "run in a dedicated process: changes permanent service proxy mode"]
    fn slow_file_io_does_not_block_worker_replacement_and_rejects_late_result() {
        use std::sync::mpsc;
        use std::time::Duration;
        let first = Arc::new(Fake {
            requests: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
        });
        set_transport(Some(first));
        call("subscribe", json!({"conn":91})).unwrap().unwrap();
        let (entered, started) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let cleaned = Arc::new(AtomicUsize::new(0));
        let worker_cleaned = cleaned.clone();
        let file = std::thread::spawn(move || {
            files_operation(
                91,
                || {
                    entered.send(()).unwrap();
                    resume.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(json!({"late":true}))
                },
                || {
                    worker_cleaned.fetch_add(1, Ordering::SeqCst);
                },
            )
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done, replaced) = mpsc::channel();
        let replace = std::thread::spawn(move || {
            set_transport(None);
            done.send(()).unwrap();
        });
        let cancelled = replaced.recv_timeout(Duration::from_secs(1));
        // 先放行慢操作再断言，回归失败也不会留下死锁测试线程。
        release.send(()).unwrap();
        replace.join().unwrap();
        assert!(
            cancelled.is_ok(),
            "slow native IO must not hold the global state lock"
        );
        assert!(file.join().unwrap().is_err());
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
    }

    #[test]
    #[ignore = "run in a dedicated process: changes permanent service proxy mode"]
    fn full_media_poll_lane_does_not_reject_ack_input_or_native_file_read() {
        use std::sync::{mpsc, Barrier};
        use std::time::Duration;

        struct PausedPolls {
            entered: mpsc::Sender<()>,
            resume: Barrier,
        }
        impl Transport for PausedPolls {
            fn request(&self, op: &str, _: Value) -> Result<Reply, ApiError> {
                if op == "poll" {
                    self.entered.send(()).unwrap();
                    self.resume.wait();
                }
                Ok(Reply {
                    value: if op == "input.create" {
                        json!(1)
                    } else {
                        Value::Null
                    },
                    bytes: Vec::new(),
                })
            }
            fn close(&self) {}
        }

        let (entered, started) = mpsc::channel();
        let transport = Arc::new(PausedPolls {
            entered,
            resume: Barrier::new(9),
        });
        set_transport(Some(transport.clone()));
        call("subscribe", json!({"conn":81})).unwrap().unwrap();
        let polls: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(|| call("poll", json!({"conn":81})).unwrap()))
            .collect();
        for _ in &polls {
            started.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        // 用另一种轮询验证相同媒体配额，避免回归时测试也阻塞在模拟的 poll 上。
        let overflow = call("poll_cursor", json!({"conn":81})).unwrap();
        let ack = call("ack_video", json!({"conn":81,"pts":1})).unwrap();
        let key = input().and_then(|mut sink| sink.key(KeyInput::Text("test".into())));
        let file = files_operation(81, || Ok(json!("block")), || {});
        transport.resume.wait();
        for poll in polls {
            assert!(poll.join().unwrap().is_ok());
        }
        assert!(matches!(overflow, Err(err) if err.status == 429));
        assert!(ack.is_ok());
        assert!(key.is_ok());
        assert_eq!(file.unwrap(), json!("block"));
        set_transport(None);
    }
}
