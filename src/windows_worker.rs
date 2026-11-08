//! SYSTEM 服务与目标 Windows 会话中的 SYSTEM 桌面 worker；此模块不创建网络监听。
#![allow(unsafe_code)]
use crate::desk::engine::rpc::{Reply, Transport};
use crate::json::{codes, ApiError};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle},
    ptr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use winapi::shared::{
    minwindef::DWORD,
    sddl::ConvertStringSecurityDescriptorToSecurityDescriptorW,
    winerror::{ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING},
};
use winapi::um::handleapi::DuplicateHandle;
use winapi::um::{
    fileapi::{CreateFileW, OPEN_EXISTING},
    handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
    ioapiset::CancelSynchronousIo,
    minwinbase::SECURITY_ATTRIBUTES,
    namedpipeapi::{ConnectNamedPipe, CreateNamedPipeW, PeekNamedPipe},
    processthreadsapi::{
        GetCurrentProcess, GetCurrentProcessId, GetCurrentThread, OpenProcess, OpenProcessToken,
    },
    securitybaseapi::{GetTokenInformation, IsWellKnownSid},
    synchapi::WaitForSingleObject,
    winbase::{
        GetNamedPipeClientProcessId, GetNamedPipeServerProcessId, LocalFree,
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, PIPE_NOWAIT, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, WAIT_OBJECT_0,
    },
    winnt::{
        TokenUser, WinLocalSystemSid, DUPLICATE_SAME_ACCESS, GENERIC_READ, GENERIC_WRITE, HANDLE,
        PROCESS_QUERY_LIMITED_INFORMATION, SYNCHRONIZE, TOKEN_QUERY, TOKEN_USER,
    },
    winuser::{
        CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_READOBJECTS, UOI_NAME,
    },
};

const IO_DEADLINE: Duration = Duration::from_secs(10);
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn engine_error() -> ApiError {
    ApiError::new(503, codes::DESK_ENGINE, "desktop worker connection failed")
}

/// 安全桌面没有已登录用户的剪贴板。先检查桌面，避免每个 Viewer 的读取
/// 都在共享 worker 管道内重试 OpenClipboard，拖住输入和视频确认。
pub(crate) fn require_clipboard_desktop() -> Result<(), ApiError> {
    let unavailable = || {
        ApiError::new(
            503,
            codes::DESK_ENGINE,
            "clipboard requires an active user desktop",
        )
    };
    let mut name = [0u16; 128];
    let mut needed = 0;
    unsafe {
        let desktop = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        if desktop.is_null() {
            return Err(unavailable());
        }
        let ok = GetUserObjectInformationW(
            desktop.cast(),
            UOI_NAME as i32,
            name.as_mut_ptr().cast(),
            std::mem::size_of_val(&name) as DWORD,
            &mut needed,
        );
        CloseDesktop(desktop);
        let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
        if ok == 0 || !String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("Default") {
            return Err(unavailable());
        }
    }
    Ok(())
}

fn is_system(pid: u32) -> io::Result<bool> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut token = ptr::null_mut();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        CloseHandle(process);
        if opened == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut size = 0;
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
        let mut bytes = vec![0usize; (size as usize + 7) / 8];
        let ok = GetTokenInformation(token, TokenUser, bytes.as_mut_ptr().cast(), size, &mut size);
        CloseHandle(token);
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(IsWellKnownSid(
            (*(bytes.as_ptr() as *const TOKEN_USER)).User.Sid,
            WinLocalSystemSid,
        ) != 0)
    }
}

fn duplicate_thread() -> io::Result<usize> {
    unsafe {
        let mut handle = ptr::null_mut();
        if DuplicateHandle(
            GetCurrentProcess(),
            GetCurrentThread(),
            GetCurrentProcess(),
            &mut handle,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        ) == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(handle as usize)
        }
    }
}

