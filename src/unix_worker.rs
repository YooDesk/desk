//! Linux用户桌面worker与独立root helper：私有Unix socket、预定UID/PID与有界协议。
#![allow(unsafe_code)]
use crate::desk::engine::{
    rpc::{Reply, Transport},
    worker::{read_frame, write_frame},
};
use crate::json::{codes, ApiError};
use serde_json::{json, Value};
use std::{
    fs, io,
    net::Shutdown,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
unsafe extern "C" {
    fn geteuid() -> u32;
    fn getegid() -> u32;
    fn chown(path: *const std::ffi::c_char, uid: u32, gid: u32) -> i32;
    fn getsockopt(
        fd: i32,
        level: i32,
        name: i32,
        value: *mut std::ffi::c_void,
        length: *mut u32,
    ) -> i32;
}
#[repr(C)]
struct Credentials {
    pid: i32,
    uid: u32,
    gid: u32,
}
fn verify(stream: &UnixStream, pid: u32, uid: u32) -> io::Result<()> {
    let mut credentials = Credentials {
        pid: 0,
        uid: 1,
        gid: 1,
    };
    let mut size = std::mem::size_of::<Credentials>() as u32;
    if unsafe {
        getsockopt(
            stream.as_raw_fd(),
            1,
            17,
            (&mut credentials as *mut Credentials).cast(),
            &mut size,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if size as usize != std::mem::size_of::<Credentials>()
        || credentials.pid as u32 != pid
        || credentials.uid != uid
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "desktop IPC peer identity mismatch",
        ));
    }
    Ok(())
}
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "desktop IPC protocol")
}
fn engine_error() -> ApiError {
    ApiError::new(503, codes::DESK_ENGINE, "desktop worker connection failed")
}

// 一个握手/请求共享同一绝对截止时间。SO_RCVTIMEO/SO_SNDTIMEO单独使用只约束
// 每次syscall，逐字节发送会不断重置计时；包装层在每次读写前扣除已耗时间。
struct DeadlineIo<'a> {
    stream: &'a mut UnixStream,
    deadline: Instant,
}
impl<'a> DeadlineIo<'a> {
    fn new(stream: &'a mut UnixStream, timeout: Duration) -> Self {
        Self {
            stream,
            deadline: Instant::now() + timeout,
        }
    }
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "desktop IPC total deadline"))
    }
}
impl io::Read for DeadlineIo<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        io::Read::read(self.stream, bytes)
    }
}
impl io::Write for DeadlineIo<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        io::Write::write(self.stream, bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        io::Write::flush(self.stream)
    }
}

