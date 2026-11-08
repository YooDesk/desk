//! 每个连接独立保存预测帧，显示器共享编码服务；浏览器真正解码后回传确认。
use crate::{Frame, YD_ERR_ARG, YD_ERR_EMPTY, YD_ERR_STATE, YD_OK};
use librustdesk::{
    input_service::MouseCursorSub, PublicGenericService, PublicService, PublicServiceTmpl,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{Duration, Instant},
};
type Packet = (Vec<u8>, bool, i64);
struct Cache {
    display: i32,
    frames: VecDeque<Packet>,
    cursors: VecDeque<Vec<u8>>,
    last_key: Option<Packet>,
    last_cursor: Option<Vec<u8>>,
    pending: Option<(i64, Instant)>,
    epoch: Instant,
    last_pts: i64,
    refresh_at: Instant,
    waiting_key: bool,
    closed: bool,
}
type Shared = Arc<(Mutex<Cache>, Condvar)>;
struct Driver {
    displays: HashMap<i32, PublicGenericService>,
    connections: HashMap<i32, Shared>,
    cursor: Option<PublicServiceTmpl<MouseCursorSub>>,
    position: Option<PublicGenericService>,
    cursor_recorder: Option<std::thread::JoinHandle<()>>,
    #[cfg(target_os = "linux")]
    layout: Option<PublicGenericService>,
}
static DRIVER: OnceLock<Mutex<Driver>> = OnceLock::new();
// 最后一名观众退出时要完成原生线程回收，再允许同名显示服务重建。
static LIFECYCLE: Mutex<()> = Mutex::new(());
fn driver() -> &'static Mutex<Driver> {
    DRIVER.get_or_init(|| {
        Mutex::new(Driver {
            displays: HashMap::new(),
            connections: HashMap::new(),
            cursor: None,
            position: None,
            cursor_recorder: None,
            #[cfg(target_os = "linux")]
            layout: None,
        })
    })
}
fn cache(conn: i32) -> Option<Shared> {
    driver().lock().unwrap().connections.get(&conn).cloned()
}

/// DRM已启用时几何来自已认证helper缓存，禁止先触发Display::all的Portal授权。
#[cfg(target_os = "linux")]
fn display_dimensions(display: i32) -> Result<(usize, usize), String> {
    #[cfg(target_os = "linux")]
    if let Some(displays) = crate::privileged::capture_geometry()? {
        let geometry = displays
            .get(usize::try_from(display).map_err(|_| "invalid display")?)
            .ok_or("DRM display not found")?;
        return Ok((geometry[0] as usize, geometry[1] as usize));
    }
    let displays = scrap::Display::all().map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    crate::privileged::refresh_user_input(&displays)?;
    let geometry = displays
        .get(usize::try_from(display).map_err(|_| "invalid display")?)
        .ok_or("display not found")?;
    Ok((geometry.width(), geometry.height()))
}