// 父端不提交等待对端的同步 I/O；整个握手/请求共享一个绝对期限。
// 这样 SYSTEM worker 被挂起时，父进程异常退出也不必等其恢复才能释放 Job。
struct Watchdog {
    active: Mutex<Option<Instant>>,
    failed: AtomicBool,
}
impl Watchdog {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active: Mutex::new(None),
            failed: AtomicBool::new(false),
        })
    }
    fn start(&self) -> io::Result<()> {
        if self.failed.load(Ordering::SeqCst) {
            return Err(invalid("IPC terminated"));
        }
        *self.active.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + IO_DEADLINE);
        Ok(())
    }
    fn check(&self) -> io::Result<()> {
        if self.failed.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "IPC terminated",
            ));
        }
        if self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.failed.store(true, Ordering::SeqCst);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "desktop IPC request deadline",
            ));
        }
        Ok(())
    }
    fn finish(&self) {
        self.active.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

struct NonblockingPipe {
    file: File,
    watchdog: Arc<Watchdog>,
}
impl Read for NonblockingPipe {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.watchdog.check()?;
            let mut available = 0;
            if unsafe {
                PeekNamedPipe(
                    self.file.as_raw_handle() as HANDLE,
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    &mut available,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if available > 0 {
                let count = bytes.len().min(available as usize);
                return self.file.read(&mut bytes[..count]);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Write for NonblockingPipe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.watchdog.check()?;
            match self.file.write(bytes) {
                Ok(0) => {}
                Err(error) if matches!(error.raw_os_error(), Some(code) if code == ERROR_NO_DATA as i32 || code == ERROR_PIPE_BUSY as i32) =>
                    {}
                result => return result,
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    // File 没有用户态缓冲；不能调用会等待客户端读尽数据的 FlushFileBuffers。
    fn flush(&mut self) -> io::Result<()> {
        self.watchdog.check()
    }
}

use crate::desk::engine::worker::{read_frame, write_frame};

/// 一次 worker 创建所使用的私有管道；仅SYSTEM且拒绝远程连接。
pub struct PipeEndpoint {
    file: File,
    name: String,
}
impl PipeEndpoint {
    /// 创建随机唯一管道，不允许抢占已有同名管道。
    pub fn new() -> io::Result<Self> {
        if !is_system(unsafe { GetCurrentProcessId() })? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "desktop broker requires SYSTEM",
            ));
        }
        let name = format!(
            "\\\\.\\pipe\\yoodesk-desktop-{}-{}",
            unsafe { GetCurrentProcessId() },
            uuid::Uuid::new_v4()
        );
        unsafe {
            let mut descriptor = ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide("D:P(A;;GA;;;SY)").as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut security: SECURITY_ATTRIBUTES = std::mem::zeroed();
            security.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD;
            security.lpSecurityDescriptor = descriptor;
            let handle = CreateNamedPipeW(
                wide(&name).as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                65536,
                65536,
                0,
                &mut security,
            );
            LocalFree(descriptor);
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                file: File::from_raw_handle(handle.cast()),
                name,
            })
        }
    }
    /// 公共随机管道名不是认证凭据。
    pub fn name(&self) -> &str {
        &self.name
    }
    /// 有截止的连接与一次性握手；PID必须是调用方实际创建的子进程。
    pub fn accept(self, worker_pid: u32, timeout: Duration) -> io::Result<Arc<dyn Transport>> {
        let deadline = Instant::now() + timeout;
        let handle = self.file.as_raw_handle() as HANDLE;
        loop {
            let connected = unsafe { ConnectNamedPipe(handle, ptr::null_mut()) };
            let error = io::Error::last_os_error();
            if connected == 0
                && !matches!(error.raw_os_error(), Some(code) if code == ERROR_PIPE_CONNECTED as i32 || code == ERROR_PIPE_LISTENING as i32 || code == ERROR_NO_DATA as i32)
            {
                return Err(error);
            }
            let mut pid = 0;
            if unsafe { GetNamedPipeClientProcessId(handle, &mut pid) } != 0 {
                if pid != worker_pid || !is_system(pid)? {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "desktop worker identity mismatch",
                    ));
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "desktop worker connect deadline",
                ));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let watchdog = Watchdog::new();
        let mut file = NonblockingPipe {
            file: self.file,
            watchdog: watchdog.clone(),
        };
        watchdog.start()?;
        let nonce: [u8; 32] = rand::random();
        let result = (|| {
            write_frame(&mut file, &json!({"v":1,"op":"hello"}), &nonce)?;
            let (hello, echo) = read_frame(&mut file)?;
            if hello != json!({"v":1,"op":"ready"}) || echo != nonce {
                return Err(invalid("desktop worker handshake"));
            }
            watchdog.check()?;
            Ok(())
        })();
        watchdog.finish();
        result?;
        Ok(Arc::new(PipeTransport {
            io: Mutex::new(Some(file)),
            watchdog,
            next: AtomicU64::new(1),
        }))
    }
}
struct PipeTransport {
    io: Mutex<Option<NonblockingPipe>>,
    watchdog: Arc<Watchdog>,
    next: AtomicU64,
}