/// root每代创建独立socket路径，按角色授予指定用户或root访问。
pub struct UnixEndpoint {
    listener: UnixListener,
    directory: PathBuf,
    name: String,
    uid: u32,
}
impl UnixEndpoint {
    /// root创建受保护目录；只允许指定UID访问socket，内核凭据另校验实际UID/PID。
    pub fn new_for_user(uid: u32, gid: u32) -> io::Result<Self> {
        if unsafe { geteuid() } != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "desktop broker requires root",
            ));
        }
        let directory = PathBuf::from(format!(
            "/run/yoodesk-desktop-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        use std::os::unix::fs::DirBuilderExt;
        let mode = if uid == 0 { 0o700 } else { 0o710 };
        fs::DirBuilder::new().mode(mode).create(&directory)?;
        let directory_name =
            std::ffi::CString::new(directory.as_os_str().as_bytes()).map_err(|_| invalid())?;
        if unsafe { chown(directory_name.as_ptr(), 0, gid) } != 0 {
            let error = io::Error::last_os_error();
            let _ = fs::remove_dir(&directory);
            return Err(error);
        }
        if let Err(error) = fs::set_permissions(&directory, fs::Permissions::from_mode(mode)) {
            let _ = fs::remove_dir(&directory);
            return Err(error);
        }
        let path = directory.join("worker.sock");
        let listener = match UnixListener::bind(&path) {
            Ok(v) => v,
            Err(e) => {
                let _ = fs::remove_dir(&directory);
                return Err(e);
            }
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let socket_name =
            std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid())?;
        if unsafe { chown(socket_name.as_ptr(), uid, gid) } != 0 {
            let error = io::Error::last_os_error();
            let _ = fs::remove_file(&path);
            let _ = fs::remove_dir(&directory);
            return Err(error);
        }
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            directory,
            name: path.to_string_lossy().into_owned(),
            uid,
        })
    }
    /// 仅用于子进程参数的公共socket名。
    pub fn name(&self) -> &str {
        &self.name
    }
    /// 有限等待并校验指定用户的内核peer PID/uid，随后执行一次性握手。
    pub fn accept(self, pid: u32, uid: u32, timeout: Duration) -> io::Result<Arc<dyn Transport>> {
        if uid != self.uid {
            return Err(invalid());
        }
        let deadline = Instant::now() + timeout;
        let mut stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if verify(&stream, pid, uid).is_ok() {
                        break stream;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "desktop worker connect deadline",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let nonce: [u8; 32] = rand::random();
        let mut handshake = DeadlineIo::new(&mut stream, Duration::from_secs(10));
        write_frame(&mut handshake, &json!({"v":1,"op":"hello"}), &nonce)?;
        let (hello, echo) = read_frame(&mut handshake)?;
        if hello != json!({"v":1,"op":"ready"}) || echo != nonce {
            return Err(invalid());
        }
        Ok(Arc::new(UnixTransport {
            cancel: stream.try_clone()?,
            io: Mutex::new(Some(stream)),
            next: AtomicU64::new(1),
            failed: AtomicBool::new(false),
        }))
    }
}
impl Drop for UnixEndpoint {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.name);
        let _ = fs::remove_dir(&self.directory);
    }
}
struct UnixTransport {
    io: Mutex<Option<UnixStream>>,
    cancel: UnixStream,
    next: AtomicU64,
    failed: AtomicBool,
}
impl Transport for UnixTransport {
    fn healthy(&self) -> bool {
        !self.failed.load(Ordering::SeqCst)
    }
    fn close(&self) {
        self.failed.store(true, Ordering::SeqCst);
        let _ = self.cancel.shutdown(Shutdown::Both);
    }
    fn request(&self, op: &str, args: Value) -> Result<Reply, ApiError> {
        if !self.healthy() {
            return Err(engine_error());
        }
        let mut guard = self.io.lock().unwrap_or_else(|e| e.into_inner());
        if !self.healthy() {
            return Err(engine_error());
        }
        let stream = guard.as_mut().ok_or_else(engine_error)?;
        let mut io = DeadlineIo::new(stream, Duration::from_secs(10));
        let result = (|| {
            let id = self.next.fetch_add(1, Ordering::SeqCst);
            let bytes = serde_json::to_vec(&args).map_err(|_| invalid())?;
            write_frame(&mut io, &json!({"v":1,"id":id,"op":op}), &bytes)?;
            let (metadata, bytes) = read_frame(&mut io)?;
            if metadata["v"] != 1 || metadata["id"] != id {
                return Err(invalid());
            }
            if metadata["error"] == true {
                let error: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                return Ok(Err(ApiError::new(
                    error["status"].as_u64().unwrap_or(503) as u16,
                    codes::DESK_ENGINE,
                    error["message"]
                        .as_str()
                        .unwrap_or("desktop operation failed"),
                )));
            }
            Ok(Ok(if metadata["json"] == true {
                Reply {
                    value: serde_json::from_slice(&bytes).map_err(|_| invalid())?,
                    bytes: Vec::new(),
                }
            } else {
                Reply {
                    value: metadata["value"].clone(),
                    bytes,
                }
            }))
        })();
        match result {
            Ok(result) => result,
            Err(_) => {
                guard.take();
                self.close();
                Err(engine_error())
            }
        }
    }
}

