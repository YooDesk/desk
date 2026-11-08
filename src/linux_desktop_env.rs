//! 对照UniRemoteLite的活动用户环境发现：允许systemd用户进程不带session id，
//! 但只使用目标UID、匹配runtime和已验证Wayland socket的显示环境。
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};
type Environment = BTreeMap<String, String>;
const KEYS: &[&str] = &[
    "DISPLAY",
    "XAUTHORITY",
    "WAYLAND_DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "DESKTOP_SESSION",
    "GDMSESSION",
    "LANG",
    "LC_ALL",
];

/// 同UID进程的环境只取白名单；不从服务worker自身读取旧代环境。
fn user_environments(uid: u32) -> io::Result<Vec<(u32, Environment)>> {
    let mut result = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let Ok(entry) = entry else { continue };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if entry.metadata().map(|m| m.uid() != uid).unwrap_or(true) {
            continue;
        }
        if fs::read_to_string(entry.path().join("comm")).is_ok_and(|s| s.trim() == "web-service") {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        let env = bytes
            .split(|b| *b == 0)
            .filter_map(|s| std::str::from_utf8(s).ok()?.split_once('='))
            .filter(|(key, _)| {
                KEYS.contains(key)
                    || ["XDG_SESSION_ID", "XDG_RUNTIME_DIR", "XDG_SESSION_TYPE"].contains(key)
            })
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        result.push((pid, env));
    }
    result.sort_by_key(|(pid, _)| *pid);
    Ok(result)
}

/// 只接受实际存在、属于目标用户的Unix socket，不硬编码wayland-0或选择.lock文件。
fn wayland_sockets(runtime: &Path, uid: u32) -> io::Result<Vec<String>> {
    let metadata = fs::symlink_metadata(runtime)?;
    if !metadata.is_dir() || metadata.uid() != uid {
        return Err(io::Error::other("untrusted graphical runtime directory"));
    }
    let mut sockets = Vec::new();
    for entry in fs::read_dir(runtime)? {
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with("wayland-")
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            continue;
        }
        if entry
            .path()
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_socket() && m.uid() == uid)
        {
            sockets.push(name);
        }
    }
    sockets.sort();
    Ok(sockets)
}

/// 根据活动会话及经过OS验证的socket选显示环境；显式其他session始终拒绝。
fn select(
    base: &Environment,
    session: &str,
    candidates: &[(u32, Environment)],
    sockets: &[String],
) -> io::Result<Environment> {
    let runtime = base
        .get("XDG_RUNTIME_DIR")
        .map(String::as_str)
        .unwrap_or("");
    let kind = base
        .get("XDG_SESSION_TYPE")
        .map(String::as_str)
        .unwrap_or("x11");
    let mut best = None;
    for (_, env) in candidates {
        let sid = env.get("XDG_SESSION_ID").filter(|s| !s.is_empty());
        if sid.is_some_and(|s| s != session) {
            continue;
        }
        if env.get("XDG_SESSION_TYPE").is_some_and(|s| s != kind) {
            continue;
        }
        if env.get("XDG_RUNTIME_DIR").is_some_and(|s| s != runtime) {
            continue;
        }
        let wayland = env.get("WAYLAND_DISPLAY").filter(|s| sockets.contains(s));
        if env.get("WAYLAND_DISPLAY").is_some_and(|s| !s.is_empty()) && wayland.is_none() {
            continue;
        }
        // 没有session id的用户管理器进程需匹配runtime；多个socket时不能猜是哪一席。
        if sid.is_none()
            && (env.get("XDG_RUNTIME_DIR").map(String::as_str) != Some(runtime)
                || (kind == "wayland" && (sockets.len() != 1 || wayland.is_none())))
        {
            continue;
        }
        let display = env.get("DISPLAY").is_some_and(|s| !s.is_empty());
        if !display && wayland.is_none() {
            continue;
        }
        let score = usize::from(sid.is_some()) * 100
            + usize::from(wayland.is_some()) * 20
            + usize::from(display) * 10
            + usize::from(env.contains_key("XAUTHORITY")) * 5;
        if best.as_ref().is_none_or(|(old, _)| score > *old) {
            best = Some((score, env));
        }
    }
    let mut result = base.clone();
    if let Some((_, env)) = best {
        // DISPLAY与XAUTHORITY来自同一候选进程，避免把两条XWayland连接拼接。
        for key in KEYS {
            if let Some(value) = env.get(*key).filter(|v| !v.is_empty()) {
                result.insert((*key).into(), value.clone());
            }
        }
    }
    if kind == "wayland" && !result.contains_key("WAYLAND_DISPLAY") {
        match sockets {
            [socket] => {
                result.insert("WAYLAND_DISPLAY".into(), socket.clone());
            }
            [] => {
                return Err(io::Error::other(
                    "active Wayland session has no owned display socket",
                ))
            }
            _ => return Err(io::Error::other("active Wayland display is ambiguous")),
        }
    }
    Ok(result)
}

