//! 端侧扩展。只复用 RustDesk 业务，不启动其网络或官方 UI。
use base::{
    fs::{self, DataSource, JobType, TransferJob},
    message_proto::{message, misc},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use protobuf::Message as _;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    ffi::c_int,
    io::{self, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

const CHUNK: usize = 128 * 1024;
const MAX_FILE: u64 = 1 << 30;
struct Aux {
    audio: VecDeque<Vec<u8>>,
    format: Option<Vec<u8>>,
    audio_service: librustdesk::PublicGenericService,
}
static AUX: Mutex<Option<HashMap<i32, Arc<Mutex<Aux>>>>> = Mutex::new(None);
static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);
static AUDIO_SERVICE: OnceLock<librustdesk::PublicGenericService> = OnceLock::new();

pub fn subscribe(conn: i32) {
    let audio_service = AUDIO_SERVICE
        .get_or_init(librustdesk::audio_service::new)
        .clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let subscriber = librustdesk::ConnInner::new(conn, Some(tx), None);
    let aux = Arc::new(Mutex::new(Aux {
        audio: VecDeque::new(),
        format: None,
        audio_service: audio_service.clone(),
    }));
    AUX.lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(conn, aux.clone());
    librustdesk::PublicService::on_subscribe(&audio_service, subscriber);
    std::thread::spawn(move || {
        while let Some((_, msg)) = rx.blocking_recv() {
            let is_format = matches!(&msg.union, Some(message::Union::Misc(m)) if matches!(m.union, Some(misc::Union::AudioFormat(_))));
            if !is_format && !matches!(&msg.union, Some(message::Union::AudioFrame(_))) {
                continue;
            }
            if let Ok(bytes) = msg.write_to_bytes() {
                let mut guarded = aux.lock().unwrap();
                if is_format {
                    guarded.format = Some(bytes.clone());
                    guarded.audio.clear();
                }
                if guarded.audio.len() >= 32 {
                    guarded.audio.pop_front();
                }
                guarded.audio.push_back(bytes);
            }
        }
    });
}

pub fn unsubscribe(conn: i32) {
    let aux = AUX.lock().unwrap().as_mut().and_then(|m| m.remove(&conn));
    if let Some(aux) = aux {
        let guard = aux.lock().unwrap();
        librustdesk::PublicService::on_unsubscribe(&guard.audio_service, conn);
    }
    let mut jobs = jobs().lock().unwrap();
    jobs.retain(|_, job| job.conn != conn);
    #[cfg(windows)]
    if let Err(err) = crate::privacy::close_for(conn) {
        log::error!("privacy cleanup: {err}");
    }
}

fn audio(conn: i32, replay: bool) -> Option<Vec<u8>> {
    let aux = AUX.lock().unwrap().as_ref()?.get(&conn)?.clone();
    let mut guard = aux.lock().unwrap();
    if replay {
        guard.format.clone()
    } else {
        guard.audio.pop_front()
    }
}

#[no_mangle]
pub extern "C" fn yd_engine_poll_audio(
    conn: c_int,
    replay: c_int,
    data: *mut u8,
    cap: usize,
) -> c_int {
    if data.is_null() || cap < 65536 {
        return -1;
    }
    let Some(bytes) = audio(conn, replay != 0) else {
        return 0;
    };
    if bytes.len() > cap {
        return -1;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
    }
    bytes.len() as c_int
}

// 同步写入使用上游同一 arboard fork，错误必须返回给浏览器。
#[path = "clipboard_retry.rs"]
mod clipboard_retry;

fn clipboard(v: &Value) -> Result<Value, Fault> {
    #[cfg(target_os = "linux")]
    if crate::wayland_portal::enabled() {
        return crate::wayland_portal::clipboard(v)
            .map_err(|(status, message)| fault(status, &message));
    }
    let mut guard = CLIPBOARD.lock().unwrap();
    if guard.is_none() {
        *guard = Some(arboard::Clipboard::new().map_err(engine_error)?);
    }
    let board = guard
        .as_mut()
        .ok_or_else(|| fault(503, "clipboard unavailable"))?;
    if v["action"] == "clipboard.write" {
        let content = &v["content"];
        let data = match content["kind"].as_str() {
            Some("text") => arboard::ClipboardData::Text(string(content, "text")?.to_owned()),
            Some("image") => {
                let width = number(content, "width")? as usize;
                let height = number(content, "height")? as usize;
                let bytes = STANDARD.decode(string(content, "data")?).map_err(invalid)?;
                if width == 0
                    || height == 0
                    || width.checked_mul(height).and_then(|n| n.checked_mul(4)) != Some(bytes.len())
                    || bytes.len() > 16 << 20
                {
                    return Err(fault(400, "invalid clipboard image"));
                }
                arboard::ClipboardData::Image(arboard::ImageData::Rgba(arboard::ImageRgba {
                    width,
                    height,
                    bytes: bytes.into(),
                }))
            }
            _ => return Err(fault(400, "invalid clipboard kind")),
        };
        board.set_formats(&[data]).map_err(engine_error)?;
        return Ok(json!({"applied":true}));
    }
    // 与 RustDesk clipboard_service 使用同一 arboard fork；读写去重留在会话层，
    // 不添加上游跨原生客户端的 owner marker，以免隐藏控制端刚写入的内容。
    let data = clipboard_retry::read_with_retry(
        || board.get_formats(&[
            arboard::ClipboardFormat::Text,
            arboard::ClipboardFormat::ImageRgba,
        ]),
        |error| matches!(error, arboard::Error::ClipboardOccupied),
    )
        .map_err(engine_error)?;
    for content in data {
        match content {
            arboard::ClipboardData::Text(text) => return Ok(json!({"kind":"text","text":text})),
            arboard::ClipboardData::Image(arboard::ImageData::Rgba(image)) => {
                if image.bytes.len() > 16 << 20 {
                    return Err(fault(413, "clipboard image too large"));
                }
                return Ok(
                    json!({"kind":"image","mime":"image/rgba","width":image.width,"height":image.height,"data":STANDARD.encode(&image.bytes)}),
                );
            }
            _ => {}
        }
    }
    Ok(Value::Null)
}

#[cfg(target_os = "linux")]
struct StagingDirectory {
    _lease: crate::file_journal::Lease,
}

#[cfg(windows)]
struct StagingDirectory {
    path: PathBuf,
    handle: std::fs::File,
}

#[cfg(not(any(windows, target_os = "linux")))]
struct StagingDirectory;

impl StagingDirectory {
    fn create(_conn: i32, _id: i32, path: &std::path::Path) -> Result<Self, Fault> {
        #[cfg(target_os = "linux")]
        {
            Ok(Self {
                _lease: crate::file_journal::create(_conn, _id, path).map_err(engine_error)?,
            })
        }
        #[cfg(windows)]
        {
            std::fs::create_dir(path).map_err(engine_error)?;
            let handle = Self::open(path).map_err(engine_error)?;
            Self::identity(&handle).map_err(engine_error)?;
            Ok(Self {
                path: path.to_path_buf(),
                handle,
            })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = path;
            Err(fault(501, "safe upload staging unavailable"))
        }
    }
}

#[cfg(windows)]
impl StagingDirectory {
    fn open(path: &std::path::Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        use winapi::um::winbase::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT};
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    fn identity(file: &std::fs::File) -> std::io::Result<(u32, u32, u32)> {
        use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
        use winapi::um::fileapi::GetFileInformationByHandle;
        let mut info = MaybeUninit::uninit();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), info.as_mut_ptr()) }
            == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let info = unsafe { info.assume_init() };
        if info.dwFileAttributes & 0x410 != 0x10 {
            return Err(std::io::Error::other(
                "upload staging is not a plain directory",
            ));
        }
        Ok((
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ))
    }

    fn cleanup(&self) -> Result<(), Fault> {
        let expected = Self::identity(&self.handle).map_err(engine_error)?;
        let current = match Self::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(engine_error(err)),
        };
        if Self::identity(&current).map_err(engine_error)? != expected {
            return Err(fault(409, "upload staging identity changed"));
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(engine_error)?
            .as_nanos();
        let tomb = self.path.with_file_name(format!(
            "{TRANSFER_DIRECTORY_PREFIX}cleanup-{}-{nonce}",
            std::process::id()
        ));
        rename_new(&self.path, &tomb)?;
        let moved = Self::open(&tomb).and_then(|file| Self::identity(&file));
        if !matches!(moved, Ok(identity) if identity == expected) {
            let _ = rename_new(&tomb, &self.path);
            return Err(fault(409, "upload staging replaced during cleanup"));
        }
        std::fs::remove_dir_all(tomb).map_err(engine_error)
    }
}