pub fn subscribe(display: i32, conn: i32, fps: u32, bitrate: u32) -> i32 {
    let _lifecycle = LIFECYCLE.lock().unwrap();
    if !(1..=60).contains(&fps) || !(200_000..=100_000_000).contains(&bitrate) {
        return YD_ERR_ARG;
    }
    #[cfg(target_os = "linux")]
    let (width, height) = match display_dimensions(display) {
        Ok(geometry) => geometry,
        Err(error) => {
            log::error!("display geometry: {error}");
            return YD_ERR_STATE;
        }
    };
    #[cfg(not(target_os = "linux"))]
    let (width, height) = {
        let Ok(displays) = scrap::Display::all() else {
            return YD_ERR_STATE;
        };
        let Some(geometry) = displays.get(display as usize) else {
            return YD_ERR_ARG;
        };
        (geometry.width(), geometry.height())
    };
    // 上游 FFmpeg 上下文按 30fps 分配每帧码率；按实际采集频率换算，才能兑现配置值。
    let target = bitrate as f32 / 1000.0 * 30.0 / fps as f32;
    let (mut low, mut high) = (0.2f32, 40.0f32);
    for _ in 0..16 {
        let mid = (low + high) * 0.5;
        let k = scrap::hwcodec::HwRamEncoder::bitrate("libx264", width, height, mid) as f32;
        if k < target {
            low = mid
        } else {
            high = mid
        }
    }
    {
        let mut qos = librustdesk::video_service::VIDEO_QOS.lock().unwrap();
        qos.on_connection_open(conn);
        qos.user_custom_fps(conn, fps);
        qos.user_auto_adjust_fps(conn, fps);
        qos.user_image_quality(conn, ((high * 50.0).round() as i32) << 8);
        qos.user_network_delay(conn, 0);
    }
    let shared = Arc::new((
        Mutex::new(Cache {
            display,
            frames: VecDeque::new(),
            cursors: VecDeque::new(),
            last_key: None,
            last_cursor: None,
            pending: None,
            epoch: Instant::now(),
            last_pts: -1,
            refresh_at: Instant::now(),
            waiting_key: true,
            closed: false,
        }),
        Condvar::new(),
    ));
    let mut state = driver().lock().unwrap();
    if state.connections.contains_key(&conn) {
        return YD_ERR_ARG;
    }
    if state.connections.is_empty() {
        state.cursor_recorder = librustdesk::input_service::try_start_record_cursor_pos();
    }
    let service = state
        .displays
        .entry(display)
        .or_insert_with(|| {
            librustdesk::video_service::new(
                librustdesk::video_service::VideoSource::Monitor,
                display as usize,
            )
        })
        .clone();
    let cursor = state
        .cursor
        .get_or_insert_with(librustdesk::input_service::new_cursor)
        .clone();
    let position = state
        .position
        .get_or_insert_with(librustdesk::input_service::new_pos)
        .clone();
    state.connections.insert(conn, shared.clone());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (tv, mut rv) = tokio::sync::mpsc::unbounded_channel();
    let sub = librustdesk::ConnInner::new(conn, Some(tx), Some(tv));
    // 复用上游显示服务持续维护Wayland布局/uinput范围，而不是自建刷新线程。
    #[cfg(target_os = "linux")]
    if std::env::var("YOODESK_ENGINE_USER_UINPUT").as_deref() == Ok("1") {
        let layout = state
            .layout
            .get_or_insert_with(librustdesk::display_service::new)
            .clone();
        PublicService::on_subscribe(&layout, sub.clone());
    }
    PublicService::on_subscribe(&service, sub.clone());
    PublicService::on_subscribe(&cursor, sub.clone());
    PublicService::on_subscribe(&position, sub);
    drop(state);
    crate::endpoint::subscribe(conn);
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        rt.block_on(async move {
            loop {
                let msg = tokio::select! {m=rx.recv()=>m,m=rv.recv()=>m};
                let Some((source_at, msg)) = msg else {
                    break;
                };
                let source_at = source_at.into_std();
                if let Some((bytes, shape)) = super::cursor_message(&msg) {
                    let mut c = shared.0.lock().unwrap();
                    if shape {
                        c.last_cursor = Some(bytes.clone());
                    }
                    if c.cursors.len() >= 8 {
                        c.cursors.pop_front();
                    }
                    c.cursors.push_back(bytes);
                    shared.1.notify_all();
                    continue;
                }
                if let Some((bytes, key, _)) = super::h264_from(&msg) {
                    let mut c = shared.0.lock().unwrap();
                    if c.closed {
                        break;
                    }
                    // refresh 之前已经排进原生通道的旧 key/delta 不能重新进入新预测链。
                    if source_at < c.refresh_at {
                        drop(c);
                        librustdesk::video_service::notify_video_frame_fetched(
                            display as usize,
                            conn,
                            None,
                        );
                        continue;
                    }
                    if c.waiting_key && !key {
                        drop(c);
                        librustdesk::video_service::notify_video_frame_fetched(
                            display as usize,
                            conn,
                            None,
                        );
                        continue;
                    }
                    if c.frames.len() >= 8 {
                        c.frames.clear();
                        c.waiting_key = true;
                        c.pending = None;
                        drop(c);
                        request_refresh(conn);
                        continue;
                    }
                    let pts = (c.epoch.elapsed().as_millis() as i64).max(c.last_pts + 1);
                    c.last_pts = pts;
                    if key {
                        c.waiting_key = false;
                        c.last_key = Some((bytes.clone(), key, pts));
                    }
                    c.pending = Some((pts, source_at));
                    c.frames.push_back((bytes, key, pts));
                    shared.1.notify_all();
                }
            }
        });
    });
    // X11 静止画面会持续 WouldBlock；上游仅在采到变化帧时处理 new_subscribes。
    // 业务层要先拿首帧才开放 WS，因此必须在原生订阅时主动重建，而不是等 WS 加入。
    request_refresh(conn)
}

