//! Android 录屏到 Desk 会话的有界帧通道。
#![allow(unsafe_code)]

use super::input::{KeyInput, MouseInput};
use super::{
    Backend, CaptureMode, Capturer, DeskCapabilities, DisplayGeometry, InputSink, RgbaFrame,
};
use crate::json::{codes, ApiError};
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant, SystemTime},
};

const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
const CONSENT_TIMEOUT: Duration = Duration::from_secs(30);
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Default)]
struct State {
    epoch: u64,
    active: bool,
    sequence: u64,
    latest: Option<Arc<RgbaFrame>>,
    waiting: usize,
}

static STATE: Mutex<State> = Mutex::new(State {
    epoch: 0,
    active: false,
    sequence: 0,
    latest: None,
    waiting: 0,
});
static CHANGED: Condvar = Condvar::new();
static INPUT: Mutex<(bool, VecDeque<Vec<u8>>)> = Mutex::new((false, VecDeque::new()));

/// 无障碍服务状态变化；撤销时清除队列，防止重新授权后执行旧操作。
#[no_mangle]
pub extern "C" fn yd_android_input_enabled(enabled: bool) {
    let mut input = INPUT.lock().unwrap();
    input.0 = enabled;
    input.1.clear();
}

/// 从有界队列取一条输入事件。
///
/// # Safety
/// `out` 必须指向至少 `capacity` 字节的可写缓冲。
#[no_mangle]
pub unsafe extern "C" fn yd_android_input_read(out: *mut u8, capacity: usize) -> usize {
    if out.is_null() {
        return 0;
    }
    let mut input = INPUT.lock().unwrap();
    let Some(value) = input.1.pop_front() else {
        return 0;
    };
    if value.len() > capacity {
        return 0;
    }
    unsafe { std::ptr::copy_nonoverlapping(value.as_ptr(), out, value.len()) };
    value.len()
}

struct AndroidInput;

impl AndroidInput {
    fn push(&self, value: serde_json::Value) -> Result<(), ApiError> {
        let capture = STATE.lock().unwrap();
        if capture.latest.is_none() {
            return Err(unavailable());
        }
        let mut input = INPUT.lock().unwrap();
        if !input.0 {
            return Err(super::unsupported());
        }
        if input.1.len() >= 128 {
            return Err(ApiError::new(
                429,
                codes::CAPACITY_EXCEEDED,
                "Android input queue is full",
            ));
        }
        let bytes = value.to_string().into_bytes();
        if bytes.len() > 8192 {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "Android input event is too large",
            ));
        }
        input.1.push_back(bytes);
        Ok(())
    }
}

impl InputSink for AndroidInput {
    fn mouse(&mut self, event: MouseInput) -> Result<(), ApiError> {
        self.push(serde_json::json!({"mouse":event}))
    }
    fn key(&mut self, event: KeyInput) -> Result<(), ApiError> {
        self.push(serde_json::json!({"key":event}))
    }
    fn release_all(&mut self) {
        let mut input = INPUT.lock().unwrap();
        input.1.clear();
        if input.0 {
            input.1.push_back(b"{\"reset\":true}".to_vec());
        }
    }
}

/// 开始新一代录屏，旧订阅及未消费帧立即失效。
#[no_mangle]
pub extern "C" fn yd_android_capture_start() -> u64 {
    let mut state = STATE.lock().unwrap();
    state.epoch = state.epoch.checked_add(1).expect("capture epoch exhausted");
    state.active = true;
    state.waiting = 0;
    state.latest = None;
    INPUT.lock().unwrap().1.clear();
    CHANGED.notify_all();
    state.epoch
}

/// 等待系统授权的请求代次。没有已授权的 Desk 会话请求时返回零。
#[no_mangle]
pub extern "C" fn yd_android_capture_pending() -> u64 {
    let state = STATE.lock().unwrap();
    if !state.active && state.waiting > 0 {
        state.epoch + 1
    } else {
        0
    }
}

/// 系统拒绝或关闭授权框时，使这一批等待的观看者退出。
#[no_mangle]
pub extern "C" fn yd_android_capture_deny(epoch: u64) {
    let mut state = STATE.lock().unwrap();
    if !state.active && state.waiting > 0 && state.epoch + 1 == epoch {
        state.epoch = epoch;
        state.waiting = 0;
        CHANGED.notify_all();
    }
}

