//! macOS 文件操作锚定到已打开的目录描述符，不跟随中途替换的符号链接。
use hbb_common::libc;
use std::{
    ffi::{CStr, CString, OsStr},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::{Component, Path},
};

fn name(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}

fn check(value: i32) -> io::Result<()> {
    if value == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn open_directory(path: &Path) -> io::Result<File> {
    let mut directory = File::open("/")?;
    if !path.is_absolute() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    for component in path.components() {
        let Component::Normal(part) = component else {
            if component == Component::RootDir {
                continue;
            }
            return Err(io::ErrorKind::InvalidInput.into());
        };
        let part = name(part)?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                part.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        check(fd)?;
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

fn parent(path: &Path) -> io::Result<(File, CString)> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
    Ok((
        open_directory(path.parent().ok_or_else(invalid)?)?,
        name(path.file_name().ok_or_else(invalid)?)?,
    ))
}

pub(crate) fn clone_payload(source: &File, destination: &Path) -> io::Result<()> {
    let (directory, target) = parent(destination)?;
    // fclonefileat 从打开的源文件发布，不会覆盖已存在的目标。
    check(unsafe {
        libc::fclonefileat(
            source.as_raw_fd(),
            directory.as_raw_fd(),
            target.as_ptr(),
            0,
        )
    })
}

pub(crate) fn rename_new(source: &Path, destination: &Path) -> io::Result<()> {
    if source.parent() != destination.parent() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let (dir, source) = parent(source)?;
    let destination = name(destination.file_name().ok_or(io::ErrorKind::InvalidInput)?)?;
    let fd = dir.as_raw_fd();
    check(unsafe {
        libc::renameatx_np(
            fd,
            source.as_ptr(),
            fd,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    })
}

fn entry_mode(dir: &File, entry: &CStr) -> io::Result<libc::mode_t> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    check(unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            entry.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    Ok(unsafe { metadata.assume_init() }.st_mode & libc::S_IFMT)
}

pub(crate) fn manage(path: &Path, create: bool, directory: bool) -> io::Result<()> {
    let (dir, entry) = parent(path)?;
    let fd = dir.as_raw_fd();
    if create {
        if directory {
            return check(unsafe { libc::mkdirat(fd, entry.as_ptr(), 0o700) });
        }
        let file = unsafe {
            libc::openat(
                fd,
                entry.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                0o600 as libc::c_int,
            )
        };
        check(file)?;
        drop(unsafe { File::from_raw_fd(file) });
        return Ok(());
    }
    let expected = if directory {
        libc::S_IFDIR
    } else {
        libc::S_IFREG
    };
    if entry_mode(&dir, &entry)? != expected {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    check(unsafe {
        libc::unlinkat(
            fd,
            entry.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    })
}
