//! 用户 worker 的未提交上传恢复清单；只检查本用户、本服务已记录的目录。
use serde_json::{json, Value};
use std::{
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
pub(crate) struct Lease {
    record: Option<PathBuf>,
    _handle: std::fs::File,
    temporary: PathBuf,
    dev: u64,
    ino: u64,
}
// 上传任务正常关闭或初始化失败都回收临时目录；已提交目的文件从不进入清单。
impl Drop for Lease {
    fn drop(&mut self) {
        if let Err(error) = remove_temporary(&self.temporary, self.dev, self.ino) {
            log::warn!("upload cleanup journal: {error}");
            return;
        }
        if let Some(record) = &self.record {
            let _ = std::fs::remove_file(record);
        }
    }
}
// 服务名只能由已认证父环境提供；临时清单根使用与公开文件 API 相同的内部路径禁用前缀。
fn directory() -> io::Result<Option<PathBuf>> {
    let Some(name) = std::env::var_os("YOODESK_ENGINE_SERVICE_NAME") else {
        return Ok(None);
    };
    let name = name
        .to_str()
        .ok_or_else(|| io::Error::other("invalid transfer service name"))?;
    if name.is_empty()
        || matches!(name, "." | "..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(io::Error::other("invalid transfer service name"));
    }
    let uid = unsafe { hbb_common::libc::geteuid() };
    if uid == 0 {
        return Err(io::Error::other(
            "transfer journal requires a user identity",
        ));
    }
    let runtime = PathBuf::from(format!("/run/user/{uid}"));
    let metadata = runtime.symlink_metadata()?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::other("untrusted user runtime"));
    }
    let product = runtime.join(format!(".yoodesk-transfer-{}", env!("PRODUCT_ID")));
    let service = product.join(name);
    for path in [&product, &service] {
        match std::fs::create_dir(path) {
            Ok(()) => std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let m = path.symlink_metadata()?;
        if !m.is_dir() || m.uid() != uid || m.mode() & 0o077 != 0 {
            return Err(io::Error::other("untrusted transfer journal directory"));
        }
    }
    Ok(Some(service))
}
// 目录路径、设备和 inode 都必须仍匹配创建时状态，不能沿符号链接或删替换后的用户目录。
fn remove_temporary(path: &Path, dev: u64, ino: u64) -> io::Result<()> {
    let metadata = match path.symlink_metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !metadata.is_dir()
        || metadata.uid() != unsafe { hbb_common::libc::geteuid() }
        || metadata.dev() != dev
        || metadata.ino() != ino
    {
        return Err(io::Error::other("upload directory identity changed"));
    }
    // 同父原子移入私有墓碑名后再核 inode；若原路径在核验期间被替换，不能删该替代目录。
    use std::os::unix::ffi::OsStrExt;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let tomb = path.with_file_name(format!(
        ".yoodesk-transfer-cleanup-{}-{nonce}",
        std::process::id()
    ));
    let rename = |from: &Path, to: &Path| -> io::Result<()> {
        let from = std::ffi::CString::new(from.as_os_str().as_bytes()).map_err(io::Error::other)?;
        let to = std::ffi::CString::new(to.as_os_str().as_bytes()).map_err(io::Error::other)?;
        if unsafe {
            hbb_common::libc::syscall(
                hbb_common::libc::SYS_renameat2,
                hbb_common::libc::AT_FDCWD,
                from.as_ptr(),
                hbb_common::libc::AT_FDCWD,
                to.as_ptr(),
                hbb_common::libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    };
    rename(path, &tomb)?;
    let moved = tomb.symlink_metadata()?;
    if !moved.is_dir()
        || moved.uid() != unsafe { hbb_common::libc::geteuid() }
        || moved.dev() != dev
        || moved.ino() != ino
    {
        let _ = rename(&tomb, path);
        return Err(io::Error::other(
            "upload directory was replaced during recovery",
        ));
    }
    std::fs::remove_dir_all(tomb)
}
// 只有 upload 创建磁盘临时目录；先持久清单再向浏览器确认任务创建。
pub(crate) fn create(conn: i32, id: i32, temporary: &Path) -> io::Result<Lease> {
    use std::os::unix::fs::DirBuilderExt;
    let directory = directory()?;
    std::fs::DirBuilder::new().mode(0o700).create(temporary)?;
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(hbb_common::libc::O_DIRECTORY | hbb_common::libc::O_NOFOLLOW)
        .open(temporary)?;
    let metadata = handle.metadata()?;
    if !metadata.is_dir() || metadata.uid() != unsafe { hbb_common::libc::geteuid() } {
        return Err(io::Error::other("invalid upload temporary directory"));
    }
    let mut lease = Lease {
        record: None,
        _handle: handle,
        temporary: temporary.to_path_buf(),
        dev: metadata.dev(),
        ino: metadata.ino(),
    };
    let Some(directory) = directory else {
        return Ok(lease);
    };
    let record = directory.join(format!("job-{}-{id}.json", std::process::id()));
    let value = json!({"v":1,"pid":std::process::id(),"conn":conn,"id":id,"temporary":temporary,"dev":metadata.dev(),"ino":metadata.ino()});
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(0o400000)
        .open(&record)?;
    lease.record = Some(record);
    let result = file
        .write_all(&serde_json::to_vec(&value).map_err(io::Error::other)?)
        .and_then(|_| file.sync_all());
    drop(file);
    result?;
    Ok(lease)
}
// 新 worker 只恢复同 UID/service 的已死旧进程任务，不遍历 Downloads 或系统文件根。
pub(crate) fn recover() -> io::Result<()> {
    let Some(directory) = directory()? else {
        return Ok(());
    };
    let uid = unsafe { hbb_common::libc::geteuid() };
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let m = entry.path().symlink_metadata()?;
        if !m.is_file() || m.uid() != uid || m.mode() & 0o077 != 0 || m.nlink() != 1 {
            return Err(io::Error::other("untrusted transfer recovery record"));
        }
        use std::io::Read;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0o400000)
            .open(entry.path())?;
        let mut bytes = Vec::new();
        file.by_ref().take(8192).read_to_end(&mut bytes)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let pid = value["pid"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .filter(|p| *p != 0)
            .ok_or_else(|| io::Error::other("invalid transfer PID"))?;
        let id = value["id"]
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| io::Error::other("invalid transfer id"))?;
        let conn = value["conn"]
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| io::Error::other("invalid transfer connection"))?;
        if value["v"] != 1 || entry.file_name() != format!("job-{pid}-{id}.json").as_str() {
            return Err(io::Error::other("invalid transfer record name"));
        }
        if Path::new("/proc").join(pid.to_string()).exists() {
            continue;
        }
        let temporary = PathBuf::from(
            value["temporary"]
                .as_str()
                .ok_or_else(|| io::Error::other("invalid temporary path"))?,
        );
        if !temporary.is_absolute()
            || temporary.file_name().and_then(|s| s.to_str())
                != Some(format!(".yoodesk-transfer-{conn}-{id}").as_str())
        {
            return Err(io::Error::other("invalid recorded temporary path"));
        }
        remove_temporary(
            &temporary,
            value["dev"]
                .as_u64()
                .ok_or_else(|| io::Error::other("invalid device"))?,
            value["ino"]
                .as_u64()
                .ok_or_else(|| io::Error::other("invalid inode"))?,
        )?;
        std::fs::remove_file(entry.path())?;
    }
    Ok(())
}