#[cfg(windows)]
impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if let Err(err) = self.cleanup() {
            log::warn!("transfer cleanup: {}", err.message);
        }
    }
}

// Keep the original file open from the first byte through atomic publication.
struct UploadPayload(std::fs::File);

impl UploadPayload {
    fn create(path: &std::path::Path) -> io::Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use winapi::um::winnt::{DELETE, FILE_SHARE_DELETE, FILE_SHARE_READ, GENERIC_WRITE};
            options
                .access_mode(GENERIC_WRITE | DELETE)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
        }
        options.open(path).map(Self)
    }

    #[cfg(target_os = "linux")]
    fn publish(&self, destination: &std::path::Path) -> io::Result<()> {
        use hbb_common::libc::{linkat, AT_FDCWD, AT_SYMLINK_FOLLOW};
        use std::{
            ffi::CString,
            os::{fd::AsRawFd, unix::ffi::OsStrExt},
        };
        let source = CString::new(format!("/proc/self/fd/{}", self.0.as_raw_fd())).unwrap();
        let destination =
            CString::new(destination.as_os_str().as_bytes()).map_err(io::Error::other)?;
        if unsafe {
            linkat(
                AT_FDCWD,
                source.as_ptr(),
                AT_FDCWD,
                destination.as_ptr(),
                AT_SYMLINK_FOLLOW,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(windows)]
    fn publish(&self, destination: &std::path::Path) -> io::Result<()> {
        use std::{
            mem::{offset_of, size_of},
            os::windows::{ffi::OsStrExt, io::AsRawHandle},
        };
        use winapi::um::{
            fileapi::{SetFileInformationByHandle, FILE_RENAME_INFO},
            minwinbase::FileRenameInfo,
        };
        let name: Vec<u16> = destination.as_os_str().encode_wide().collect();
        let length = name.len() * size_of::<u16>();
        let bytes = (offset_of!(FILE_RENAME_INFO, FileName) + length + size_of::<u16>())
            .max(size_of::<FILE_RENAME_INFO>());
        // Aligned variable-length FILE_RENAME_INFO, with replacement disabled.
        let mut buffer = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        unsafe {
            (*info).FileNameLength = length as u32;
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
                name.len(),
            );
            if SetFileInformationByHandle(
                self.0.as_raw_handle().cast(),
                FileRenameInfo,
                info.cast(),
                bytes as u32,
            ) != 0
            {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    fn publish(&self, _: &std::path::Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic upload publication unavailable",
        ))
    }
}

struct Job {
    staging: Option<StagingDirectory>,
    upload: Option<UploadPayload>,
    conn: i32,
    transfer: Option<TransferJob>,
    size: u64,
    offset: u64,
    destination: Option<PathBuf>,
    temporary: Option<PathBuf>,
    source: Option<PathBuf>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.transfer.take();
        self.upload.take();
        self.staging.take();
    }
}
static JOBS: OnceLock<Mutex<HashMap<i32, Job>>> = OnceLock::new();
fn jobs() -> &'static Mutex<HashMap<i32, Job>> {
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
fn runtime() -> Result<&'static tokio::runtime::Runtime, Fault> {
    if RUNTIME.get().is_none() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(engine_error)?;
        let _ = RUNTIME.set(rt);
    }
    RUNTIME
        .get()
        .ok_or_else(|| fault(503, "endpoint runtime unavailable"))
}