/// 用户桌面worker入口：普通用户运行，校验root服务父；不创建HTTP listener或读服务凭据。
pub fn run_worker(path: &str, parent_pid: u32) -> io::Result<()> {
    let uid = unsafe { geteuid() };
    let gid = unsafe { getegid() };
    if uid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "desktop worker requires the selected graphical user",
        ));
    }
    let socket = PathBuf::from(path);
    if !socket.starts_with("/run") {
        return Err(invalid());
    }
    let directory = socket.parent().ok_or_else(invalid)?;
    let parent = fs::symlink_metadata(directory)?;
    let metadata = fs::symlink_metadata(&socket)?;
    if parent.uid() != 0
        || parent.mode() & 0o777 != 0o710
        || parent.gid() != gid
        || !parent.is_dir()
        || metadata.uid() != uid
        || metadata.gid() != gid
        || metadata.mode() & 0o777 != 0o600
        || !metadata.file_type().is_socket()
    {
        return Err(invalid());
    }
    let mut stream = UnixStream::connect(&socket)?;
    verify(&stream, parent_pid, 0)?;
    let mut handshake = DeadlineIo::new(&mut stream, Duration::from_secs(10));
    let (hello, nonce) = read_frame(&mut handshake)?;
    if hello != json!({"v":1,"op":"hello"}) || nonce.len() != 32 {
        return Err(invalid());
    }
    write_frame(&mut handshake, &json!({"v":1,"op":"ready"}), &nonce)?;
    // worker可在无请求时长期空闲；握手期限不能变成空闲会话期限。
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    // 父退出会关闭socket，EOF沿共享serve路径释放输入、订阅和隐私状态。
    crate::desk::engine::worker::serve(&mut stream)
}