pub fn ack(conn: i32, pts: i64) -> i32 {
    let Some(shared) = cache(conn) else {
        return YD_ERR_STATE;
    };
    let mut c = shared.0.lock().unwrap();
    let Some((expected, at)) = c.pending else {
        return YD_OK;
    };
    if expected != pts {
        return YD_OK;
    }
    c.pending = None;
    let display = c.display;
    let delay = at.elapsed().as_millis().min(u32::MAX as u128) as u32;
    drop(c);
    librustdesk::video_service::notify_video_frame_fetched(display as usize, conn, None);
    librustdesk::video_service::VIDEO_QOS
        .lock()
        .unwrap()
        .user_network_delay(conn, delay);
    YD_OK
}

pub fn request_refresh(conn: i32) -> i32 {
    let state = driver().lock().unwrap();
    let Some(current) = state.connections.get(&conn) else {
        return YD_ERR_STATE;
    };
    let display = current.0.lock().unwrap().display;
    let Some(service) = state.displays.get(&display) else {
        return YD_ERR_STATE;
    };
    let refresh_at = Instant::now();
    let mut affected = Vec::new();
    #[cfg(target_os = "linux")]
    let mut recently_healthy = false;
    // 重建该显示器编码器时，所有观众都从新 IDR 开始，旧 delta 不再混进来。
    for (&id, shared) in &state.connections {
        let mut c = shared.0.lock().unwrap();
        if c.display != display {
            continue;
        }
        #[cfg(target_os = "linux")]
        if c.last_pts >= 0
            && c.epoch
                .elapsed()
                .as_millis()
                .saturating_sub(c.last_pts as u128)
                < 1000
        {
            recently_healthy = true;
        }
        c.frames.clear();
        c.last_key = None;
        c.waiting_key = true;
        c.pending = None;
        c.refresh_at = refresh_at;
        affected.push(id);
    }
    #[cfg(target_os = "linux")]
    if recently_healthy && std::env::var("YOODESK_ENGINE_DRM_ENABLED").as_deref() == Ok("1") {
        // 仅已有观众一秒内实收有效帧时标记主动重建；不清除零帧/失败降级状态。
        librustdesk::yoodesk_drm_note_healthy_client_refresh(display as usize);
    }
    // 先清理所有观众，再唤醒编码器。反过来会把刚产生的新 IDR 误判成旧包而丢掉。
    service.set_option_bool(librustdesk::video_service::OPTION_REFRESH, true);
    for id in affected {
        librustdesk::video_service::notify_video_frame_fetched(display as usize, id, None);
    }
    YD_OK
}
pub fn replay(conn: i32) -> i32 {
    let Some(shared) = cache(conn) else {
        return YD_ERR_STATE;
    };
    let mut c = shared.0.lock().unwrap();
    if let Some(key) = c.last_key.clone() {
        if c.pending.map(|p| p.0) == Some(key.2) {
            c.frames.clear();
            c.frames.push_back(key);
            shared.1.notify_all();
            return YD_OK;
        }
    }
    drop(c);
    request_refresh(conn)
}
pub fn replay_cursor(conn: i32) -> i32 {
    let Some(shared) = cache(conn) else {
        return YD_ERR_STATE;
    };
    let mut c = shared.0.lock().unwrap();
    if let Some(shape) = c.last_cursor.clone() {
        if c.cursors.len() >= 8 {
            c.cursors.pop_back();
        }
        c.cursors.push_front(shape);
        shared.1.notify_all();
    }
    YD_OK
}
pub fn poll_cursor(conn: i32, data: *mut u8, cap: usize) -> i32 {
    if data.is_null() {
        return YD_ERR_ARG;
    }
    let Some(shared) = cache(conn) else {
        return YD_ERR_STATE;
    };
    let mut c = shared.0.lock().unwrap();
    let Some(bytes) = c.cursors.front() else {
        return YD_ERR_EMPTY;
    };
    if bytes.len() > cap {
        return YD_ERR_ARG;
    }
    let len = bytes.len();
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, len);
    }
    c.cursors.pop_front();
    len as i32
}
pub fn poll(conn: i32, data: *mut u8, cap: usize, frame: *mut Frame, timeout: u64) -> i32 {
    if data.is_null() || frame.is_null() {
        return YD_ERR_ARG;
    }
    let Some(shared) = cache(conn) else {
        return YD_ERR_STATE;
    };
    let deadline = Instant::now() + Duration::from_millis(timeout);
    let mut c = shared.0.lock().unwrap();
    loop {
        if let Some(p) = c.frames.front() {
            if p.0.len() > cap {
                return YD_ERR_ARG;
            }
            unsafe {
                std::ptr::copy_nonoverlapping(p.0.as_ptr(), data, p.0.len());
                *frame = Frame {
                    len: p.0.len(),
                    key: i32::from(p.1),
                    pts: p.2,
                };
            }
            c.frames.pop_front();
            return YD_OK;
        }
        if c.closed {
            return YD_ERR_STATE;
        }
        if Instant::now() >= deadline {
            return YD_ERR_EMPTY;
        }
        let (pair, _) = shared
            .1
            .wait_timeout(c, deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        c = pair;
    }
}
pub fn unsubscribe(conn: i32) -> i32 {
    let _lifecycle = LIFECYCLE.lock().unwrap();
    crate::endpoint::unsubscribe(conn);
    let mut state = driver().lock().unwrap();
    let Some(shared) = state.connections.remove(&conn) else {
        return YD_OK;
    };
    let mut c = shared.0.lock().unwrap();
    let display = c.display;
    #[cfg(target_os = "linux")]
    let recently_healthy = c.last_pts >= 0
        && c.epoch
            .elapsed()
            .as_millis()
            .saturating_sub(c.last_pts as u128)
            < 1000;
    c.closed = true;
    shared.1.notify_all();
    drop(c);
    if let Some(s) = state.displays.get(&display) {
        PublicService::on_unsubscribe(s, conn);
    }
    if let Some(s) = &state.cursor {
        PublicService::on_unsubscribe(s, conn);
    }
    if let Some(s) = &state.position {
        PublicService::on_unsubscribe(s, conn);
    }
    #[cfg(target_os = "linux")]
    if let Some(s) = &state.layout {
        PublicService::on_unsubscribe(s, conn);
    }
    let mut retired: Vec<Box<dyn PublicService>> = Vec::new();
    if !state
        .connections
        .values()
        .any(|s| s.0.lock().unwrap().display == display)
    {
        if let Some(service) = state.displays.remove(&display) {
            retired.push(Box::new(service));
        }
    }
    let mut recorder = None;
    if state.connections.is_empty() {
        librustdesk::input_service::try_stop_record_cursor_pos();
        recorder = state.cursor_recorder.take();
        if let Some(service) = state.cursor.take() {
            retired.push(Box::new(service));
        }
        if let Some(service) = state.position.take() {
            retired.push(Box::new(service));
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(service) = state.layout.take() {
                retired.push(Box::new(service));
            }
        }
    }
    drop(state);
    #[cfg(target_os = "linux")]
    if recently_healthy && std::env::var("YOODESK_ENGINE_DRM_ENABLED").as_deref() == Ok("1") {
        // 已实收新帧的正常退出也不是故障，否则无保留观众的重复连接同样误触发flapping。
        librustdesk::yoodesk_drm_note_healthy_client_refresh(display as usize);
    }
    // 上游按发送时的连接集合等待ACK；移除订阅不会自动释放该集合。
    // 关闭观众必须走其公开关闭通知，否则每次断连都会等待3秒流控期限，拖住其他观众。
    librustdesk::video_service::notify_video_frame_fetched_by_conn_id(conn, None);
    // 仅drop Arc并不会停止上游active循环，显式join避免每轮连接泄漏服务线程。
    // 已释放driver锁，接收线程可处理剩余消息；生命周期锁阻止新服务与旧清理竞态。
    for service in retired {
        service.join();
    }
    if let Some(recorder) = recorder {
        let _ = recorder.join();
    }
    librustdesk::video_service::VIDEO_QOS
        .lock()
        .unwrap()
        .on_connection_close(conn);
    YD_OK
}
pub fn close_all() -> i32 {
    let ids: Vec<_> = driver()
        .lock()
        .unwrap()
        .connections
        .keys()
        .copied()
        .collect();
    for id in ids {
        unsubscribe(id);
    }
    YD_OK
}