impl Transport for PipeTransport {
    fn healthy(&self) -> bool {
        !self.watchdog.failed.load(Ordering::SeqCst)
    }
    fn close(&self) {
        self.watchdog.failed.store(true, Ordering::SeqCst);
    }
    fn request(&self, op: &str, args: Value) -> Result<Reply, ApiError> {
        let mut guard = self.io.lock().unwrap_or_else(|e| e.into_inner());
        let file = guard.as_mut().ok_or_else(engine_error)?;
        self.watchdog.start().map_err(|_| engine_error())?;
        let result = (|| {
            let id = self.next.fetch_add(1, Ordering::SeqCst);
            let bytes = serde_json::to_vec(&args).map_err(|_| invalid("IPC request"))?;
            write_frame(file, &json!({"v":1,"id":id,"op":op}), &bytes)?;
            let (metadata, bytes) = read_frame(file)?;
            if metadata["v"] != 1 || metadata["id"] != id {
                return Err(invalid("IPC response binding"));
            }
            if metadata["error"] == true {
                let value: Value =
                    serde_json::from_slice(&bytes).map_err(|_| invalid("IPC error"))?;
                return Ok(Err(ApiError::new(
                    value["status"].as_u64().unwrap_or(503) as u16,
                    codes::DESK_ENGINE,
                    value["message"]
                        .as_str()
                        .unwrap_or("desktop worker operation failed"),
                )));
            }
            let reply = if metadata["json"] == true {
                Reply {
                    value: serde_json::from_slice(&bytes).map_err(|_| invalid("IPC value"))?,
                    bytes: Vec::new(),
                }
            } else {
                Reply {
                    value: metadata["value"].clone(),
                    bytes,
                }
            };
            self.watchdog.check()?;
            Ok(Ok(reply))
        })();
        self.watchdog.finish();
        match result {
            Ok(result) => result,
            Err(_) => {
                self.watchdog.failed.store(true, Ordering::SeqCst);
                guard.take();
                Err(engine_error())
            }
        }
    }
}