/// 当前录屏授权是否已经交付首帧。
#[no_mangle]
pub extern "C" fn yd_android_capture_ready() -> bool {
    STATE.lock().unwrap().latest.is_some()
}

/// 停止指定代录屏；迟到的系统回调不影响新一代。
#[no_mangle]
pub extern "C" fn yd_android_capture_stop(epoch: u64) {
    let mut state = STATE.lock().unwrap();
    if state.active && state.epoch == epoch {
        state.active = false;
        state.latest = None;
        let mut input = INPUT.lock().unwrap();
        input.1.clear();
        if input.0 {
            input.1.push_back(b"{\"reset\":true}".to_vec());
        }
        CHANGED.notify_all();
    }
}

/// 接收 RustDesk 采集桥交来的 RGBA。只在调用期间读取缓冲，发布时拥有副本。
///
/// # Safety
/// `data` 必须在调用期间指向至少 `len` 字节的稳定可读内存。
#[no_mangle]
pub unsafe extern "C" fn yd_android_capture_frame(
    epoch: u64,
    width: u32,
    height: u32,
    stride: usize,
    data: *const u8,
    len: usize,
) -> i32 {
    let Some(row) = (width as usize).checked_mul(4) else {
        return -1;
    };
    let Some(span) = (height as usize)
        .checked_sub(1)
        .and_then(|rows| rows.checked_mul(stride))
        .and_then(|bytes| bytes.checked_add(row))
    else {
        return -1;
    };
    if width < 2
        || height < 2
        || stride < row
        || span > len
        || span > MAX_FRAME_BYTES
        || data.is_null()
    {
        return -1;
    }
    // YUV420 要求偶数宽高；应用窗口可能是奇数像素，只裁掉末行或末列。
    let (width, height) = (width & !1, height & !1);
    let row = width as usize * 4;
    let mut state = STATE.lock().unwrap();
    if !state.active || state.epoch != epoch {
        return 0;
    }
    let raw = unsafe { std::slice::from_raw_parts(data, span) };
    let mut pixels = Vec::with_capacity(row * height as usize);
    for line in raw.chunks(stride).take(height as usize) {
        for pixel in line[..row].chunks_exact(4) {
            pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
    state.sequence += 1;
    state.latest = Some(Arc::new(RgbaFrame {
        display: 0,
        width,
        height,
        data: pixels,
        captured_at: SystemTime::now(),
    }));
    CHANGED.notify_all();
    1
}

pub(super) struct AndroidBackend;

fn geometry(frame: &RgbaFrame) -> DisplayGeometry {
    DisplayGeometry {
        index: 0,
        x: 0,
        y: 0,
        width: frame.width,
        height: frame.height,
        name: "Android".into(),
        primary: true,
    }
}

fn unavailable() -> ApiError {
    ApiError::new(
        503,
        codes::UNSUPPORTED_FEATURE,
        "Android screen capture is not ready",
    )
}

impl Backend for AndroidBackend {
    fn accepts_capture_request(&self) -> bool {
        self.supports_h264()
    }

    fn accepts_display(&self, display: u32) -> Result<bool, ApiError> {
        Ok(display == 0)
    }
    fn name(&self) -> &'static str {
        "rustdesk-android"
    }

    fn capabilities(&self) -> DeskCapabilities {
        let mut capabilities = DeskCapabilities::none();
        let ready = STATE.lock().unwrap().latest.is_some();
        capabilities.capture = ready && self.supports_h264();
        capabilities.displays = ready;
        capabilities.h264 = capabilities.capture;
        capabilities.pointer = self.supports_input();
        capabilities.keyboard = capabilities.pointer;
        capabilities
    }

    fn displays(&self) -> Result<Vec<DisplayGeometry>, ApiError> {
        Ok(STATE
            .lock()
            .unwrap()
            .latest
            .as_deref()
            .map(|frame| vec![geometry(frame)])
            .unwrap_or_default())
    }

    fn create_capturer(&self, display: u32) -> Result<Box<dyn Capturer>, ApiError> {
        if display != 0 {
            return Err(super::unsupported());
        }
        let mut state = STATE.lock().unwrap();
        let waiting = !state.active;
        let epoch = if waiting {
            state.waiting += 1;
            state.epoch + 1
        } else {
            state.epoch
        };
        let geometry = state
            .latest
            .as_deref()
            .map(geometry)
            .unwrap_or(DisplayGeometry {
                index: 0,
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                name: "Android".into(),
                primary: true,
            });
        Ok(Box::new(AndroidCapturer {
            geometry,
            epoch,
            sequence: None,
            consent_deadline: waiting.then(|| Instant::now() + CONSENT_TIMEOUT),
        }))
    }

    fn create_input(&self) -> Result<Box<dyn InputSink>, ApiError> {
        if self.supports_input() {
            Ok(Box::new(AndroidInput))
        } else {
            Err(super::unsupported())
        }
    }
    fn supports_input(&self) -> bool {
        INPUT.lock().unwrap().0
    }
    fn supports_h264(&self) -> bool {
        super::encode::h264_available()
    }
}

struct AndroidCapturer {
    geometry: DisplayGeometry,
    epoch: u64,
    sequence: Option<u64>,
    consent_deadline: Option<Instant>,
}

impl AndroidCapturer {
    fn read(&mut self, timeout: Duration, snapshot: bool) -> io::Result<RgbaFrame> {
        let (state, _) = CHANGED
            .wait_timeout_while(STATE.lock().unwrap(), timeout, |state| {
                self.waiting_for_consent(state)
                    || (state.active
                        && state.epoch == self.epoch
                        && (state.latest.is_none()
                            || (!snapshot && self.sequence == Some(state.sequence))))
            })
            .unwrap();
        if self.waiting_for_consent(&state) {
            return Err(if self
                .consent_deadline
                .is_some_and(|end| Instant::now() >= end)
            {
                io::ErrorKind::TimedOut
            } else {
                io::ErrorKind::WouldBlock
            }
            .into());
        }
        if !state.active || state.epoch != self.epoch {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let frame = state
            .latest
            .as_ref()
            .filter(|_| snapshot || self.sequence != Some(state.sequence))
            .ok_or(io::ErrorKind::WouldBlock)?
            .clone();
        self.sequence = Some(state.sequence);
        self.geometry = geometry(&frame);
        drop(state);
        Ok((*frame).clone())
    }

    fn waiting_for_consent(&self, state: &State) -> bool {
        self.consent_deadline.is_some() && !state.active && state.epoch + 1 == self.epoch
    }
}

impl Drop for AndroidCapturer {
    fn drop(&mut self) {
        let mut state = STATE.lock().unwrap();
        if self.waiting_for_consent(&state) {
            state.waiting -= 1;
            if state.waiting == 0 {
                state.epoch = self.epoch;
                CHANGED.notify_all();
            }
        }
    }
}

impl Capturer for AndroidCapturer {
    fn first_frame_timeout(&self) -> Option<Duration> {
        Some(FIRST_FRAME_TIMEOUT)
    }

    fn geometry(&self) -> &DisplayGeometry {
        &self.geometry
    }
    fn next_frame(&mut self, timeout: Duration) -> io::Result<RgbaFrame> {
        self.read(timeout, false)
    }
    fn snapshot(&mut self) -> io::Result<RgbaFrame> {
        self.read(Duration::ZERO, true)
    }
    fn mode(&self) -> CaptureMode {
        CaptureMode::Native
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn padded_frames_reach_both_viewers_and_old_projection_cannot_publish() {
        let old = yd_android_capture_start();
        let mut rgba = [99; 44];
        rgba[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        rgba[16..24].copy_from_slice(&[9, 10, 11, 12, 13, 14, 15, 16]);
        let publish =
            |epoch| unsafe { yd_android_capture_frame(epoch, 3, 3, 16, rgba.as_ptr(), rgba.len()) };
        assert!(!AndroidBackend.capabilities().capture);
        assert_eq!(publish(old), 1);
        assert!(AndroidBackend.capabilities().capture);
        let mut first = AndroidBackend.create_capturer(0).unwrap();
        let mut second = AndroidBackend.create_capturer(0).unwrap();
        for viewer in [&mut first, &mut second] {
            let frame = viewer.next_frame(Duration::ZERO).unwrap();
            assert_eq!((frame.width, frame.height), (2, 2));
            assert_eq!(
                frame.data,
                [3, 2, 1, 4, 7, 6, 5, 8, 11, 10, 9, 12, 15, 14, 13, 16]
            );
            assert_eq!(
                viewer.next_frame(Duration::ZERO).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            assert!(viewer.snapshot().is_ok());
        }
        let new = yd_android_capture_start();
        yd_android_capture_stop(old);
        assert_eq!(publish(old), 0);
        assert!(AndroidBackend.displays().unwrap().is_empty());
        assert!(!AndroidBackend.capabilities().capture);
        assert_eq!(publish(new), 1);
        assert_eq!(
            first.snapshot().unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        yd_android_capture_stop(new);
        assert!(AndroidBackend.displays().unwrap().is_empty());
        assert!(!AndroidBackend.capabilities().capture);
        assert!(AndroidBackend.accepts_capture_request());
        let mut denied = AndroidBackend.create_capturer(0).unwrap();
        let request = yd_android_capture_pending();
        assert_ne!(request, 0);
        assert_eq!(
            denied.snapshot().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        yd_android_capture_deny(request);
        assert_eq!(
            denied.snapshot().unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        let mut granted = AndroidBackend.create_capturer(0).unwrap();
        let request = yd_android_capture_pending();
        assert_eq!(yd_android_capture_start(), request);
        assert_eq!(publish(request), 1);
        assert!(granted.snapshot().is_ok());
        assert!(!AndroidBackend.supports_input());
        yd_android_input_enabled(true);
        let mut input = AndroidBackend.create_input().unwrap();
        input.mouse(MouseInput::Move { x: 1, y: 1 }).unwrap();
        let mut buffer = [0; 128];
        let size = unsafe { yd_android_input_read(buffer.as_mut_ptr(), buffer.len()) };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&buffer[..size]).unwrap(),
            serde_json::json!({"mouse":{"Move":{"x":1,"y":1}}})
        );
        input.key(KeyInput::Text("fixture".into())).unwrap();
        yd_android_capture_stop(request);
        let size = unsafe { yd_android_input_read(buffer.as_mut_ptr(), buffer.len()) };
        assert_eq!(&buffer[..size], b"{\"reset\":true}");
        assert!(input.key(KeyInput::Text("late".into())).is_err());
        yd_android_input_enabled(false);

        // 真正的会话管理器必须能在无录屏时创建会话，并在许可后发出可解码的关键帧。
        use crate::access::{AccessContext, AccessMode, LocalPrincipal};
        use crate::business::desk_session::{CreateOptions, DeskSessionManager};
        use protobuf::Message as _;
        let manager = DeskSessionManager::new(Arc::new(AndroidBackend));
        let now = SystemTime::now();
        let access = AccessContext {
            session_id: 7,
            principal: LocalPrincipal {
                principal_id: "fixture".into(),
                scopes: vec!["session.connect".into(), "screen.view".into()],
            },
            mode: AccessMode::Cookie,
            csrf: "fixture".into(),
            created_at: now,
            expires_at: now + Duration::from_secs(60),
            absolute_expires_at: now + Duration::from_secs(60),
        };
        let session = manager
            .create(&access, "local", &CreateOptions::default())
            .unwrap();
        assert!(!session.info().stream_available);
        let pending = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let pending = yd_android_capture_pending();
                if pending != 0 {
                    break pending;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(yd_android_capture_start(), pending);
        let mut picture = [0; 64 * 64 * 4];
        for (index, rgba) in picture.chunks_exact_mut(4).enumerate() {
            rgba.copy_from_slice(if index % 64 < 32 {
                &[200, 32, 20, 255]
            } else {
                &[20, 32, 200, 255]
            });
        }
        assert_eq!(
            unsafe {
                yd_android_capture_frame(pending, 64, 64, 256, picture.as_ptr(), picture.len())
            },
            1
        );
        let mut stream = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if session.info().stream_available {
                    break session.subscribe_frames().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let packet = tokio::time::timeout(Duration::from_secs(3), stream.recv())
            .await
            .unwrap()
            .unwrap();
        let message = crate::proto::Message::parse_from_bytes(&packet).unwrap();
        let frame = &message.video_frame().h264s().frames[0];
        assert!(frame.key);
        assert!(!frame.data.is_empty());
        use openh264::formats::YUVSource as _;
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        let decoded = decoder.decode(&frame.data).unwrap().unwrap();
        assert_eq!(decoded.dimensions(), (64, 64));
        let mut rgb = [0; 64 * 64 * 3];
        decoded.write_rgb8(&mut rgb);
        let left = (32 * 64 + 16) * 3;
        let right = (32 * 64 + 48) * 3;
        assert!(rgb[left] > 150 && rgb[left + 2] < 80);
        assert!(rgb[right + 2] > 150 && rgb[right] < 80);
        yd_android_capture_stop(pending);
        session.close("fixture_done");
    }
}
