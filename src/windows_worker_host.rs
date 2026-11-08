//! SYSTEM 服务持有桌面 worker 的 Job；会话代次切换先撤销业务，再回收 IPC 与进程。
#![allow(unsafe_code)]
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};

// Migrated from the service host: preserve CommandLineToArgvW quoting for worker re-entry.
fn worker_command(exe: &std::path::Path, pipe: &str, parent: u32) -> Vec<u16> {
    fn quote(value: &OsStr) -> OsString {
        let mut output = vec![b'"' as u16];
        let mut slashes = 0;
        for value in value.encode_wide() {
            if value == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            output.extend(std::iter::repeat_n(
                b'\\' as u16,
                if value == b'"' as u16 {
                    slashes * 2 + 1
                } else {
                    slashes
                },
            ));
            slashes = 0;
            output.push(value);
        }
        output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        output.push(b'"' as u16);
        OsString::from_wide(&output)
    }
    let mut command = quote(exe.as_os_str());
    command.push(" --rustdesk-worker --pipe ");
    command.push(quote(OsStr::new(pipe)));
    command.push(format!(" --parent-pid {parent}"));
    command.encode_wide().chain(std::iter::once(0)).collect()
}
use crate::{business::BusinessHub, desk::engine::rpc, windows_worker::PipeEndpoint};
use std::{
    io, mem, ptr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;
use winapi::shared::{
    minwindef::{DWORD, FALSE, LPVOID},
    winerror::ERROR_NOT_ALL_ASSIGNED,
};
use winapi::um::{
    handleapi::CloseHandle,
    jobapi2::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject},
    minwinbase::STILL_ACTIVE,
    processthreadsapi::{
        CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken,
        ResumeThread, PROCESS_INFORMATION, STARTUPINFOW,
    },
    securitybaseapi::{AdjustTokenPrivileges, DuplicateTokenEx, SetTokenInformation},
    userenv::{CreateEnvironmentBlock, DestroyEnvironmentBlock},
    winbase::{
        LookupPrivilegeValueW, WTSGetActiveConsoleSessionId, CREATE_NO_WINDOW, CREATE_SUSPENDED,
        CREATE_UNICODE_ENVIRONMENT,
    },
    winnt::{
        JobObjectExtendedLimitInformation, SecurityImpersonation, TokenPrimary, TokenSessionId,
        HANDLE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        MAXIMUM_ALLOWED, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_DUPLICATE,
        TOKEN_PRIVILEGES, TOKEN_QUERY,
    },
};

// winapi 0.3 的 wtsapi32 只声明 WTSQueryUserToken；补齐 SDK 的枚举 ABI。
#[repr(C)]
struct WtsSession {
    session_id: DWORD,
    station: *mut u16,
    state: DWORD,
}
#[link(name = "Wtsapi32")]
extern "system" {
    fn WTSEnumerateSessionsW(
        server: HANDLE,
        reserved: DWORD,
        version: DWORD,
        entries: *mut *mut WtsSession,
        count: *mut DWORD,
    ) -> i32;
    fn WTSFreeMemory(memory: LPVOID);
}

struct Handle(usize);
impl Handle {
    fn raw(&self) -> HANDLE {
        self.0 as HANDLE
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.raw());
        }
    }
}
struct Worker {
    process: Handle,
    _job: Handle,
    session: u32,
}
impl Worker {
    fn alive(&self) -> bool {
        let mut code = 0;
        unsafe { GetExitCodeProcess(self.process.raw(), &mut code) != 0 && code == STILL_ACTIVE }
    }
}

fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

/// 优先活动 console，再选其他活动会话，最后保留有效 console 的登录/安全桌面。
/// Session 0 与系统报告的无 console 值永不作为桌面目标。
fn active_session() -> io::Result<Option<u32>> {
    unsafe {
        let console = Some(WTSGetActiveConsoleSessionId()).filter(|id| *id != 0 && *id != u32::MAX);
        let mut entries: *mut WtsSession = ptr::null_mut();
        let mut count = 0;
        if WTSEnumerateSessionsW(ptr::null_mut(), 0, 1, &mut entries, &mut count) == 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            if !entries.is_null() {
                WTSFreeMemory(entries as LPVOID);
            }
            return Ok(console);
        }
        let sessions = std::slice::from_raw_parts(entries, count as usize);
        let selected = console
            .filter(|id| {
                sessions
                    .iter()
                    .any(|session| session.session_id == *id && session.state == 0)
            })
            .or_else(|| {
                sessions
                    .iter()
                    .filter(|s| s.session_id != 0 && s.state == 0)
                    .map(|s| s.session_id)
                    .min()
            })
            .or(console);
        WTSFreeMemory(entries as LPVOID);
        Ok(selected)
    }
}

fn enable_launch_privileges() -> io::Result<()> {
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES,
            &mut token,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let token = Handle(token as usize);
        for name in [
            "SeTcbPrivilege",
            "SeAssignPrimaryTokenPrivilege",
            "SeIncreaseQuotaPrivilege",
        ] {
            let mut privileges: TOKEN_PRIVILEGES = mem::zeroed();
            privileges.PrivilegeCount = 1;
            if LookupPrivilegeValueW(
                ptr::null(),
                wide(name).as_ptr(),
                &mut privileges.Privileges[0].Luid,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            privileges.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
            if AdjustTokenPrivileges(
                token.raw(),
                FALSE,
                &mut privileges,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NOT_ALL_ASSIGNED as i32) {
                return Err(error);
            }
        }
        Ok(())
    }
}