/// 仅由CLI worker分支调用：验证服务父，执行原生桌面操作；永不启动HTTP。
pub fn run_worker(pipe: &str, parent_pid: u32) -> io::Result<()> {
    if !pipe.starts_with("\\\\.\\pipe\\yoodesk-desktop-")
        || !is_system(unsafe { GetCurrentProcessId() })?
        || !is_system(parent_pid)?
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "desktop worker requires SYSTEM parent and child",
        ));
    }
    let handle = unsafe {
        CreateFileW(
            wide(pipe).as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            ptr::null_mut(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_handle(handle.cast()) };
    let mut actual_parent = 0;
    if unsafe { GetNamedPipeServerProcessId(handle, &mut actual_parent) } == 0
        || actual_parent != parent_pid
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "desktop parent identity mismatch",
        ));
    }
    let (hello, nonce) = read_frame(&mut file)?;
    if hello != json!({"v":1,"op":"hello"}) || nonce.len() != 32 {
        return Err(invalid("desktop worker handshake"));
    }
    write_frame(&mut file, &json!({"v":1,"op":"ready"}), &nonce)?;
    let parent = unsafe { OpenProcess(SYNCHRONIZE, 0, parent_pid) };
    if parent.is_null() {
        return Err(io::Error::last_os_error());
    }
    let parent = parent as usize;
    let thread = duplicate_thread()?;
    let finished = Arc::new(AtomicBool::new(false));
    let monitor_finished = finished.clone();
    let monitor = std::thread::spawn(move || {
        while !monitor_finished.load(Ordering::SeqCst) {
            if unsafe { WaitForSingleObject(parent as HANDLE, 100) } == WAIT_OBJECT_0 {
                unsafe {
                    CancelSynchronousIo(thread as HANDLE);
                }
                break;
            }
        }
        unsafe {
            CloseHandle(parent as HANDLE);
            CloseHandle(thread as HANDLE);
        }
    });
    let result = crate::desk::engine::worker::serve(&mut file);
    finished.store(true, Ordering::SeqCst);
    let _ = monitor.join();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn client(name: &str) -> File {
        let handle = unsafe {
            CreateFileW(
                wide(name).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null_mut(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        unsafe { File::from_raw_handle(handle.cast()) }
    }
    #[test]
    fn connected_pipe_with_stalled_peer_hits_request_deadline() {
        // Test the production transport on a local pipe without the SYSTEM-only broker handshake.
        let name = format!(r"\\.\pipe\yoodesk-deadline-test-{}", uuid::Uuid::new_v4());
        let handle = unsafe {
            CreateNamedPipeW(
                wide(&name).as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                65536,
                65536,
                0,
                ptr::null_mut(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        let file = unsafe { File::from_raw_handle(handle.cast()) };
        let mut peer = client(&name);
        let watchdog = Watchdog::new();
        let transport = PipeTransport {
            io: Mutex::new(Some(NonblockingPipe {
                file,
                watchdog: watchdog.clone(),
            })),
            watchdog,
            next: AtomicU64::new(1),
        };
        let (received, waiting) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let peer = std::thread::spawn(move || {
            let (message, _) = read_frame(&mut peer).unwrap();
            received.send(message).unwrap();
            let _ = released.recv_timeout(IO_DEADLINE + Duration::from_secs(5));
        });
        let started = Instant::now();
        assert!(transport.request("displays", json!({})).is_err());
        let elapsed = started.elapsed();
        release.send(()).unwrap();
        peer.join().unwrap();
        assert_eq!(waiting.recv().unwrap()["op"], "displays");
        assert!(elapsed >= IO_DEADLINE && elapsed < IO_DEADLINE + Duration::from_secs(3));
        assert!(!transport.healthy());
        assert!(transport.io.lock().unwrap().is_none());
    }
    #[test]
    #[ignore = "requires a dedicated SYSTEM process on the Windows VM"]
    fn parent_pipe_transfers_large_frames_with_nonblocking_partial_io() {
        let endpoint = PipeEndpoint::new().unwrap();
        let name = endpoint.name().to_owned();
        let (release, waiting) = std::sync::mpsc::channel();
        let peer = std::thread::spawn(move || {
            let mut file = client(&name);
            let (_, nonce) = read_frame(&mut file).unwrap();
            write_frame(&mut file, &json!({"v":1,"op":"ready"}), &nonce).unwrap();
            let (request, bytes) = read_frame(&mut file).unwrap();
            let args: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(args["payload"].as_str().unwrap().len(), 2 << 20);
            write_frame(
                &mut file,
                &json!({"v":1,"id":request["id"],"value":{"length":2 << 20}}),
                &vec![0x5a; 2 << 20],
            )
            .unwrap();
            // 保持客户端存活直到父完成读取，避免把正常 EOF 与本测试混在一起。
            let _ = waiting.recv_timeout(Duration::from_secs(5));
        });
        let transport = endpoint
            .accept(std::process::id(), Duration::from_secs(2))
            .unwrap();
        let reply = transport
            .request("large", json!({"payload":"x".repeat(2 << 20)}))
            .unwrap();
        assert_eq!(reply.bytes, vec![0x5a; 2 << 20]);
        assert_eq!(reply.value["length"], 2 << 20);
        release.send(()).unwrap();
        peer.join().unwrap();
    }
    #[test]
    #[ignore = "requires a dedicated SYSTEM process on the Windows VM"]
    fn protocol_failure_marks_transport_unhealthy_while_peer_is_alive() {
        let endpoint = PipeEndpoint::new().unwrap();
        let name = endpoint.name().to_owned();
        let (release, waiting) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut peer = client(&name);
            let (_, nonce) = read_frame(&mut peer).unwrap();
            write_frame(&mut peer, &json!({"v":1,"op":"ready"}), &nonce).unwrap();
            let (request, _) = read_frame(&mut peer).unwrap();
            write_frame(
                &mut peer,
                &json!({"v":1,"id":request["id"],"error":true}),
                br#"{"status":400,"message":"business rejection"}"#,
            )
            .unwrap();
            let (request, _) = read_frame(&mut peer).unwrap();
            write_frame(
                &mut peer,
                &json!({"v":1,"id":request["id"].as_u64().unwrap()+1,"json":true}),
                b"null",
            )
            .unwrap();
            let _ = waiting.recv_timeout(Duration::from_secs(5));
        });
        let transport = endpoint
            .accept(std::process::id(), Duration::from_secs(2))
            .unwrap();
        assert_eq!(
            transport.request("test", json!({})).err().unwrap().status,
            400
        );
        assert!(
            transport.healthy(),
            "business errors leave the worker usable"
        );
        assert!(transport.request("test", json!({})).is_err());
        assert!(!transport.healthy());
        assert!(
            !worker.is_finished(),
            "peer remains alive while transport is poisoned"
        );
        release.send(()).unwrap();
        worker.join().unwrap();
    }
    #[test]
    #[ignore = "requires a dedicated SYSTEM process on the Windows VM"]
    fn system_pipe_binds_peer_pid_and_cancels_pending_io() {
        let endpoint = PipeEndpoint::new().unwrap();
        let name = endpoint.name().to_owned();
        let wrong = std::thread::spawn(move || {
            let file = client(&name);
            std::thread::sleep(Duration::from_millis(100));
            file
        });
        assert_eq!(
            endpoint
                .accept(std::process::id() + 1, Duration::from_secs(2))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        drop(wrong.join().unwrap());
        let endpoint = PipeEndpoint::new().unwrap();
        let name = endpoint.name().to_owned();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut file = client(&name);
            let (hello, nonce) = read_frame(&mut file).unwrap();
            assert_eq!(hello["v"], 1);
            assert_eq!(nonce.len(), 32);
            write_frame(&mut file, &json!({"v":1,"op":"ready"}), &nonce).unwrap();
            let _ = read_frame(&mut file).unwrap();
            entered_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });
        let transport = endpoint
            .accept(std::process::id(), Duration::from_secs(2))
            .unwrap();
        let requester = transport.clone();
        let request = std::thread::spawn(move || requester.request("displays", json!({})));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let start = Instant::now();
        transport.close();
        assert!(request.join().unwrap().is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert!(transport.request("displays", json!({})).is_err());
    }
}