fn default_root() -> Result<PathBuf, Fault> {
    let p = match std::env::var_os("YOODESK_DESK_FILE_ROOT") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .ok_or_else(|| fault(503, "file root unavailable"))?,
        )
        .join("Downloads")
        .join(env!("PRODUCT_ID")),
    };
    std::fs::create_dir_all(&p).map_err(engine_error)?;
    p.canonicalize().map_err(engine_error)
}
// 根ID只能选服务端枚举的系统根，不能把请求字符串直接变成本机绝对路径。
fn system_roots() -> Result<Vec<(String, PathBuf)>, Fault> {
    #[cfg(windows)]
    {
        let mask = unsafe { winapi::um::fileapi::GetLogicalDrives() };
        if mask == 0 {
            return Err(engine_error(std::io::Error::last_os_error()));
        }
        Ok((0..26)
            .filter(|index| mask & (1 << index) != 0)
            .map(|index| {
                let letter = (b'A' + index) as char;
                (
                    format!("drive-{letter}"),
                    PathBuf::from(format!("{letter}:\\")),
                )
            })
            .collect())
    }
    #[cfg(not(windows))]
    {
        Ok(vec![("system".to_owned(), PathBuf::from("/"))])
    }
}
fn display_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix("\\\\?\\").unwrap_or(&text).to_owned()
}

fn selected_root(v: &Value) -> Result<PathBuf, Fault> {
    match v.get("root_id") {
        None => default_root(),
        Some(Value::String(id)) if id == "default" => default_root(),
        Some(Value::String(id)) => system_roots()?
            .into_iter()
            .find(|(key, _)| key == id)
            .ok_or_else(|| fault(400, "unknown file root"))?
            .1
            .canonicalize()
            .map_err(engine_error),
        _ => Err(fault(400, "invalid file root")),
    }
}
const TRANSFER_DIRECTORY_PREFIX: &str = ".yoodesk-transfer-";

// Reserve every ASCII case variant so case-insensitive filesystems cannot alias staging data.
fn is_transfer_directory(name: &str) -> bool {
    name.as_bytes()
        .get(..TRANSFER_DIRECTORY_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(TRANSFER_DIRECTORY_PREFIX.as_bytes()))
}

fn path(v: &Value, allow_root: bool) -> Result<(PathBuf, PathBuf), Fault> {
    let root = selected_root(v)?;
    let relative = string(v, "path")?;
    if relative.split(['/', '\\']).any(is_transfer_directory) {
        return Err(fault(400, "internal transfer path forbidden"));
    }
    for name in relative.split(['/', '\\']) {
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        if name.chars().any(|c| c < ' ' || ":*?\"<>|".contains(c))
            || name.ends_with(['.', ' '])
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(fault(400, "invalid Windows file name"));
        }
    }
    if relative.is_empty() {
        return if allow_root {
            Ok((root.clone(), root))
        } else {
            Err(fault(400, "file path required"))
        };
    }
    let target = fs::join_validated_path(&root, relative).map_err(invalid)?;
    let target = validate_path_components(&root, &target)?;
    Ok((root, target))
}

fn validate_path_components(
    root: &std::path::Path,
    target: &std::path::Path,
) -> Result<PathBuf, Fault> {
    let relative = target.strip_prefix(root).map_err(invalid)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        let meta = match std::fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(file_error(err)),
        };
        if meta.file_type().is_symlink() {
            return Err(fault(400, "symlink forbidden"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(fault(400, "reparse point forbidden"));
            }
            // Expand NTFS short aliases before checking the reserved namespace.
            let resolved = current.canonicalize().map_err(file_error)?;
            let components = resolved
                .strip_prefix(root)
                .map_err(|_| fault(400, "path escapes file root"))?;
            if components
                .iter()
                .any(|name| is_transfer_directory(&name.to_string_lossy()))
            {
                return Err(fault(400, "internal transfer path forbidden"));
            }
            current = resolved;
        }
    }
    Ok(current)
}

fn destination_reserved(
    jobs: &HashMap<i32, Job>,
    candidate: &std::path::Path,
    except: Option<i32>,
) -> bool {
    jobs.iter().any(|(id, job)| {
        Some(*id) != except
            && job.destination.as_ref().is_some_and(|destination| {
                destination == candidate
                    || (cfg!(windows)
                        && destination
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&candidate.to_string_lossy()))
            })
    })
}

// Windows式编号保留扩展名；待提交任务也占用名字，发布仍以不覆盖的hard_link为准。
fn available_destination(
    target: &std::path::Path,
    reserved: impl Fn(&std::path::Path) -> bool,
) -> Result<PathBuf, Fault> {
    if !target.try_exists().map_err(engine_error)? && !reserved(target) {
        return Ok(target.to_owned());
    }
    let parent = target
        .parent()
        .ok_or_else(|| fault(400, "parent directory required"))?;
    let stem = target
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| fault(400, "file name required"))?;
    let suffix = target
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{extension}"))
        .unwrap_or_default();
    let numbered = stem
        .strip_suffix(')')
        .and_then(|stem| stem.rsplit_once(" ("))
        .and_then(|(base, number)| {
            number
                .parse::<u64>()
                .ok()
                .filter(|number| *number > 0)
                .map(|number| (base, number.saturating_add(1)))
        });
    let (base, start) = numbered.unwrap_or((stem, 2));
    for number in start..start.saturating_add(10_000) {
        let candidate = parent.join(format!("{base} ({number}){suffix}"));
        if !candidate.try_exists().map_err(engine_error)? && !reserved(&candidate) {
            return Ok(candidate);
        }
    }
    Err(fault(409, "cannot allocate a free file name"))
}

