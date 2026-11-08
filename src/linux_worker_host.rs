//! Linux 服务父进程只拥有 HTTP；root helper 与用户 desktop/audio 按同一会话代次回收。
#![allow(unsafe_code)]
use crate::{business::BusinessHub, desk::engine::rpc, unix_worker::UnixEndpoint};
use std::{
    collections::BTreeMap,
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;
#[path = "linux_desktop_env.rs"]
mod environment;
#[path = "linux_worker_process.rs"]
mod process;
use process::Worker;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Desktop {
    session: String,
    uid: u32,
    gid: u32,
    groups: Vec<u32>,
    env: BTreeMap<String, String>,
}
// 所有外部会话查询设硬期限，不能让 logind/NSS 卡住 HTTP 或停止流程。
fn query(program: &str, args: &[&str]) -> io::Result<std::process::Output> {
    std::process::Command::new("/usr/bin/timeout")
        .args(["3", program])
        .args(args)
        .env("LC_ALL", "C")
        .output()
}
// 在父进程解析 initgroups 的完整 NSS 组列表，避免多线程 fork 后调用 NSS 分配器。
fn groups(user: &str, gid: u32) -> io::Result<Vec<u32>> {
    let output = query("/usr/bin/id", &["-G", "--", user])?;
    if !output.status.success() {
        return Err(io::Error::other("user supplementary group lookup failed"));
    }
    let mut groups = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .map(|value| value.parse::<u32>().map_err(io::Error::other))
        .collect::<io::Result<Vec<_>>>()?;
    if groups.len() > 65536 || !groups.contains(&gid) {
        return Err(io::Error::other("invalid supplementary groups"));
    }
    groups.sort_unstable();
    groups.dedup();
    Ok(groups)
}
// logind 选定会话，NSS 提供真实 HOME/用户/主组；未登录时不伪造 root desktop。
fn desktop() -> io::Result<Desktop> {
    let seat = query(
        "/usr/bin/loginctl",
        &["show-seat", "seat0", "-p", "ActiveSession", "--value"],
    )?;
    if !seat.status.success() {
        return Err(io::Error::other("loginctl seat query failed"));
    }
    let session = String::from_utf8_lossy(&seat.stdout).trim().to_owned();
    if session.is_empty() {
        return Ok(Desktop::default());
    }
    let properties = query(
        "/usr/bin/loginctl",
        &[
            "show-session",
            &session,
            "-p",
            "User",
            "-p",
            "Display",
            "-p",
            "Type",
        ],
    )?;
    if !properties.status.success() {
        return Err(io::Error::other("loginctl session query failed"));
    }
    let text = String::from_utf8_lossy(&properties.stdout);
    let properties: BTreeMap<_, _> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let uid = properties
        .get("User")
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|uid| *uid != 0)
        .ok_or_else(|| io::Error::other("a non-root graphical user is required"))?;
    let account = query("/usr/bin/getent", &["passwd", &uid.to_string()])?;
    if !account.status.success() {
        return Err(io::Error::other("graphical user NSS lookup failed"));
    }
    let account = String::from_utf8_lossy(&account.stdout);
    let fields: Vec<_> = account.trim().split(':').collect();
    if fields.len() != 7
        || fields[2].parse::<u32>().ok() != Some(uid)
        || fields[0].is_empty()
        || !std::path::Path::new(fields[5]).is_absolute()
    {
        return Err(io::Error::other("invalid graphical user account"));
    }
    let gid = fields[3].parse::<u32>().map_err(io::Error::other)?;
    let groups = groups(fields[0], gid)?;
    let mut env = BTreeMap::from([
        ("HOME".into(), fields[5].into()),
        ("USER".into(), fields[0].into()),
        ("LOGNAME".into(), fields[0].into()),
        ("SHELL".into(), fields[6].into()),
        ("XDG_SESSION_ID".into(), session.clone()),
        ("XDG_RUNTIME_DIR".into(), format!("/run/user/{uid}")),
        (
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!("unix:path=/run/user/{uid}/bus"),
        ),
        (
            "PULSE_SERVER".into(),
            format!("unix:/run/user/{uid}/pulse/native"),
        ),
    ]);
    if let Some(display) = properties.get("Display").filter(|v| !v.is_empty()) {
        env.insert("DISPLAY".into(), display.to_string());
    }
    if let Some(kind) = properties.get("Type") {
        env.insert("XDG_SESSION_TYPE".into(), kind.to_string());
    }
    environment::complete(&mut env, &session, uid)?;
    let kind = env
        .get("XDG_SESSION_TYPE")
        .map(String::as_str)
        .unwrap_or("x11");
    env.insert(
        "RUSTDESK_FORCED_DISPLAY_SERVER".into(),
        if kind == "wayland" { "wayland" } else { "x11" }.into(),
    );
    Ok(Desktop {
        session,
        uid,
        gid,
        groups,
        env,
    })
}
// 先撤销代理使阻塞 IO 立即取消，再关闭本代业务并回收两个角色。
async fn clear(business: &Arc<BusinessHub>, worker: &mut Option<Worker>) {
    rpc::set_transport(None);
    let business = business.clone();
    let retired = worker.take();
    if let Err(err) = tokio::task::spawn_blocking(move || {
        business.desk.close_all("desktop_session_changed");
        drop(retired);
    })
    .await
    {
        tracing::error!(%err, "desktop worker cleanup task failed");
    }
}
// root helper 先认证并完成私有 native IPC 初始化，用户子进程之后才能订阅该代。
pub async fn supervise(
    business: Arc<BusinessHub>,
    mut stop: watch::Receiver<bool>,
    _sessions: watch::Receiver<u32>,
) {
    let records = match process::records().and_then(|d| {
        process::recover(&d)?;
        Ok(d)
    }) {
        Ok(v) => v,
        Err(error) => {
            tracing::error!(%error,"worker records unavailable; HTTP remains available");
            return;
        }
    };
    let mut worker: Option<Worker> = None;
    let mut next_start = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        if *stop.borrow() {
            break;
        }
        let selected = match tokio::task::spawn_blocking(desktop).await {
            Ok(Ok(value)) => Some(value),
            Ok(Err(error)) => {
                tracing::warn!(%error,"graphical session query failed; retaining current generation");
                None
            }
            Err(error) => {
                tracing::warn!(%error,"graphical session task failed");
                None
            }
        };
        let dead = worker.is_some() && !rpc::worker_healthy()
            || worker.as_mut().is_some_and(|w| !w.alive());
        let changed = worker
            .as_ref()
            .is_some_and(|w| selected.as_ref().is_some_and(|s| s != &w.desktop));
        if dead || changed {
            clear(&business, &mut worker).await;
            next_start = Instant::now()
                + if dead {
                    Duration::from_secs(5)
                } else {
                    Duration::ZERO
                };
        }
        if worker.is_none() && Instant::now() >= next_start {
            if let Some(selected) = selected.filter(|s| s.uid != 0) {
                let result=async{
                    let mut child=process::spawn(selected,&records)?;
                    let wayland=child.desktop.env.get("XDG_SESSION_TYPE").is_some_and(|kind|kind=="wayland");
                    if wayland && !child.prefers_x11() && std::env::var("YOODESK_ENGINE_DRM_REQUESTED").ok().as_deref().unwrap_or("1")=="1" {
                        for probe in [true,false] {
                            let endpoint=UnixEndpoint::new_for_user(0,0)?;
                            let pid=child.start_helper(endpoint.name(),probe)?;
                            let connected=tokio::task::spawn_blocking(move||endpoint.accept(pid,0,Duration::from_secs(3)));
                            let result=tokio::select!{r=connected=>r.map_err(|e|io::Error::other(e.to_string()))?,_=stop.changed()=>return Err(io::Error::other("service stopping"))};
                            match result {
                                Ok(transport) if probe=>{child.helper_control=Some(transport);child.finish_probe();},
                                Ok(transport)=>child.enable_drm(transport),
                                Err(error)=>{tracing::info!(%error,"DRM实际帧探测或初始化失败；用户采集后端继续，未启动DRM producer");child.finish_probe();break;}
                            }
                        }
                    }
                    let uid=child.desktop.uid;let endpoint=UnixEndpoint::new_for_user(uid,child.desktop.gid)?;
                    let pid=child.start_user(endpoint.name())?;
                    let connected=tokio::task::spawn_blocking(move||endpoint.accept(pid,uid,Duration::from_secs(3)));
                    let transport=tokio::select!{r=connected=>r.map_err(|e|io::Error::other(e.to_string()))??,_=stop.changed()=>return Err(io::Error::other("service stopping"))};
                    if *stop.borrow(){return Err(io::Error::other("service stopped"));}rpc::set_transport(Some(transport));Ok::<Worker,io::Error>(child)
                }.await;
                match result {
                    Ok(child) => {
                        tracing::info!(user_pid=child.user_id(),helper_pid=child.helper_id(),uid=child.desktop.uid,session=%child.desktop.session,"user desktop/audio connected; DRM-only helper is optional");
                        worker = Some(child);
                    }
                    Err(error) => {
                        clear(&business, &mut worker).await;
                        tracing::warn!(%error,"desktop subprocess startup failed; HTTP remains available");
                        next_start = Instant::now() + Duration::from_secs(5);
                    }
                }
            }
        }
        tokio::select! {_=tick.tick()=>{},_=stop.changed()=>{}}
    }
    clear(&business, &mut worker).await;
    let _ = std::fs::remove_dir(&records);
    if let Some(parent) = records.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}
