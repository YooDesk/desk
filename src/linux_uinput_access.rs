//! C++式精确UID设备ACL租约：不改设备owner/group，不开放所有用户，最后持有者撤自身增量。
#![allow(unsafe_code)]
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::Command,
};
#[derive(Serialize, Deserialize, Default)]
struct State {
    dev: u64,
    ino: u64,
    base: String,
    users: BTreeMap<u32, Entry>,
}
#[derive(Serialize, Deserialize)]
struct Entry {
    before: Option<String>,
    holders: Vec<Holder>,
}
#[derive(Serialize, Deserialize)]
struct Holder {
    id: String,
    pid: u32,
    start: String,
    service: String,
}
pub struct Lease {
    device: Option<File>,
    id: String,
    uid: u32,
    available: bool,
}
impl Lease {
    pub fn available(&self) -> bool {
        self.available
    }
}
/// root只向指定UID追加rw；缺设备/工具或不安全ACL时明确不可用，X11不因此强制退出。
pub fn acquire(uid: u32, _groups: &[u32], service: &str) -> io::Result<Lease> {
    let unavailable = || Lease {
        device: None,
        id: String::new(),
        uid,
        available: false,
    };
    if uid == 0
        || service.is_empty()
        || service.len() > 80
        || !service
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(io::Error::other("invalid uinput lease identity"));
    }
    let device = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(0o400000 | 0o4000)
        .open("/dev/uinput")
    {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(%error,"uinput device unavailable; no permissions changed");
            return Ok(unavailable());
        }
    };
    if !device.metadata()?.file_type().is_char_device() {
        return Err(io::Error::other("uinput is not a character device"));
    }
    if !Path::new("/usr/bin/getfacl").exists() || !Path::new("/usr/bin/setfacl").exists() {
        tracing::warn!("getfacl/setfacl unavailable; uinput ACL not changed");
        return Ok(unavailable());
    }
    let holder_start = start(std::process::id())
        .ok_or_else(|| io::Error::other("cannot identify lease process"))?;
    let (_lock, path) = lock()?;
    let target = fd_path(&device);
    let meta = device.metadata()?;
    let mut acl = get(&target)?;
    let mut state = load(&path)?;
    if state.dev != meta.dev() || state.ino != meta.ino() {
        state = State {
            dev: meta.dev(),
            ino: meta.ino(),
            base: acl.clone(),
            users: BTreeMap::new(),
        };
    }
    let stale = state
        .users
        .iter_mut()
        .filter_map(|(id, entry)| {
            entry.holders.retain(alive);
            entry.holders.is_empty().then_some(*id)
        })
        .collect::<Vec<_>>();
    for id in stale {
        if let Some(entry) = state.users.remove(&id) {
            restore(&target, id, &entry, &acl)?;
            acl = get(&target)?;
        }
    }
    let created = !state.users.contains_key(&uid);
    if created {
        let before = user(&acl, uid);
        // 保留既有mask；若mask不能容纳rw，拒绝隐式扩展其他命名用户的有效权限。
        let mask = acl.lines().find_map(|line| line.strip_prefix("mask::"));
        if mask.is_some_and(|m| !m.contains('r') || !m.contains('w')) {
            tracing::warn!("existing uinput ACL mask cannot grant rw safely");
            save(&path, &state)?;
            return Ok(unavailable());
        }
        if before.as_deref() == Some("rw-") {
            save(&path, &state)?;
            return Ok(Lease {
                device: Some(device),
                id: String::new(),
                uid,
                available: true,
            });
        }
        set(&target, "-m", &format!("u:{uid}:rw-"), mask.is_some())?;
        state.users.insert(
            uid,
            Entry {
                before,
                holders: Vec::new(),
            },
        );
    }
    let current = get(&target)?;
    if user(&current, uid).as_deref() != Some("rw-")
        || current
            .lines()
            .find_map(|line| line.strip_prefix("mask::"))
            .is_some_and(|mask| !mask.contains('r') || !mask.contains('w'))
    {
        tracing::warn!("uinput ACL changed externally; not taking ownership");
        return Ok(unavailable());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let holder = Holder {
        id: id.clone(),
        pid: std::process::id(),
        start: holder_start,
        service: service.into(),
    };
    state.users.get_mut(&uid).unwrap().holders.push(holder);
    if let Err(error) = save(&path, &state) {
        if created {
            if let Some(entry) = state.users.get(&uid) {
                let _ = restore(&target, uid, entry, &get(&target)?);
            }
        }
        return Err(error);
    }
    Ok(Lease {
        device: Some(device),
        id,
        uid,
        available: true,
    })
}
impl Drop for Lease {
    fn drop(&mut self) {
        if self.id.is_empty() {
            return;
        }
        if let Err(error) = self.release() {
            tracing::error!(%error,"uinput ACL lease cleanup failed");
        }
    }
}
impl Lease {
    /// 仅撤本租约；同UID其他实例仍持有时不撤权限，设备inode换代也不误改新节点。
    fn release(&mut self) -> io::Result<()> {
        let Some(device) = self.device.as_ref() else {
            return Ok(());
        };
        let meta = device.metadata()?;
        let (_lock, path) = lock()?;
        let mut state = load(&path)?;
        if state.dev != meta.dev() || state.ino != meta.ino() {
            return Ok(());
        }
        let target = fd_path(device);
        let mut remove = false;
        if let Some(entry) = state.users.get_mut(&self.uid) {
            entry.holders.retain(|h| h.id != self.id && alive(h));
            remove = entry.holders.is_empty();
        }
        if remove {
            if let Some(entry) = state.users.remove(&self.uid) {
                restore(&target, self.uid, &entry, &get(&target)?)?;
            }
        }
        if state.users.is_empty() {
            let current = get(&target)?;
            if !state.base.lines().any(|line| line.starts_with("mask::"))
                && without_mask(&current) == without_mask(&state.base)
            {
                tool("/usr/bin/setfacl", &["-b", &target])?;
            }
        }
        save(&path, &state)
    }
}
/// 固定工具路径和三秒期限；设备通过父FD引用，udev换节点不把旧回滚应用到新设备。
fn tool(program: &str, args: &[&str]) -> io::Result<String> {
    let output = Command::new("/usr/bin/timeout")
        .arg("3")
        .arg(program)
        .args(args)
        .env("LC_ALL", "C")
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
fn fd_path(file: &File) -> String {
    format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd())
}
fn get(target: &str) -> io::Result<String> {
    Ok(tool("/usr/bin/getfacl", &["-c", "-n", "-p", target])?
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}
fn user(acl: &str, uid: u32) -> Option<String> {
    let prefix = format!("user:{uid}:");
    acl.lines()
        .find_map(|line| line.strip_prefix(&prefix).map(str::to_owned))
}
fn without_mask(acl: &str) -> String {
    acl.lines()
        .filter(|line| !line.starts_with("mask::"))
        .collect::<Vec<_>>()
        .join("\n")
}
fn set(target: &str, op: &str, value: &str, no_mask: bool) -> io::Result<()> {
    let mut args = Vec::new();
    if no_mask {
        args.push("-n");
    }
    args.extend([op, value, target]);
    tool("/usr/bin/setfacl", &args).map(|_| ())
}
fn restore(target: &str, uid: u32, entry: &Entry, acl: &str) -> io::Result<()> {
    if user(acl, uid).as_deref() != Some("rw-") {
        return Ok(());
    }
    match &entry.before {
        Some(before) => set(target, "-m", &format!("u:{uid}:{before}"), true),
        None => set(target, "-x", &format!("u:{uid}"), true),
    }
}
/// PID加启动时刻识别租约持有者，避免PID复用误判仍存活。
fn start(pid: u32) -> Option<String> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    raw.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}
fn alive(holder: &Holder) -> bool {
    start(holder.pid).as_deref() == Some(holder.start.as_str())
}
/// 根专属锁/记录，不跟随软链接；序列化多个本组件实例对同设备ACL的增量操作。
fn lock() -> io::Result<(File, PathBuf)> {
    let directory = Path::new("/run").join(format!("{}-uinput-access", env!("PRODUCT_ID")));
    match fs::create_dir(&directory) {
        Ok(()) => fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = directory.symlink_metadata()?;
    if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other("untrusted uinput lease directory"));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(0o400000)
        .open(directory.join("lock"))?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
        return Err(io::Error::other("untrusted uinput lease lock"));
    }
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    if unsafe { flock(file.as_raw_fd(), 2) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((file, directory.join("leases.json")))
}
fn load(path: &Path) -> io::Result<State> {
    match OpenOptions::new()
        .read(true)
        .custom_flags(0o400000)
        .open(path)
    {
        Ok(file) => {
            let meta = file.metadata()?;
            if !meta.is_file()
                || meta.uid() != 0
                || meta.mode() & 0o077 != 0
                || meta.nlink() != 1
                || meta.len() > 1 << 20
            {
                return Err(io::Error::other("untrusted uinput lease record"));
            }
            serde_json::from_reader(file).map_err(io::Error::other)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e),
    }
}
fn save(path: &Path, state: &State) -> io::Result<()> {
    use io::Write;
    let temporary = path.with_extension(uuid::Uuid::new_v4().to_string());
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(0o400000)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(&serde_json::to_vec(state).map_err(io::Error::other)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires a dedicated root process and real /dev/uinput on the Linux VM"]
    fn exact_uid_two_leases_preserve_other_acl_and_restore_increment() {
        use std::os::unix::process::CommandExt;
        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0o4000)
            .open("/dev/uinput")
            .unwrap();
        let target = fd_path(&device);
        let before = get(&target).unwrap();
        let uid = 60000;
        assert!(
            user(&before, uid).is_none(),
            "dedicated test UID must have no preexisting ACL"
        );
        let first = acquire(uid, &[uid], "YooDeskAclReviewA").unwrap();
        assert!(first.available());
        let second = acquire(uid, &[uid], "YooDeskAclReviewB").unwrap();
        assert!(second.available());
        let mut child = Command::new("/usr/bin/python3");
        child.args([
            "-c",
            "import os; f=os.open('/dev/uinput',os.O_RDWR|os.O_NONBLOCK); os.close(f)",
        ]);
        unsafe {
            child.pre_exec(move || {
                unsafe extern "C" {
                    fn setgroups(n: usize, g: *const u32) -> i32;
                    fn setgid(g: u32) -> i32;
                    fn setuid(u: u32) -> i32;
                }
                if setgroups(0, std::ptr::null()) != 0 || setgid(uid) != 0 || setuid(uid) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        assert!(
            child.status().unwrap().success(),
            "actual unprivileged UID must open granted device"
        );
        drop(first);
        assert_eq!(user(&get(&target).unwrap(), uid).as_deref(), Some("rw-"));
        drop(second);
        assert_eq!(
            get(&target).unwrap(),
            before,
            "other users and original ACL must survive"
        );
    }
}