/// 补齐活动用户的显示及D-Bus白名单；不改变logind确定的用户、会话类型或runtime。
pub(super) fn complete(env: &mut Environment, session: &str, uid: u32) -> io::Result<()> {
    let runtime = env
        .get("XDG_RUNTIME_DIR")
        .ok_or_else(|| io::Error::other("missing graphical runtime"))?;
    let sockets = wayland_sockets(Path::new(runtime), uid)?;
    *env = select(env, session, &user_environments(uid)?, &sockets)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 构造隔离会话环境，不访问实际桌面或进程。
    fn env(values: &[(&str, &str)]) -> Environment {
        values
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
    /// systemd启动的Portal缺少XDG_SESSION_ID仍能补齐同用户Wayland环境。
    #[test]
    fn user_manager_environment_without_session_id_is_accepted() {
        let base = env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]);
        let candidate = env(&[
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("DISPLAY", ":0"),
            ("XAUTHORITY", "/run/user/1000/xauth"),
            ("LD_PRELOAD", "unsafe"),
        ]);
        let result = select(&base, "3", &[(4, candidate)], &["wayland-1".into()]).unwrap();
        assert_eq!(result["WAYLAND_DISPLAY"], "wayland-1");
        assert_eq!(result["DISPLAY"], ":0");
        assert_eq!(result["XAUTHORITY"], "/run/user/1000/xauth");
        assert!(!result.contains_key("LD_PRELOAD"));
    }
    /// 不允许其他会话或其他runtime污染当前席位。
    #[test]
    fn explicit_other_session_and_runtime_are_ignored() {
        let base = env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]);
        let bad = env(&[
            ("XDG_SESSION_ID", "9"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":9"),
        ]);
        let other = env(&[
            ("XDG_RUNTIME_DIR", "/run/user/120"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":8"),
        ]);
        let result = select(&base, "3", &[(1, bad), (2, other)], &["wayland-0".into()]).unwrap();
        assert!(!result.contains_key("DISPLAY"));
        assert_eq!(result["WAYLAND_DISPLAY"], "wayland-0");
    }
    /// 多socket场景只接受明确属于活动session的进程，不按目录顺序猜测。
    #[test]
    fn multiple_sockets_require_an_explicit_session_match() {
        let base = env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]);
        let mut candidate = env(&[
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("WAYLAND_DISPLAY", "wayland-1"),
        ]);
        let sockets = vec!["wayland-0".into(), "wayland-1".into()];
        assert!(select(&base, "3", &[(1, candidate.clone())], &sockets).is_err());
        candidate.insert("XDG_SESSION_ID".into(), "3".into());
        assert_eq!(
            select(&base, "3", &[(1, candidate)], &sockets).unwrap()["WAYLAND_DISPLAY"],
            "wayland-1"
        );
    }
    /// X11仍按明确session环境工作，不要求存在Wayland socket。
    #[test]
    fn x11_environment_keeps_its_paired_authority() {
        let base = env(&[
            ("XDG_SESSION_TYPE", "x11"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]);
        let candidate = env(&[
            ("XDG_SESSION_ID", "3"),
            ("DISPLAY", ":2"),
            ("XAUTHORITY", "/tmp/xauth"),
        ]);
        assert_eq!(
            select(&base, "3", &[(1, candidate)], &[]).unwrap()["DISPLAY"],
            ":2"
        );
    }
    /// socket缺失必须明确失败，不能默认一个不存在的wayland-0。
    #[test]
    fn missing_wayland_socket_is_not_fabricated() {
        let base = env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]);
        assert!(select(&base, "3", &[], &[]).is_err());
    }
}