fn spawn(session: u32, pipe: &str) -> io::Result<Worker> {
    enable_launch_privileges()?;
    unsafe {
        let mut source = ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &mut source,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let source = Handle(source as usize);
        let mut primary = ptr::null_mut();
        if DuplicateTokenEx(
            source.raw(),
            MAXIMUM_ALLOWED,
            ptr::null_mut(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let primary = Handle(primary as usize);
        let mut target_session = session;
        if SetTokenInformation(
            primary.raw(),
            TokenSessionId,
            &mut target_session as *mut _ as LPVOID,
            mem::size_of::<u32>() as DWORD,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let job = CreateJobObjectW(ptr::null_mut(), ptr::null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Handle(job as usize);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job.raw(),
            JobObjectExtendedLimitInformation,
            &mut limits as *mut _ as LPVOID,
            mem::size_of_val(&limits) as DWORD,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let exe = std::env::current_exe()?;
        // 管道名由 PipeEndpoint 生成，仅含安全的 ASCII；exe 参数通过统一 Windows 引号器。
        let mut command = worker_command(&exe, pipe, std::process::id());
        let mut startup: STARTUPINFOW = mem::zeroed();
        startup.cb = mem::size_of::<STARTUPINFOW>() as DWORD;
        let mut desktop = wide("winsta0\\default");
        startup.lpDesktop = desktop.as_mut_ptr();
        let mut info: PROCESS_INFORMATION = mem::zeroed();
        let directory = wide(
            exe.parent()
                .ok_or_else(|| io::Error::other("可执行文件缺少父目录"))?,
        );
        // 由 SYSTEM token 创建新的系统环境块，不把父进程任意环境变量带给桌面 worker。
        let mut environment = ptr::null_mut();
        if CreateEnvironmentBlock(&mut environment, primary.raw(), FALSE) == 0 {
            return Err(io::Error::last_os_error());
        }
        let created = CreateProcessAsUserW(
            primary.raw(),
            wide(&exe).as_ptr(),
            command.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            FALSE,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            environment,
            directory.as_ptr(),
            &mut startup,
            &mut info,
        );
        let error = io::Error::last_os_error();
        DestroyEnvironmentBlock(environment);
        if created == 0 {
            return Err(error);
        }
        let process = Handle(info.hProcess as usize);
        let thread = Handle(info.hThread as usize);
        if AssignProcessToJobObject(job.raw(), process.raw()) == 0 {
            // 挂起子进程未入 Job 时必须显式杀掉，不能留下孤儿。
            winapi::um::processthreadsapi::TerminateProcess(process.raw(), 1);
            return Err(io::Error::last_os_error());
        }
        if ResumeThread(thread.raw()) == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(Worker {
            process,
            _job: job,
            session,
        })
    }
}

async fn clear(business: &Arc<BusinessHub>, worker: &mut Option<Worker>) {
    // 先取消正在进行的 IPC，使 close_all 的输入释放/隐私查询有界失败；
    // 全部旧业务关闭之前绝不连接下一代 worker。
    rpc::set_transport(None);
    let business = business.clone();
    let retired = worker.take();
    if let Err(err) = tokio::task::spawn_blocking(move || {
        business.desk.close_all("desktop_session_changed");
        // Dropping the Job reaps the worker process tree.
        drop(retired);
    })
    .await
    {
        tracing::error!(%err, "desktop worker cleanup task failed");
    }
}

pub async fn supervise(
    business: Arc<BusinessHub>,
    mut stop: watch::Receiver<bool>,
    mut sessions: watch::Receiver<u32>,
) {
    let mut worker: Option<Worker> = None;
    let mut next_start = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        if *stop.borrow() {
            break;
        }
        let selected = match active_session() {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "读取交互会话失败");
                None
            }
        };
        let failed = worker
            .as_ref()
            .is_some_and(|child| !child.alive() || !rpc::worker_healthy());
        if worker
            .as_ref()
            .is_some_and(|child| Some(child.session) != selected || failed)
        {
            clear(&business, &mut worker).await;
            next_start = Instant::now()
                + if failed {
                    Duration::from_secs(5)
                } else {
                    Duration::ZERO
                };
        }
        if worker.is_none() && Instant::now() >= next_start {
            if let Some(session) = selected {
                let result = async {
                    let endpoint = PipeEndpoint::new()?;
                    let child = spawn(session, endpoint.name())?;
                    let pid = unsafe { winapi::um::processthreadsapi::GetProcessId(child.process.raw()) };
                    let connected = tokio::task::spawn_blocking(move || endpoint.accept(pid, Duration::from_secs(3)));
                    let transport = tokio::select! {
                        result = connected => result.map_err(|error| io::Error::other(error.to_string()))??,
                        _ = stop.changed() => return Err(io::Error::other("服务停止，取消桌面握手")),
                    };
                    if *stop.borrow() || active_session()? != Some(session) { return Err(io::Error::other("桌面会话已切换")); }
                    rpc::set_transport(Some(transport));
                    Ok::<Worker, io::Error>(child)
                }.await;
                match result {
                    Ok(child) => {
                        tracing::info!(session, "用户会话 RustDesk worker 已连接");
                        worker = Some(child);
                    }
                    Err(error) => {
                        clear(&business, &mut worker).await;
                        next_start = Instant::now() + Duration::from_secs(5);
                        tracing::warn!(%error, session, "桌面 worker 启动失败；HTTP 保持可用，稍后重试");
                    }
                }
            }
        }
        tokio::select! { _ = tick.tick() => {}, _ = sessions.changed() => {}, _ = stop.changed() => {} }
    }
    clear(&business, &mut worker).await;
}