/// root特权helper仅探测/运行DRM并等待父控制连接关闭，不运行输入/桌面/音频/HTTP。
pub fn run_privileged_worker(path: &str, parent_pid: u32) -> io::Result<()> {
    if unsafe { geteuid() } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "privileged worker requires root",
        ));
    }
    let uid = std::env::var("YOODESK_ENGINE_HELPER_UID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|uid| *uid != 0)
        .ok_or_else(invalid)?;
    let gid = std::env::var("YOODESK_ENGINE_HELPER_GID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(invalid)?;
    let socket = PathBuf::from(path);
    if !socket.starts_with("/run") {
        return Err(invalid());
    }
    let directory = fs::symlink_metadata(socket.parent().ok_or_else(invalid)?)?;
    let metadata = fs::symlink_metadata(&socket)?;
    if !directory.is_dir()
        || directory.uid() != 0
        || directory.mode() & 0o777 != 0o700
        || !metadata.file_type().is_socket()
        || metadata.uid() != 0
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(invalid());
    }
    let mut stream = UnixStream::connect(socket)?;
    verify(&stream, parent_pid, 0)?;
    let mut handshake = DeadlineIo::new(&mut stream, Duration::from_secs(10));
    let (hello, nonce) = read_frame(&mut handshake)?;
    if hello != json!({"v":1,"op":"hello"}) || nonce.len() != 32 {
        return Err(invalid());
    }
    crate::desk::engine::initialize_privileged(uid, gid)
        .map_err(|error| io::Error::other(error.message))?;
    write_frame(&mut handshake, &json!({"v":1,"op":"ready"}), &nonce)?;
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(None)?;
    let mut byte = [0u8; 1];
    match std::io::Read::read(&mut stream, &mut byte) {
        Ok(0) => Ok(()),
        Ok(_) => Err(invalid()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "run explicitly as the ordinary graphical user on the Linux VM"]
    fn ordinary_user_cannot_start_privileged_role() {
        assert_ne!(unsafe { geteuid() }, 0);
        assert_eq!(
            run_privileged_worker("/run/invalid", std::process::id())
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            UnixEndpoint::new_for_user(65534, 65534)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
    #[test]
    fn protocol_failure_marks_transport_unhealthy_while_peer_is_alive() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let transport = UnixTransport {
            cancel: stream.try_clone().unwrap(),
            io: Mutex::new(Some(stream)),
            next: AtomicU64::new(1),
            failed: AtomicBool::new(false),
        };
        let (release, waiting) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
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
        assert_eq!(
            transport.request("test", json!({})).err().unwrap().status,
            400
        );
        assert!(
            transport.healthy(),
            "business errors leave the worker usable"
        );
        assert!(transport.request("test", json!({})).is_err());
        assert!(!transport.healthy(), "wrong response generation is fatal");
        assert!(
            !worker.is_finished(),
            "peer remains alive while transport is poisoned"
        );
        release.send(()).unwrap();
        worker.join().unwrap();
    }
    #[test]
    fn total_deadline_rejects_trickled_payload_and_stalled_write() {
        use std::io::Write;
        let (mut reader, mut sender) = UnixStream::pair().unwrap();
        let send = std::thread::spawn(move || {
            // 每块间隔30ms，小于100ms超时；整帧却需要600ms。
            sender.write_all(&2u32.to_le_bytes()).unwrap();
            sender.write_all(&20u32.to_le_bytes()).unwrap();
            sender.write_all(b"{}").unwrap();
            for _ in 0..20 {
                if sender.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        let start = Instant::now();
        assert!(read_frame(&mut DeadlineIo::new(
            &mut reader,
            Duration::from_millis(100)
        ))
        .is_err());
        assert!(start.elapsed() < Duration::from_millis(400));
        drop(reader);
        send.join().unwrap();
        let (mut writer, _unread) = UnixStream::pair().unwrap();
        let start = Instant::now();
        assert!(write_frame(
            &mut DeadlineIo::new(&mut writer, Duration::from_millis(100)),
            &json!({"v":1}),
            &vec![0; 24 << 20]
        )
        .is_err());
        assert!(start.elapsed() < Duration::from_millis(400));
    }
    #[test]
    #[ignore = "requires a dedicated root process on the Linux VM"]
    fn root_socket_binds_peer_pid_and_cancels_pending_io() {
        // 临时放宽本测试socket权限，单独证明peercred仍拒绝uid非0；生产始终0700/0600。
        use std::os::unix::process::CommandExt;
        let endpoint = UnixEndpoint::new_for_user(0, 0).unwrap();
        fs::set_permissions(&endpoint.directory, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(endpoint.name(), fs::Permissions::from_mode(0o666)).unwrap();
        let mut unprivileged=std::process::Command::new("/usr/bin/python3")
            .args(["-c","import socket,sys,time; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); time.sleep(0.1)",endpoint.name()])
            .uid(65534).gid(65534).spawn().unwrap();
        assert_eq!(
            endpoint
                .accept(unprivileged.id(), 0, Duration::from_secs(2))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(unprivileged.wait().unwrap().success());
        let endpoint = UnixEndpoint::new_for_user(0, 0).unwrap();
        let path = endpoint.name().to_owned();
        let wrong = std::thread::spawn(move || UnixStream::connect(path).unwrap());
        assert_eq!(
            endpoint
                .accept(std::process::id() + 1, 0, Duration::from_millis(150))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::TimedOut
        );
        drop(wrong.join().unwrap());
        let endpoint = UnixEndpoint::new_for_user(0, 0).unwrap();
        let path = endpoint.name().to_owned();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut stream = UnixStream::connect(path).unwrap();
            let (hello, nonce) = read_frame(&mut stream).unwrap();
            assert_eq!(hello["v"], 1);
            assert_eq!(nonce.len(), 32);
            write_frame(&mut stream, &json!({"v":1,"op":"ready"}), &nonce).unwrap();
            let _ = read_frame(&mut stream).unwrap();
            entered_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });
        let transport = endpoint
            .accept(std::process::id(), 0, Duration::from_secs(2))
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
    #[test]
    #[ignore = "requires a dedicated root process on the Linux VM"]
    fn selected_user_socket_accepts_only_expected_uid_and_root_parent() {
        use std::os::unix::process::CommandExt;
        assert_eq!(
            run_worker("/run/invalid", std::process::id())
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let endpoint = UnixEndpoint::new_for_user(65534, 65534).unwrap();
        let script = r#"import socket,sys,struct,json
s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); pid,uid,gid=struct.unpack('3i',s.getsockopt(socket.SOL_SOCKET,socket.SO_PEERCRED,12)); assert uid==0 and pid==int(sys.argv[2])
def read(n):
 b=b''
 while len(b)<n:
  chunk=s.recv(n-len(b)); assert chunk; b+=chunk
 return b
def frame():
 a,b=struct.unpack('<II',read(8)); return json.loads(read(a)),read(b)
def send(meta,payload):
 h=json.dumps(meta,separators=(',',':')).encode(); s.sendall(struct.pack('<II',len(h),len(payload))+h+payload)
m,nonce=frame(); assert m=={'v':1,'op':'hello'} and len(nonce)==32; send({'v':1,'op':'ready'},nonce)
m,args=frame(); send({'v':1,'id':m['id'],'json':True},b'null')
"#;
        let mut child = std::process::Command::new("/usr/bin/python3")
            .args([
                "-c",
                script,
                endpoint.name(),
                &std::process::id().to_string(),
            ])
            .uid(65534)
            .gid(65534)
            .spawn()
            .unwrap();
        let transport = endpoint
            .accept(child.id(), 65534, Duration::from_secs(2))
            .unwrap();
        assert!(transport.request("displays", json!({})).is_ok());
        assert!(child.wait().unwrap().success());
    }
}