fn file_error(error: std::io::Error) -> Fault {
    use std::io::ErrorKind;
    fault(
        match error.kind() {
            ErrorKind::AlreadyExists => 409,
            ErrorKind::NotFound => 404,
            ErrorKind::PermissionDenied => 403,
            _ => 503,
        },
        &error.to_string(),
    )
}

// 系统原子“不替换”重命名，不能用 exists + rename（Linux rename 会覆盖竞态中出现的文件）。
fn rename_new(from: &std::path::Path, to: &std::path::Path) -> Result<(), Fault> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { winapi::um::winbase::MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
            return Err(file_error(std::io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        extern "C" {
            fn renameat2(oldfd: i32, old: *const i8, newfd: i32, new: *const i8, flags: u32)
                -> i32;
        }
        let from = CString::new(from.as_os_str().as_bytes()).map_err(invalid)?;
        let to = CString::new(to.as_os_str().as_bytes()).map_err(invalid)?;
        if unsafe { renameat2(-100, from.as_ptr(), -100, to.as_ptr(), 1) } != 0 {
            return Err(file_error(std::io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (from, to);
        Err(fault(501, "atomic rename unavailable"))
    }
}

// Resolve each ancestor without following links. Keep the parent pinned
// until the mutation completes, including recursive removal.
struct MutationParent {
    directory: PathBuf,
    _handles: Vec<std::fs::File>,
}

impl MutationParent {
    fn open(path: &std::path::Path) -> Result<Self, Fault> {
        use std::path::Component;
        let mut directory = PathBuf::new();
        let mut handles = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(_) => directory.push(component),
                Component::RootDir => {
                    directory.push(component);
                    #[cfg(target_os = "linux")]
                    {
                        let handle = Self::open_directory(&directory)?;
                        use std::os::fd::AsRawFd;
                        directory = PathBuf::from(format!("/proc/self/fd/{}", handle.as_raw_fd()));
                        handles.push(handle);
                    }
                }
                Component::Normal(name) => {
                    let child = directory.join(name);
                    let handle = Self::open_directory(&child)?;
                    #[cfg(target_os = "linux")]
                    {
                        use std::os::fd::AsRawFd;
                        directory = PathBuf::from(format!("/proc/self/fd/{}", handle.as_raw_fd()));
                        handles.clear();
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        directory = child;
                    }
                    handles.push(handle);
                }
                _ => return Err(fault(400, "invalid parent path")),
            }
        }
        if !path.is_absolute() {
            return Err(fault(400, "absolute parent path required"));
        }
        Ok(Self { directory, _handles: handles })
    }

    fn open_directory(path: &std::path::Path) -> Result<std::fs::File, Fault> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // O_PATH | O_DIRECTORY | O_NOFOLLOW: no read permission is
            // required on ancestors, and the final component cannot be a link.
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(0o10000000 | 0o200000 | 0o400000)
                .open(path)
                .map_err(file_error)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            let handle = std::fs::OpenOptions::new()
                .access_mode(0x80) // FILE_READ_ATTRIBUTES
                .share_mode(1) // FILE_SHARE_READ; deny replacement and reparse changes
                .custom_flags(0x02200000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
                .open(path)
                .map_err(file_error)?;
            let metadata = handle.metadata().map_err(file_error)?;
            if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
                return Err(fault(400, "invalid parent directory"));
            }
            Ok(handle)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = path;
            Err(fault(501, "safe file mutation unavailable"))
        }
    }

    fn entry(&self, path: &std::path::Path) -> Result<PathBuf, Fault> {
        Ok(self.directory.join(
            path.file_name().ok_or_else(|| fault(400, "file name required"))?,
        ))
    }
}

fn manage_file(v: &Value, guard: &HashMap<i32, Job>) -> Result<Value, Fault> {
    let (root, target) = path(v, false)?;
    let parent_path = target
        .parent()
        .ok_or_else(|| fault(400, "parent directory required"))?;
    let parent = MutationParent::open(parent_path)?;
    #[cfg(windows)]
    validate_path_components(&root, &target)?;
    let _ = root;
    let entry = parent.entry(&target)?;
    let action = string(v, "action")?;
    // 活跃传输占用的文件、父目录和临时目录不能在传输途中被改名或删除。
    let busy = |candidate: &std::path::Path| {
        guard.values().any(|job| {
            job.destination
                .iter()
                .chain(job.temporary.iter())
                .chain(job.source.iter())
                .any(|p| {
                    if cfg!(windows) {
                        PathBuf::from(p.to_string_lossy().to_lowercase())
                            .starts_with(PathBuf::from(candidate.to_string_lossy().to_lowercase()))
                    } else {
                        p.starts_with(candidate)
                    }
                })
        })
    };
    if busy(&target) {
        return Err(fault(409, "path has active transfer"));
    }
    if action == "files.rename" {
        let name = string(v, "name")?;
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err(fault(400, "single file name required"));
        }
        let relative_parent = std::path::Path::new(string(v, "path")?)
            .parent()
            .unwrap_or(std::path::Path::new(""));
        let mut destination = v.clone();
        destination["path"] = json!(relative_parent.join(name).to_string_lossy());
        let (_, destination) = path(&destination, false)?;
        if busy(&destination) {
            return Err(fault(409, "path has active transfer"));
        }
        if destination.parent() != Some(parent_path) {
            return Err(fault(409, "parent directory changed; refresh directory"));
        }
        rename_new(&entry, &parent.entry(&destination)?)?;
    } else {
        let directory = match string(v, "kind")? {
            "file" => false,
            "directory" => true,
            _ => return Err(fault(400, "invalid entry kind")),
        };
        if action == "files.create" {
            if directory {
                std::fs::create_dir(&entry).map_err(file_error)?;
            } else {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&entry)
                    .map_err(file_error)?;
            }
        } else {
            let metadata = std::fs::symlink_metadata(&entry).map_err(file_error)?;
            if metadata.is_dir() != directory || !(metadata.is_dir() || metadata.is_file()) {
                return Err(fault(409, "entry type changed; refresh directory"));
            }
            if directory {
                std::fs::remove_dir_all(&entry).map_err(file_error)?;
            } else {
                std::fs::remove_file(&entry).map_err(file_error)?;
            }
        }
    }
    Ok(json!({"applied":true}))
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn mutation_stays_in_pinned_parent_after_symlink_swap() {
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!(
        "desk-mutation-parent-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    let original = root.join("parent");
    let moved = root.join("moved");
    let outside = root.join("outside");
    std::fs::create_dir(&original).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(original.join("entry"), b"original").unwrap();
    std::fs::write(outside.join("entry"), b"outside").unwrap();

    let parent = MutationParent::open(&original).unwrap();
    std::fs::rename(&original, &moved).unwrap();
    symlink(&outside, &original).unwrap();
    assert!(MutationParent::open(&original).is_err());

    let entry = parent.entry(&original.join("entry")).unwrap();
    let renamed = parent.entry(&original.join("renamed")).unwrap();
    rename_new(&entry, &renamed).unwrap();
    assert_eq!(std::fs::read(outside.join("entry")).unwrap(), b"outside");
    assert!(!outside.join("renamed").exists());
    assert_eq!(std::fs::read(moved.join("renamed")).unwrap(), b"original");
    std::fs::remove_dir_all(root).unwrap();
}

const MAX_DIRECTORY_ENTRIES: usize = 2000;
const MAX_DIRECTORY_SCAN: usize = 4096;

fn list_directory(target: &std::path::Path) -> io::Result<(Vec<Value>, bool)> {
    let mut entries = Vec::new();
    for (index, entry) in std::fs::read_dir(target)?.enumerate() {
        if index == MAX_DIRECTORY_SCAN || entries.len() == MAX_DIRECTORY_ENTRIES {
            return Ok((entries, true));
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if is_transfer_directory(name) {
            continue;
        }
        #[cfg(not(windows))]
        if name.starts_with('.') {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x2 != 0 {
                continue;
            }
        }
        let directory = metadata.is_dir();
        entries.push(
            json!({"name":name,"type":if directory {"directory"} else {"file"},
            "size":if directory {0} else {metadata.len()}}),
        );
    }
    Ok((entries, false))
}

// Directory discovery does not touch the transfer table or initialize its runtime.
fn files_endpoint(conn: i32, v: &Value) -> Result<Value, Fault> {
    match string(v, "action")? {
        "files.roots" => {
            let roots = system_roots()?;
            Ok(
                json!({"platform": if cfg!(windows) {"windows"} else {"unix"},
                "roots": roots.iter().map(|(id, path)| json!({"id":id,"path":display_path(path)})).collect::<Vec<_>>()}),
            )
        }
        "files.list" => {
            let (root, target) = path(v, true)?;
            let (entries, truncated) = list_directory(&target).map_err(file_error)?;
            Ok(
                json!({"root":display_path(&root),"path":display_path(&target),
                "platform":if cfg!(windows) {"windows"} else {"unix"},"entries":entries,"truncated":truncated}),
            )
        }
        _ => files_with_context(conn, v, &mut jobs().lock().unwrap(), runtime()?),
    }
}

fn files_with_context(
    conn: i32,
    v: &Value,
    guard: &mut HashMap<i32, Job>,
    rt: &tokio::runtime::Runtime,
) -> Result<Value, Fault> {
    let action = string(v, "action")?;
    if matches!(action, "files.create" | "files.rename" | "files.delete") {
        return manage_file(v, &guard);
    }
    if action == "files.upload" || action == "files.download" {
        if guard.values().filter(|j| j.conn == conn).count() >= 4 {
            return Err(fault(429, "too many transfers"));
        }
        let (_, mut target) = path(v, false)?;
        let id = fs::get_next_job_id();
        let upload = action == "files.upload";
        let size = if upload {
            number(v, "size")?
        } else {
            let m = target.metadata().map_err(engine_error)?;
            if !m.is_file() {
                return Err(fault(400, "regular file required"));
            }
            m.len()
        };
        if size > MAX_FILE {
            return Err(fault(413, "file exceeds 1 GiB"));
        }
        let job = if upload {
            target = available_destination(&target, |candidate| {
                destination_reserved(&guard, candidate, None)
            })?;
            if !target.parent().map(|p| p.is_dir()).unwrap_or(false) {
                return Err(fault(404, "parent directory not found"));
            }
            // 临时文件与目标放在同一目录/文件系统；浏览系统根不要求能写磁盘根。
            let dir = target
                .parent()
                .ok_or_else(|| fault(400, "parent directory required"))?
                .join(format!("{TRANSFER_DIRECTORY_PREFIX}{conn}-{id}"));
            let staging = StagingDirectory::create(conn, id, &dir)?;
            let upload =
                UploadPayload::create(&dir.join("payload.download")).map_err(engine_error)?;
            Job {
                staging: Some(staging),
                upload: Some(upload),
                conn,
                transfer: None,
                size,
                offset: 0,
                destination: Some(target),
                temporary: Some(dir),
                source: None,
            }
        } else {
            let transfer = TransferJob::new_read(
                id,
                JobType::Generic,
                String::new(),
                DataSource::FilePath(target.clone()),
                0,
                false,
                true,
                false,
            )
            .map_err(engine_error)?;
            Job {
                staging: None,
                upload: None,
                conn,
                transfer: Some(transfer),
                size,
                offset: 0,
                destination: None,
                temporary: None,
                source: Some(target),
            }
        };
        let name = job.destination.as_ref().map(|path| fs::get_file_name(path));
        guard.insert(id, job);
        return Ok(json!({"transfer_id":id,"size":size,"name":name}));
    }
    let id = i32::try_from(number(v, "transfer_id")?).map_err(invalid)?;
    let job = guard
        .get_mut(&id)
        .filter(|j| j.conn == conn)
        .ok_or_else(|| fault(404, "transfer not found"))?;
    if action == "files.cancel" {
        guard.remove(&id);
        return Ok(json!({"cancelled":true}));
    }
    if action == "files.commit" {
        if job.destination.is_none() {
            return Err(fault(400, "upload required"));
        }
        if job.offset != job.size {
            return Err(fault(409, "upload incomplete"));
        }
        let destination = job
            .destination
            .clone()
            .ok_or_else(|| fault(400, "upload required"))?;
        let payload = job
            .upload
            .as_ref()
            .ok_or_else(|| fault(409, "upload finalized"))?;
        payload.0.sync_all().map_err(engine_error)?;
        if payload.0.metadata().map_err(engine_error)?.len() != job.size {
            return Err(fault(409, "upload size mismatch"));
        }
        let payload = job.upload.take().expect("upload validated above");
        // Publish the open file atomically without replacing an existing destination.
        let publish = (|| {
            let mut destination = destination;
            for attempt in 0..10_000 {
                destination = available_destination(&destination, |candidate| {
                    destination_reserved(&guard, candidate, Some(id))
                })?;
                match payload.publish(&destination) {
                    Ok(()) => break,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::AlreadyExists && attempt < 9999 =>
                    {
                        continue
                    }
                    Err(error) => return Err(engine_error(error)),
                }
            }
            Ok(fs::get_file_name(&destination))
        })();
        let name = match publish {
            Ok(name) => {
                drop(payload);
                name
            }
            Err(err) => {
                guard
                    .get_mut(&id)
                    .expect("upload remains registered")
                    .upload = Some(payload);
                return Err(err);
            }
        };
        guard.remove(&id);
        return Ok(json!({"committed":true,"name":name}));
    }
    if number(v, "offset")? != job.offset {
        return Err(fault(409, "transfer offset mismatch"));
    }
    if action == "files.write" {
        if job.destination.is_none() {
            return Err(fault(400, "upload required"));
        }
        let bytes = STANDARD.decode(string(v, "data")?).map_err(invalid)?;
        if bytes.is_empty() || bytes.len() > CHUNK || job.offset + bytes.len() as u64 > job.size {
            return Err(fault(400, "invalid upload block"));
        }
        let length = bytes.len() as u64;
        let payload = job
            .upload
            .as_mut()
            .ok_or_else(|| fault(409, "upload finalized"))?;
        payload
            .0
            .seek(SeekFrom::Start(job.offset))
            .map_err(engine_error)?;
        payload.0.write_all(&bytes).map_err(engine_error)?;
        job.offset += length;
        return Ok(json!({"offset":job.offset}));
    }
    if action == "files.read" {
        if job.destination.is_some() {
            return Err(fault(400, "download required"));
        }
        let transfer = job
            .transfer
            .as_mut()
            .ok_or_else(|| fault(409, "transfer finalized"))?;
        rt.block_on(transfer.init_data_stream_for_cm())
            .map_err(engine_error)?;
        let block = rt.block_on(transfer.read()).map_err(engine_error)?;
        let bytes = match block {
            Some(b) => {
                if b.compressed {
                    hbb_common::compress::decompress(&b.data)
                } else {
                    b.data.to_vec()
                }
            }
            None => Vec::new(),
        };
        job.offset += bytes.len() as u64;
        if job.offset > job.size || (bytes.is_empty() && job.offset != job.size) {
            return Err(fault(409, "file changed during download"));
        }
        let done = job.offset == job.size;
        let result = json!({"data":STANDARD.encode(&bytes),"offset":job.offset,"done":done});
        if done {
            guard.remove(&id);
        }
        return Ok(result);
    }
    Err(fault(400, "unknown file action"))
}

fn privacy_supported() -> bool {
    #[cfg(windows)]
    {
        crate::privacy::supported()
    }
    #[cfg(not(windows))]
    {
        false
    }
}
fn privacy(conn: i32, v: &Value) -> Result<Value, Fault> {
    #[cfg(not(windows))]
    let _ = conn;
    #[cfg(windows)]
    if v["action"] == "privacy.get" {
        return Ok(json!({"enabled":crate::privacy::enabled_for(conn)}));
    }
    if !privacy_supported() {
        return Err(fault(501, "privacy screen unavailable"));
    }
    let enabled = v["enabled"]
        .as_bool()
        .ok_or_else(|| fault(400, "enabled boolean required"))?;
    #[cfg(windows)]
    crate::privacy::set(conn, enabled)
        .map_err(|e| fault(if e.contains("occupied") { 409 } else { 503 }, &e))?;
    Ok(json!({"enabled":enabled}))
}

struct Fault {
    status: u16,
    message: String,
}
fn fault(status: u16, message: &str) -> Fault {
    Fault {
        status,
        message: message.to_owned(),
    }
}
fn invalid(e: impl std::fmt::Display) -> Fault {
    fault(400, &e.to_string())
}
fn engine_error(e: impl std::fmt::Display) -> Fault {
    fault(503, &e.to_string())
}
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str, Fault> {
    v[k].as_str()
        .ok_or_else(|| fault(400, &format!("{k} string required")))
}
fn number(v: &Value, k: &str) -> Result<u64, Fault> {
    v[k].as_u64()
        .ok_or_else(|| fault(400, &format!("{k} unsigned integer required")))
}

fn dispatch(conn: i32, v: &Value) -> Result<Value, Fault> {
    let action = string(v, "action")?;
    if action == "capabilities" {
        #[cfg(windows)]
        let hdr = !scrap::dxgi::hdr::UNAVAILABLE.load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(windows))]
        let hdr = false;
        return Ok(json!({"privacy_screen":privacy_supported(),"hdr":hdr}));
    }
    if action.starts_with("clipboard.") {
        return clipboard(v);
    }
    if !AUX
        .lock()
        .unwrap()
        .as_ref()
        .map(|m| m.contains_key(&conn))
        .unwrap_or(false)
    {
        return Err(fault(409, "engine session not active"));
    }
    if action.starts_with("files.") {
        files_endpoint(conn, v)
    } else if action.starts_with("privacy.") {
        privacy(conn, v)
    } else {
        Err(fault(400, "unknown endpoint action"))
    }
}

#[no_mangle]
pub extern "C" fn yd_engine_endpoint(
    conn: c_int,
    input: *const u8,
    len: usize,
    out: *mut u8,
    cap: usize,
) -> c_int {
    if input.is_null() || out.is_null() || len > 24 << 20 || cap < 24 << 20 {
        return -1;
    }
    let parsed = serde_json::from_slice::<Value>(unsafe { std::slice::from_raw_parts(input, len) })
        .map_err(invalid);
    let result = parsed.and_then(|v| dispatch(conn, &v));
    let value = match result {
        Ok(v) => json!({"data":v}),
        Err(e) => json!({"error":e.message,"status":e.status}),
    };
    let Ok(bytes) = serde_json::to_vec(&value) else {
        return -1;
    };
    if bytes.len() > cap {
        return -1;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
    }
    bytes.len() as c_int
}

#[cfg(test)]
mod file_root_tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_short_alias_cannot_reach_staging_directory() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let root = std::env::temp_dir().join(format!("yoodesk-short-path-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let staging = root.join(".yoodesk-transfer-12345-67890");
        std::fs::create_dir(&staging).unwrap();
        let input: Vec<u16> = staging.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut short = vec![0u16; 32768];
        let length = unsafe {
            winapi::um::fileapi::GetShortPathNameW(
                input.as_ptr(),
                short.as_mut_ptr(),
                short.len() as u32,
            )
        };
        assert!(length > 0 && (length as usize) < short.len());
        let short = PathBuf::from(std::ffi::OsString::from_wide(&short[..length as usize]));
        let alias = root
            .join(short.file_name().unwrap())
            .join("payload.download");
        let result = validate_path_components(&root, &alias);
        std::fs::remove_dir_all(root).unwrap();
        assert!(matches!(result, Err(Fault { status: 400, .. })));
    }

    #[test]
    fn directory_listing_reports_truncation_and_hides_staging() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("yoodesk-list-test-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join(".YOODESK-TRANSFER-1-2")).unwrap();
        std::fs::write(root.join("visible"), b"data").unwrap();
        let (entries, truncated) = list_directory(&root).unwrap();
        assert!(!truncated);
        assert_eq!(
            entries,
            vec![json!({"name":"visible","type":"file","size":4})]
        );
        for index in 0..MAX_DIRECTORY_ENTRIES {
            std::fs::File::create(root.join(index.to_string())).unwrap();
        }
        let (entries, truncated) = list_directory(&root).unwrap();
        assert_eq!(entries.len(), MAX_DIRECTORY_ENTRIES);
        assert!(truncated);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn upload_publishes_open_file_without_replacing_destination() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "yoodesk-publish-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let source = root.join("payload.download");
        let destination = root.join("published");
        let mut upload = UploadPayload::create(&source).unwrap();
        upload.0.write_all(b"original").unwrap();
        upload.0.sync_all().unwrap();
        std::fs::rename(&source, root.join("moved")).unwrap();
        std::fs::write(&source, b"replaced").unwrap();
        std::fs::write(&destination, b"existing").unwrap();
        assert_eq!(
            upload.publish(&destination).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"existing");
        std::fs::remove_file(&destination).unwrap();
        upload.publish(&destination).unwrap();
        drop(upload);
        assert_eq!(std::fs::read(&destination).unwrap(), b"original");
        assert_eq!(std::fs::read(&source).unwrap(), b"replaced");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn staging_drop_preserves_replacement_and_cleans_owned_directory() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "yoodesk-staging-test-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = root.join(".yoodesk-transfer-7-42");
        let staging =
            StagingDirectory::create(7, 42, &path).unwrap_or_else(|err| panic!("{}", err.message));
        std::fs::write(path.join("payload"), b"original").unwrap();
        let moved = root.join("moved");
        std::fs::rename(&path, &moved).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("unrelated"), b"keep").unwrap();
        drop(staging);
        assert_eq!(std::fs::read(path.join("unrelated")).unwrap(), b"keep");
        assert_eq!(std::fs::read(moved.join("payload")).unwrap(), b"original");
        let owned = root.join(".yoodesk-transfer-7-43");
        let staging =
            StagingDirectory::create(7, 43, &owned).unwrap_or_else(|err| panic!("{}", err.message));
        std::fs::write(owned.join("payload"), b"temporary").unwrap();
        drop(staging);
        assert!(!owned.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn root_selection_rejects_paths_and_unknown_ids() {
        for id in ["C:\\", "../root", "drive-0", "unknown"] {
            let result = selected_root(&json!({"root_id": id}));
            assert!(matches!(result, Err(Fault { status: 400, .. })));
        }
    }

    #[test]
    fn system_root_keeps_relative_path_validation() {
        let roots = system_roots().unwrap_or_else(|_| panic!("system roots unavailable"));
        let id = &roots.first().expect("system root").0;
        for relative in [
            "../escape",
            "/etc",
            "C:\\Windows",
            "CON",
            ".yoodesk-transfer-1/payload",
            ".YOODESK-TRANSFER-1/payload.download",
            "nested/.YooDesk-Transfer-1/payload.download",
            "nested\\.YooDesk-Transfer-1\\payload.download",
        ] {
            let result = path(&json!({"root_id": id, "path": relative}), false);
            assert!(
                matches!(result, Err(Fault { status: 400, .. })),
                "{relative}"
            );
        }
    }

    #[test]
    fn displayed_windows_path_omits_extended_prefix() {
        assert_eq!(
            display_path(std::path::Path::new("\\\\?\\C:\\Users\\coder")),
            "C:\\Users\\coder"
        );
    }

    #[test]
    fn management_never_overwrites_and_rejects_root_and_busy_paths() {
        let directory = std::env::temp_dir()
            .canonicalize()
            .expect("temp")
            .join(format!("yoodesk-manage-{}", std::process::id()));
        std::fs::create_dir(&directory).expect("isolated test directory");
        let (id, root) = system_roots()
            .unwrap_or_else(|_| panic!("roots unavailable"))
            .into_iter()
            .find(|(_, root)| directory.starts_with(root.canonicalize().expect("root")))
            .expect("temp root");
        let relative = directory
            .strip_prefix(root.canonicalize().expect("root"))
            .expect("relative")
            .to_string_lossy();
        let request = |action: &str, suffix: &str, kind: &str| json!({"action":action,"root_id":id,"path":format!("{relative}/{suffix}"),"kind":kind});
        let mut guard = HashMap::new();
        assert!(manage_file(&request("files.create", "a.txt", "file"), &guard).is_ok());
        std::fs::write(directory.join("a.txt"), b"original").expect("content");
        assert!(matches!(
            manage_file(&request("files.create", "a.txt", "file"), &guard),
            Err(Fault { status: 409, .. })
        ));
        assert!(manage_file(&request("files.create", "folder", "directory"), &guard).is_ok());
        std::fs::write(directory.join("folder/nested"), b"child").expect("child");
        let mut rename = request("files.rename", "a.txt", "file");
        rename["name"] = json!("folder");
        assert!(manage_file(&rename, &guard).is_err());
        assert_eq!(
            std::fs::read(directory.join("a.txt")).expect("kept"),
            b"original"
        );
        rename["name"] = json!("b.txt");
        assert!(manage_file(&rename, &guard).is_ok());
        guard.insert(
            1,
            Job {
                staging: None,
                upload: None,
                conn: 1,
                transfer: None,
                size: 0,
                offset: 0,
                destination: None,
                temporary: None,
                source: Some(directory.join("b.txt")),
            },
        );
        assert!(matches!(
            manage_file(&request("files.delete", "b.txt", "file"), &guard),
            Err(Fault { status: 409, .. })
        ));
        guard.clear();
        assert!(manage_file(&request("files.delete", "b.txt", "directory"), &guard).is_err());
        assert!(manage_file(&request("files.delete", "b.txt", "file"), &guard).is_ok());
        assert!(manage_file(&request("files.delete", "folder", "directory"), &guard).is_ok());
        assert!(manage_file(
            &json!({"action":"files.delete","root_id":id,"path":"","kind":"directory"}),
            &guard
        )
        .is_err());
        std::fs::remove_dir(&directory).expect("cleanup");
    }

    #[test]
    fn duplicate_upload_names_keep_extension_and_windows_numbering() {
        let temporary_root = std::env::temp_dir().canonicalize().expect("temporary root");
        let directory = temporary_root.join(format!(
            "yoodesk-name-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir(&directory).expect("test directory");
        assert!(directory
            .canonicalize()
            .expect("canonical test directory")
            .starts_with(&temporary_root));
        for name in ["报告.tar.gz", "报告.tar (2).gz", "文件", ".env"] {
            std::fs::write(directory.join(name), b"original").expect("original file");
        }
        for (original, expected) in [
            ("报告.tar.gz", "报告.tar (3).gz"),
            ("报告.tar (2).gz", "报告.tar (3).gz"),
            ("文件", "文件 (2)"),
            (".env", ".env (2)"),
        ] {
            let result = available_destination(&directory.join(original), |_| false)
                .unwrap_or_else(|_| panic!("name allocation failed"));
            assert_eq!(fs::get_file_name(&result), expected);
            assert_eq!(
                std::fs::read(directory.join(original)).expect("original"),
                b"original"
            );
        }
        let result = available_destination(&directory.join("文件"), |candidate| {
            candidate.file_name().is_some_and(|name| name == "文件 (2)")
        })
        .unwrap_or_else(|_| panic!("reserved name allocation failed"));
        assert_eq!(fs::get_file_name(&result), "文件 (3)");
        std::fs::remove_dir_all(&directory).expect("test directory cleanup");
    }
}

/// 服务父进程专用文件入口，不初始化桌面或音频；沿用服务账户和业务授权。
#[no_mangle]
pub extern "C" fn yd_engine_file_endpoint(
    conn: c_int,
    input: *const u8,
    len: usize,
    out: *mut u8,
    cap: usize,
) -> c_int {
    if input.is_null() || out.is_null() || len > 24 << 20 || cap < 24 << 20 {
        return -1;
    }
    let result = serde_json::from_slice::<Value>(unsafe { std::slice::from_raw_parts(input, len) })
        .map_err(invalid)
        .and_then(|v| {
            if !string(&v, "action")?.starts_with("files.") {
                return Err(fault(400, "file action required"));
            }
            files_endpoint(conn, &v)
        });
    let value = match result {
        Ok(data) => json!({"data":data}),
        Err(error) => json!({"error":error.message,"status":error.status}),
    };
    let Ok(bytes) = serde_json::to_vec(&value) else {
        return -1;
    };
    if bytes.len() > cap {
        return -1;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
    }
    bytes.len() as c_int
}

/// 授权会话结束时只回收父进程文件任务，不触碰桌面 worker 或音频服务。
#[no_mangle]
pub extern "C" fn yd_engine_file_close(conn: c_int) -> c_int {
    jobs().lock().unwrap().retain(|_, job| job.conn != conn);
    0
}

#[cfg(target_os = "linux")]
pub(crate) fn recover_file_jobs() -> std::io::Result<()> {
    crate::file_journal::recover()
}
