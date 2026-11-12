//! desk 会话：采集 → 编码 → `hbb.Message` 帧广播，以及输入注入。
//!
//! 状态机（与设计一致）：
//! `created` → `connecting` → `transport_ready` → `active` → `closing` → `closed`
//! 采集/编码初始化失败进入 `failed` 终态；关闭是幂等的。
//!
//! 一个会话独占一块显示器；同一显示器允许多个只读观察者，但输入只发给会话持有者的
//! `InputSink`。帧通过有界 broadcast 分发，慢消费者只会丢帧，不会拖慢采集线程。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::sync::broadcast;

use super::events::{
    initial_snapshot, ClosedPayload, EventBus, EventEnvelope, EventKind, SnapshotSource,
};
use super::model::{SessionInfo, SessionState, SessionStateKind, TargetAuthentication};
use crate::access::{AccessContext, Clock};
use crate::desk::encode::{EncodedFrame, EncoderSettings, H264Encoder};
use crate::desk::{Backend, InputSink};
use crate::json::{codes, ApiError};

/// 单会话帧通道容量：只保留最新两帧，积压时丢弃旧帧。
pub const FRAME_CHANNEL_CAPACITY: usize = 2;

/// 光标通道容量：光标变化不需要保留历史，多出的直接丢弃。
pub const CURSOR_CHANNEL_CAPACITY: usize = 4;
/// 音频订阅与已知格式快照：新观众必须先收到格式。
pub type AudioSubscription = (broadcast::Receiver<Arc<Vec<u8>>>, Option<Arc<Vec<u8>>>);

/// 采集轮询超时；超时表示画面无变化，不产生帧。
const CAPTURE_TIMEOUT: Duration = Duration::from_millis(120);
const STREAM_ATTACH_TIMEOUT: Duration = Duration::from_secs(30);

/// 默认会话上限：本机单用户场景，限制并发采集占用。
pub const DEFAULT_MAX_SESSIONS: usize = 4;

/// 会话所有权：只允许创建它的本地访问会话访问。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Owner {
    access_session_id: u64,
    principal_id: String,
}

/// 会话运行数据。
#[derive(Debug)]
struct Runtime {
    state: SessionStateKind,
    revision: u64,
    stream_available: bool,
    target_authentication: TargetAuthentication,
    closed_reason: Option<String>,
}

impl Runtime {
    fn new() -> Self {
        Self {
            state: SessionStateKind::Created,
            revision: 1,
            stream_available: false,
            target_authentication: TargetAuthentication::Unknown,
            closed_reason: None,
        }
    }

    fn transition(&mut self, state: SessionStateKind, stream_available: bool) -> u64 {
        self.state = state;
        self.stream_available = stream_available;
        self.revision += 1;
        self.revision
    }
}

/// desk 会话。
pub struct DeskSession {
    id: uuid::Uuid,
    owner: Owner,
    product_id: String,
    target_id: String,
    display: u32,
    read_only: bool,
    chat: Arc<super::chat::ChatRoom>,
    runtime: Mutex<Runtime>,
    events: EventBus,
    /// 已编码的 `hbb.Message` 字节。
    frames: broadcast::Sender<Arc<Vec<u8>>>,
    /// 光标消息（`hbb.Message.cursor_data`/`cursor_position`），与帧共用同一 WS。
    cursors: broadcast::Sender<Arc<Vec<u8>>>,
    /// 请求采集线程发送一次关键帧快照（新观众需要可解码的起始帧）。
    keyframe_requests: std::sync::mpsc::SyncSender<()>,
    /// 请求采集线程重发当前光标形状（新观众需要可绘制内容）。
    cursor_requests: std::sync::mpsc::SyncSender<()>,
    input: Mutex<Option<Box<dyn InputSink>>>,
    stop: Arc<AtomicBool>,
    closed: tokio::sync::watch::Sender<bool>,
    /// 采集线程是否已结束（用于关闭时等待资源回收）。
    worker_done: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    created_at: SystemTime,
    access_expires_at: Mutex<SystemTime>,
    /// 创建时固定的后端能力；会话存续期间不变。
    capabilities: crate::desk::DeskCapabilities,
    /// 剪贴板访问；`None` 表示后端不支持。
    clipboard: Mutex<Option<Box<dyn crate::desk::Clipboard>>>,
    /// 最近一次**从系统读到**的剪贴板内容，用于避免重复上报。
    ///
    /// 只由 `clipboard_read` 更新：写入是控制端→被控端方向，不代表远端用户复制了内容，
    /// 因此写入不能置位这里，否则紧接着的读取会被误判为“未变化”。
    last_clipboard: Mutex<Option<crate::desk::ClipboardContent>>,
    audio: broadcast::Sender<Arc<Vec<u8>>>,
    audio_format: Mutex<Option<Arc<Vec<u8>>>>,
    #[cfg(feature = "desk-engine")]
    engine_conn: i32,
    endpoint_lock: Mutex<()>,
    #[cfg(feature = "desk-engine")]
    media_lock: Mutex<()>,
}

impl std::fmt::Debug for DeskSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeskSession")
            .field("id", &self.id)
            .field("target_id", &self.target_id)
            .field("display", &self.display)
            .finish()
    }
}

/// 会话操作需要的路径前缀信息，用于生成 event/stream URL。
#[derive(Debug, Clone)]
pub struct UrlBase {
    /// 产品前缀，例如 `/yoodesk`。
    pub product_id: String,
}

impl UrlBase {
    fn session_url(&self, id: uuid::Uuid, suffix: &str) -> String {
        format!(
            "/{}/webclient/desk/api/v2/sessions/{}/{}",
            self.product_id, id, suffix
        )
    }
}

impl SnapshotSource for DeskSession {
    fn snapshot(&self) -> Option<SessionInfo> {
        let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        if runtime.state == SessionStateKind::Closed {
            // 已关闭的会话仍可读取快照（幂等 GET），但 stream 不可用。
        }
        let base = UrlBase {
            product_id: self.product_id.clone(),
        };
        Some(self.build_info(&runtime, &base))
    }

    fn state(&self) -> Option<(SessionStateKind, u64, bool, TargetAuthentication)> {
        let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        Some((
            runtime.state,
            runtime.revision,
            runtime.stream_available,
            runtime.target_authentication,
        ))
    }
}

impl DeskSession {
    fn build_info(&self, runtime: &Runtime, base: &UrlBase) -> SessionInfo {
        let stream_available = runtime.stream_available
            && matches!(
                runtime.state,
                SessionStateKind::TransportReady
                    | SessionStateKind::Active
                    | SessionStateKind::Authenticating
                    | SessionStateKind::WaitingChallenge
            );
        SessionInfo {
            business_session_id: self.id,
            capability: "desk",
            target_id: self.target_id.clone(),
            state: runtime.state,
            state_revision: runtime.revision.to_string(),
            stream_available,
            target_authentication: runtime.target_authentication,
            stream_profile: crate::proto::PROFILE,
            features: self.features(),
            event_url: base.session_url(self.id, "events/"),
            stream_url: if stream_available {
                Some(base.session_url(self.id, "stream/"))
            } else {
                None
            },
            access_mode: "local",
            access_expires_at: *self
                .access_expires_at
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        }
    }

    /// 会话实际可用的 feature：平台能力与只读限制取交集。
    fn features(&self) -> Vec<String> {
        self.capabilities
            .features()
            .into_iter()
            .filter(|feature| {
                if self.read_only {
                    // 只读会话不提供输入与剪贴板写入。
                    feature != "desk.input.pointer"
                        && feature != "desk.input.keyboard"
                        && feature != "desk.clipboard.write"
                        && feature != "desk.privacy.screen"
                } else {
                    true
                }
            })
            .collect()
    }

    /// 会话能力（后端能力与只读限制取交集后的结果）。
    pub fn capabilities(&self) -> crate::desk::DeskCapabilities {
        let mut capabilities = self.capabilities.clone();
        if self.read_only {
            capabilities.pointer = false;
            capabilities.keyboard = false;
            capabilities.clipboard_write = false;
            capabilities.privacy_screen = false;
        }
        capabilities
    }

    /// 当前光标通道的订阅者数量（诊断用）。
    pub fn cursor_receiver_count(&self) -> usize {
        self.cursors.receiver_count()
    }

    /// 订阅光标/位置消息。
    ///
    /// 形状只在变化时产生，且可能在订阅之前就已发出（广播不保留历史）；
    /// 因此订阅时主动请求重发一次当前形状，否则新观众永远没有可绘制内容。
    pub fn subscribe_cursors(&self) -> broadcast::Receiver<Arc<Vec<u8>>> {
        let receiver = self.cursors.subscribe();
        let _ = self.cursor_requests.try_send(());
        receiver
    }

    /// 读取目标剪贴板；内容未变化返回 `Ok(None)`。
    pub fn clipboard_read(&self) -> Result<Option<crate::desk::ClipboardContent>, ApiError> {
        self.require_active_endpoint()?;
        let mut guard = self.clipboard.lock().unwrap_or_else(|e| e.into_inner());
        self.require_active_endpoint()?;
        let reader = guard.as_mut().ok_or_else(|| {
            ApiError::new(
                501,
                codes::UNSUPPORTED_FEATURE,
                "clipboard is not available",
            )
        })?;
        let content = reader.read()?;
        if let Some(content) = &content {
            let mut last = self
                .last_clipboard
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if last.as_ref() == Some(content) {
                return Ok(None);
            }
            *last = Some(content.clone());
        }
        Ok(content)
    }

    /// 写入目标剪贴板；只读会话明确拒绝。
    pub fn clipboard_write(&self, content: &crate::desk::ClipboardContent) -> Result<(), ApiError> {
        self.require_active_endpoint()?;
        if self.read_only {
            return Err(ApiError::new(403, codes::FORBIDDEN, "session is read-only"));
        }
        let mut guard = self.clipboard.lock().unwrap_or_else(|e| e.into_inner());
        self.require_active_endpoint()?;
        let writer = guard.as_mut().ok_or_else(|| {
            ApiError::new(
                501,
                codes::UNSUPPORTED_FEATURE,
                "clipboard is not available",
            )
        })?;
        writer.write(content)?;
        Ok(())
    }

    /// 扩展操作只允许访问存续且未到期的会话。
    fn require_active_endpoint(&self) -> Result<(), ApiError> {
        if self.stop.load(Ordering::SeqCst)
            || SystemTime::now()
                >= *self
                    .access_expires_at
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
        {
            return Err(ApiError::new(
                409,
                codes::STATE_CONFLICT,
                "session is closed or expired",
            ));
        }
        Ok(())
    }

    /// 订阅 Opus 数据并返回格式快照。
    pub fn subscribe_audio(&self) -> AudioSubscription {
        let rx = self.audio.subscribe();
        let format = self
            .audio_format
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        (rx, format)
    }

    /// 会话拥有扩展操作；关闭与写入串行，不能在收尾后重新打开隐私屏。
    pub fn endpoint(&self, value: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
        self.require_active_endpoint()?;
        let _guard = self.endpoint_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.require_active_endpoint()?;
        let action = value["action"].as_str().unwrap_or("");
        if action == "input.release" {
            if let Some(sink) = self
                .input
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_mut()
            {
                sink.release_all();
            }
            return Ok(serde_json::json!({"released": true}));
        }
        if action.starts_with("chat.") {
            return self.chat.endpoint(value, self.read_only);
        }
        #[cfg(feature = "desk-engine")]
        {
            if self.read_only
                && matches!(
                    action,
                    "files.upload"
                        | "files.write"
                        | "files.commit"
                        | "files.create"
                        | "files.delete"
                        | "files.rename"
                        | "privacy.set"
                )
            {
                return Err(ApiError::new(403, codes::FORBIDDEN, "session is read-only"));
            }
            crate::desk::engine::EngineBackend.endpoint(self.engine_conn, value)
        }
        #[cfg(not(feature = "desk-engine"))]
        {
            let _ = value;
            Err(ApiError::new(
                501,
                codes::UNSUPPORTED_FEATURE,
                "endpoint extensions require desk-engine",
            ))
        }
    }

    /// 流控绑定本会话，和关闭串行；只读观众也必须能够确认视频。
    pub fn ack_video(&self, pts: i64) -> Result<(), ApiError> {
        self.require_active_endpoint()?;
        #[cfg(feature = "desk-engine")]
        {
            let _guard = self.media_lock.lock().unwrap_or_else(|e| e.into_inner());
            self.require_active_endpoint()?;
            crate::desk::engine::EngineBackend.ack_video(self.engine_conn, pts)?;
        }
        #[cfg(not(feature = "desk-engine"))]
        let _ = pts;
        Ok(())
    }

    /// 参考帧链中断时请求完整 IDR，而不是继续播放损坏的 delta。
    pub fn refresh_video(&self) -> Result<(), ApiError> {
        self.require_active_endpoint()?;
        #[cfg(feature = "desk-engine")]
        {
            let _guard = self.media_lock.lock().unwrap_or_else(|e| e.into_inner());
            self.require_active_endpoint()?;
            crate::desk::engine::EngineBackend.refresh_video(self.engine_conn)?;
        }
        #[cfg(not(feature = "desk-engine"))]
        let _ = self.keyframe_requests.try_send(());
        Ok(())
    }

    /// 会话 id。
    pub fn id(&self) -> uuid::Uuid {
        self.id
    }

    /// 会话创建时刻。
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// 当前快照（对外入口）。
    pub fn info(&self) -> SessionInfo {
        let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        let base = UrlBase {
            product_id: self.product_id.clone(),
        };
        self.build_info(&runtime, &base)
    }

    /// 订阅事件：先发首包快照，之后接收状态事件。
    pub fn subscribe_events(
        &self,
    ) -> Result<(EventEnvelope, broadcast::Receiver<Arc<EventEnvelope>>), ApiError> {
        let (snapshot, _revision) = initial_snapshot(self)?;
        let envelope = EventEnvelope::new(self.id, 1, EventKind::Snapshot { payload: snapshot });
        Ok((envelope, self.events.subscribe()))
    }

    /// 订阅帧；返回接收者与当前是否可流。
    pub fn subscribe_frames(&self) -> Result<broadcast::Receiver<Arc<Vec<u8>>>, ApiError> {
        let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        if !runtime.stream_available {
            return Err(ApiError::new(
                409,
                codes::STATE_CONFLICT,
                "stream is not available",
            ));
        }
        let receiver = self.frames.subscribe();
        // 与最后订阅者退出后的关闭判断共用runtime锁，避免检查后新订阅被误关。
        drop(runtime);
        // 静态桌面不会产生增量更新，新观众必须收到一次 IDR 快照才能开画。
        // 请求是有界通道，满了说明采集线程已经在处理，直接复用其结果。
        let _ = self.keyframe_requests.try_send(());
        Ok(receiver)
    }

    /// 最后一条画面流退出后释放采集/输入；events连接不延长业务生命周期。
    pub fn close_if_no_streams(&self) {
        self.close_when("stream_disconnected", true);
    }

    /// Wait for a terminal session, including one closed before subscription.
    pub async fn closed(&self) {
        let mut closed = self.closed.subscribe();
        let _ = closed.wait_for(|value| *value).await;
    }

    fn expire_unattached_stream(&self, deadline: &mut Option<std::time::Instant>) {
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            *deadline = None;
            self.close_when("stream_attach_timeout", true);
        }
    }

    /// 注入鼠标输入；只读会话明确拒绝。
    pub fn mouse(&self, input: crate::desk::input::MouseInput) -> Result<(), ApiError> {
        if self.read_only {
            return Err(ApiError::new(403, codes::FORBIDDEN, "session is read-only"));
        }
        let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        if runtime.state != SessionStateKind::Active
            && runtime.state != SessionStateKind::TransportReady
        {
            return Err(ApiError::new(
                409,
                codes::STATE_CONFLICT,
                "session is not active",
            ));
        }
        drop(runtime);
        let mut guard = self.input.lock().unwrap_or_else(|e| e.into_inner());
        self.require_active_endpoint()?;
        match guard.as_mut() {
            Some(sink) => sink.mouse(input),
            None => Err(ApiError::new(
                409,
                codes::STATE_CONFLICT,
                "input not available",
            )),
        }
    }

    /// 注入键盘输入。
    pub fn key(&self, input: crate::desk::input::KeyInput) -> Result<(), ApiError> {
        if self.read_only {
            return Err(ApiError::new(403, codes::FORBIDDEN, "session is read-only"));
        }
        let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        if runtime.state != SessionStateKind::Active
            && runtime.state != SessionStateKind::TransportReady
        {
            return Err(ApiError::new(
                409,
                codes::STATE_CONFLICT,
                "session is not active",
            ));
        }
        drop(runtime);
        let mut guard = self.input.lock().unwrap_or_else(|e| e.into_inner());
        self.require_active_endpoint()?;
        match guard.as_mut() {
            Some(sink) => sink.key(input),
            None => Err(ApiError::new(
                409,
                codes::STATE_CONFLICT,
                "input not available",
            )),
        }
    }

    /// 空闲续期后同步访问截止时间。
    pub fn update_access_expiry(&self, expires_at: SystemTime, lease_version: u64) {
        *self
            .access_expires_at
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = expires_at;
        let revision = {
            let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            runtime.revision += 1;
            runtime.revision
        };
        self.events.publish(EventEnvelope::new(
            self.id,
            revision,
            EventKind::AccessRenewed {
                payload: super::events::RenewedPayload {
                    lease_version: lease_version.to_string(),
                    expires_at,
                    state_revision: revision.to_string(),
                },
            },
        ));
    }

    /// 关闭会话：幂等；停止采集、释放输入、广播关闭事件。
    pub fn close(&self, reason: &str) {
        self.close_when(reason, false);
    }

    fn close_when(&self, reason: &str, only_without_streams: bool) {
        let (changed, revision) = {
            let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            if only_without_streams && self.frames.receiver_count() != 0 {
                return;
            }
            if runtime.state.is_terminal() {
                (false, runtime.revision)
            } else {
                let revision = runtime.transition(SessionStateKind::Closed, false);
                runtime.closed_reason = Some(reason.to_string());
                self.stop.store(true, Ordering::SeqCst);
                self.closed.send_replace(true);
                (true, revision)
            }
        };
        if !changed {
            return;
        }
        #[cfg(feature = "desk-engine")]
        {
            let _guard = self.endpoint_lock.lock().unwrap_or_else(|e| e.into_inner());
            let engine = crate::desk::engine::EngineBackend;
            if engine
                .endpoint(
                    self.engine_conn,
                    &serde_json::json!({"action":"privacy.get"}),
                )
                .map(|v| v["enabled"] == true)
                .unwrap_or(false)
            {
                if let Err(err) = engine.endpoint(
                    self.engine_conn,
                    &serde_json::json!({"action":"privacy.set","enabled":false}),
                ) {
                    tracing::warn!(%err, "隐私屏恢复失败");
                }
            }
        }
        // 释放输入：先抬起所有按下的键/按钮，避免目标机卡住。
        if let Some(mut sink) = self.input.lock().unwrap_or_else(|e| e.into_inner()).take() {
            sink.release_all();
        }
        self.events.publish(EventEnvelope::new(
            self.id,
            revision,
            EventKind::Closed {
                payload: ClosedPayload {
                    reason: reason.to_string(),
                },
            },
        ));
    }

    /// 等待采集线程退出（关闭时调用，确保资源回收）。
    pub fn join_worker(&self, timeout: Duration) {
        let receiver = self
            .worker_done
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(receiver) = receiver {
            let _ = receiver.recv_timeout(timeout);
        }
    }

    /// 标记流可用并广播状态变化。
    fn publish_state(&self, state: SessionStateKind, stream_available: bool) {
        let (revision, target_authentication) = {
            let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            if runtime.state.is_terminal() {
                return;
            }
            let revision = runtime.transition(state, stream_available);
            if state.is_terminal() {
                self.closed.send_replace(true);
            }
            (revision, runtime.target_authentication)
        };
        self.events.publish(EventEnvelope::new(
            self.id,
            revision,
            EventKind::State {
                payload: SessionState {
                    state,
                    state_revision: revision.to_string(),
                    stream_available,
                    target_authentication,
                },
            },
        ));
    }

    fn fail(&self, reason: &str) {
        let revision = {
            let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            if runtime.state.is_terminal() {
                return;
            }
            self.stop.store(true, Ordering::SeqCst);
            self.closed.send_replace(true);
            runtime.target_authentication = TargetAuthentication::Failed;
            runtime.transition(SessionStateKind::Failed, false)
        };
        if let Some(mut sink) = self.input.lock().unwrap_or_else(|e| e.into_inner()).take() {
            sink.release_all();
        }
        self.events.publish(EventEnvelope::new(
            self.id,
            revision,
            EventKind::Closed {
                payload: ClosedPayload {
                    reason: reason.to_string(),
                },
            },
        ));
    }

    fn owner(&self) -> &Owner {
        &self.owner
    }
}

/// desk 会话管理器。
pub struct DeskSessionManager {
    backend: Arc<dyn Backend>,
    clock: Arc<dyn Clock>,
    sessions: Mutex<HashMap<uuid::Uuid, Arc<DeskSession>>>,
    chats: Mutex<HashMap<String, Arc<super::chat::ChatRoom>>>,
    max_sessions: usize,
    product_id: String,
    encoder_settings: EncoderSettings,
}

impl std::fmt::Debug for DeskSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeskSessionManager")
            .field("backend", &self.backend.name())
            .field("max_sessions", &self.max_sessions)
            .finish()
    }
}

impl DeskSessionManager {
    /// 创建管理器。
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self::with_settings(
            backend,
            EncoderSettings::default(),
            DEFAULT_MAX_SESSIONS,
            "yoodesk",
        )
    }

    /// 指定编码参数、会话上限与产品 id。
    pub fn with_settings(
        backend: Arc<dyn Backend>,
        encoder_settings: EncoderSettings,
        max_sessions: usize,
        product_id: &str,
    ) -> Self {
        Self {
            backend,
            clock: Arc::new(crate::access::SystemClock),
            sessions: Mutex::new(HashMap::new()),
            chats: Mutex::new(HashMap::new()),
            max_sessions: max_sessions.max(1),
            product_id: product_id.to_string(),
            encoder_settings,
        }
    }

    /// 聊天按访问会话隔离，并在业务重连时保留；过期房间不无限积累。
    pub fn chat_for(
        &self,
        context: &AccessContext,
    ) -> Result<Arc<super::chat::ChatRoom>, ApiError> {
        let mut chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        chats.retain(|_, room| !room.is_closed());
        if let Some(room) = chats.get(&context.principal.principal_id) {
            room.join(context.session_id, context.expires_at);
            return Ok(room.clone());
        }
        if chats.len() >= 64 {
            return Err(ApiError::new(
                503,
                codes::INTERNAL,
                "chat room limit reached",
            ));
        }
        let room = super::chat::ChatRoom::new(context.expires_at);
        room.join(context.session_id, context.expires_at);
        chats.insert(context.principal.principal_id.clone(), room.clone());
        Ok(room)
    }

    /// 本机管理页读取各主体的有界聊天快照。
    pub fn chat_rooms(&self) -> serde_json::Value {
        let mut chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        chats.retain(|_, room| !room.is_closed());
        serde_json::json!({"rooms": chats.iter().filter_map(|(id, room)| room.read(0).ok().map(|history| serde_json::json!({"conversation_id": id, "history": history}))).collect::<Vec<_>>()})
    }

    /// 本机管理入口只向已经存在的房间发言，不能匿名建立远程会话。
    pub fn chat_reply(
        &self,
        conversation: &str,
        message_id: uuid::Uuid,
        text: &str,
    ) -> Result<serde_json::Value, ApiError> {
        let chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        let room = chats
            .get(conversation)
            .ok_or_else(|| ApiError::new(404, codes::NOT_FOUND, "chat conversation not found"))?;
        Ok(serde_json::json!({"message": room.append(message_id, "host", text)?}))
    }

    /// 后端引用，用于能力响应。
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    /// 显示器列表。
    pub fn displays(&self) -> Result<Vec<crate::desk::DisplayGeometry>, ApiError> {
        self.backend.displays()
    }

    /// 当前会话数。
    pub fn session_count(&self) -> usize {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.len()
    }

    /// 创建 desk 会话并启动采集线程。
    ///
    /// `options` 仅接受 `display_id` 与 `read_only`；未知能力/目标返回明确错误。
    pub fn create(
        &self,
        context: &AccessContext,
        target_id: &str,
        options: &CreateOptions,
    ) -> Result<Arc<DeskSession>, ApiError> {
        if target_id != "local" {
            return Err(ApiError::new(
                404,
                codes::NOT_FOUND,
                "desk target must be \"local\"",
            ));
        }
        let display = match &options.display_id {
            None => 0u32,
            Some(value) => value
                .parse::<u32>()
                .map_err(|_| ApiError::new(400, codes::INVALID_REQUEST, "invalid display_id"))?,
        };
        if !self.backend.accepts_display(display)? {
            return Err(ApiError::new(404, codes::NOT_FOUND, "display not found"));
        }
        {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            // 清理已关闭会话，保证上限只统计活跃会话。
            sessions.retain(|_, session| {
                let runtime = session.runtime.lock().unwrap_or_else(|e| e.into_inner());
                !runtime.state.is_terminal()
            });
            if sessions.len() >= self.max_sessions {
                return Err(
                    ApiError::new(429, codes::CAPACITY_EXCEEDED, "too many desk sessions")
                        .retryable(true),
                );
            }
        }

        // 输入注入器与采集器不同：前者只在 HTTP 请求线程上被调用（受 Mutex 保护），
        // 不涉及 D3D11 线程亲和，可以在这里创建并尽早暴露平台错误。
        let input: Option<Box<dyn InputSink>> =
            if options.read_only || !self.backend.supports_input() {
                None
            } else {
                Some(self.backend.create_input()?)
            };
        let id = uuid::Uuid::new_v4();
        let (frame_tx, _) = broadcast::channel(FRAME_CHANNEL_CAPACITY);
        let (cursor_tx, _) = broadcast::channel(CURSOR_CHANNEL_CAPACITY);
        let (keyframe_tx, keyframe_rx) = std::sync::mpsc::sync_channel(1);
        let (cursor_request_tx, cursor_request_rx) = std::sync::mpsc::sync_channel(1);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let capabilities = self.backend.capabilities();
        let clipboard = self.backend.clipboard();
        let (audio_tx, _) = broadcast::channel(32);
        #[cfg(feature = "desk-engine")]
        let engine_conn = next_engine_conn();

        let session = Arc::new(DeskSession {
            id,
            owner: Owner {
                access_session_id: context.session_id,
                principal_id: context.principal.principal_id.clone(),
            },
            product_id: self.product_id.clone(),
            target_id: target_id.to_string(),
            display,
            read_only: options.read_only || input.is_none(),
            chat: self.chat_for(context)?,
            runtime: Mutex::new(Runtime::new()),
            events: EventBus::new(16),
            frames: frame_tx,
            cursors: cursor_tx,
            keyframe_requests: keyframe_tx,
            cursor_requests: cursor_request_tx,
            input: Mutex::new(input),
            stop: Arc::new(AtomicBool::new(false)),
            closed: tokio::sync::watch::channel(false).0,
            worker_done: Mutex::new(Some(done_rx)),
            created_at: self.clock.now(),
            access_expires_at: Mutex::new(context.expires_at),
            capabilities,
            clipboard: Mutex::new(clipboard),
            last_clipboard: Mutex::new(None),
            audio: audio_tx,
            audio_format: Mutex::new(None),
            #[cfg(feature = "desk-engine")]
            engine_conn,
            endpoint_lock: Mutex::new(()),
            #[cfg(feature = "desk-engine")]
            media_lock: Mutex::new(()),
        });

        // 先置为 connecting，再由采集线程在拿到第一帧后置为 transport_ready。
        session.publish_state(SessionStateKind::Connecting, false);
        {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            // 初始化期间其他请求可能已占满名额；在同一临界区检查并登记，
            // 只有成功登记的会话才能启动采集任务。
            if sessions.len() >= self.max_sessions {
                return Err(
                    ApiError::new(429, codes::CAPACITY_EXCEEDED, "too many desk sessions")
                        .retryable(true),
                );
            }
            sessions.insert(id, session.clone());
        }
        #[cfg(not(feature = "desk-engine"))]
        let engine_conn = 0;
        self.spawn_capture(
            session.clone(),
            display,
            engine_conn,
            keyframe_rx,
            cursor_request_rx,
            done_tx,
        );

        Ok(session)
    }

    /// 启动采集线程。
    ///
    /// 采集器与编码器都在 blocking 线程内创建：D3D11 设备/上下文与 DXGI duplication
    /// 必须在同一线程创建和轮询，否则在 Windows 上表现为永远收不到帧。
    fn spawn_capture(
        &self,
        session: Arc<DeskSession>,
        display_id: u32,
        engine_conn: i32,
        keyframe_requests: std::sync::mpsc::Receiver<()>,
        cursor_requests: std::sync::mpsc::Receiver<()>,
        done: std::sync::mpsc::Sender<()>,
    ) {
        let backend = self.backend.clone();
        let encoder_settings = self.encoder_settings;
        let stop = session.stop.clone();
        tokio::task::spawn_blocking(move || {
            let mut capturer = match backend.create_capturer(display_id) {
                Ok(capturer) => capturer,
                Err(err) => {
                    tracing::error!(session = %session.id, %err, "创建采集器失败");
                    session.fail("capture_unavailable");
                    let _ = done.send(());
                    return;
                }
            };
            // 引擎后端自带编码：不创建本产品的 H.264 编码器，改为拉取引擎码流。
            #[cfg(feature = "desk-engine")]
            let engine = backend.as_any_engine();
            #[cfg(feature = "desk-engine")]
            if let Some(engine) = engine {
                tracing::info!(
                    "进入引擎采集路径 session={} display={} conn={}",
                    session.id,
                    display_id,
                    engine_conn
                );
                if let Err(err) = engine.subscribe(display_id, engine_conn, encoder_settings) {
                    tracing::error!(session = %session.id, %err, "引擎订阅失败");
                    session.fail("capture_unavailable");
                    let _ = done.send(());
                    return;
                }
                run_engine_loop(
                    engine,
                    &session,
                    engine_conn,
                    keyframe_requests,
                    cursor_requests,
                    stop,
                );
                let _ = done.send(());
                return;
            }
            let mut encoder = match H264Encoder::new(encoder_settings) {
                Ok(encoder) => encoder,
                Err(err) => {
                    tracing::error!(session = %session.id, %err, "创建编码器失败");
                    session.fail("encode_unavailable");
                    let _ = done.send(());
                    return;
                }
            };
            // 光标来源与采集器同线程创建：两者都依赖平台桌面会话状态。
            let mut cursor = backend.cursor_source();
            let mut first_frame = true;
            let display_name = capturer.geometry().name.clone();
            tracing::info!(
                session = %session.id,
                display = session.display,
                %display_name,
                gdi = capturer.is_gdi(),
                "desk 采集开始"
            );
            // A lost create response must not reserve capture until the login expires.
            let started = std::time::Instant::now();
            let first_frame_deadline = capturer
                .first_frame_timeout()
                .map(|timeout| started + timeout);
            let mut attach_deadline = first_frame_deadline
                .is_none()
                .then(|| started + STREAM_ATTACH_TIMEOUT);
            while !stop.load(Ordering::SeqCst) {
                if first_frame
                    && first_frame_deadline
                        .is_some_and(|deadline| std::time::Instant::now() >= deadline)
                {
                    session.fail("first_frame_timeout");
                    break;
                }
                session.expire_unattached_stream(&mut attach_deadline);
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                // 访问截止即结束关联业务（设计 §2）：过期后不得继续采集或注入。
                if SystemTime::now()
                    >= *session
                        .access_expires_at
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                {
                    tracing::info!(session = %session.id, "访问到期，结束 desk 会话");
                    session.close("access_expired");
                    break;
                }
                // 新观众加入：编码一次当前画面并标记为关键帧。
                while keyframe_requests.try_recv().is_ok() {
                    match capture_snapshot(&mut capturer, &mut encoder, &session) {
                        Ok(true) => first_frame = false,
                        Ok(false) => {}
                        Err(err) => {
                            tracing::warn!(session = %session.id, %err, "关键帧快照失败");
                        }
                    }
                }
                // 新观众加入：重发一次当前光标形状（形状不重复发送，订阅者会错过）。
                while cursor_requests.try_recv().is_ok() {
                    if let Some(source) = cursor.as_mut() {
                        // 形状只在变化时产生，而广播不保留历史：先重置记忆再采样，
                        // 保证新观众拿到可绘制内容。
                        source.resend_shape();
                        match source.poll() {
                            Ok(Some(cursor_frame)) => {
                                for message in cursor_messages(&cursor_frame) {
                                    let _ = session.cursors.send(Arc::new(message));
                                }
                            }
                            Ok(None) => {}
                            Err(err) => {
                                tracing::debug!(session = %session.id, %err, "重发光标失败");
                            }
                        }
                    }
                }
                // 光标与画面独立变化：即使画面静止也要上报位置/形状变化。
                if let Some(source) = cursor.as_mut() {
                    match source.poll() {
                        Ok(Some(cursor_frame)) => {
                            for message in cursor_messages(&cursor_frame) {
                                let _ = session.cursors.send(Arc::new(message));
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            tracing::debug!(session = %session.id, %err, "读取光标失败");
                        }
                    }
                }
                match capturer.next_frame(CAPTURE_TIMEOUT) {
                    Ok(frame) => {
                        match encoder.encode(frame.width, frame.height, &frame.data) {
                            Ok(encoded) => {
                                if let Some(message) =
                                    to_message(&encoded, frame.width, frame.height)
                                {
                                    if first_frame {
                                        first_frame = false;
                                        if first_frame_deadline.is_some() {
                                            attach_deadline = Some(
                                                std::time::Instant::now() + STREAM_ATTACH_TIMEOUT,
                                            );
                                        }
                                        session.publish_state(SessionStateKind::TransportReady, true);
                                        session.publish_state(SessionStateKind::Active, true);
                                    }
                                    // 无订阅者时 `send` 失败是正常的；丢弃而不是报错。
                                    let _ = session.frames.send(Arc::new(message));
                                }
                            }
                            Err(err) => {
                                tracing::warn!(session = %session.id, %err, "编码失败");
                                session.fail("encode_failed");
                                break;
                            }
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        tracing::trace!(session = %session.id, "画面无变化");
                    }
                    Err(err) => {
                        tracing::warn!(session = %session.id, %err, "采集失败");
                        session.fail("capture_failed");
                        break;
                    }
                }
            }
            session.stop.store(true, Ordering::SeqCst);
            session.publish_state(SessionStateKind::Closed, false);
            tracing::info!(session = %session.id, "desk 采集结束");
            let _ = done.send(());
        });
    }

    /// 按归属查找会话；无权或不存在统一返回 `not_found`，避免枚举。
    pub fn find(
        &self,
        context: &AccessContext,
        id: uuid::Uuid,
    ) -> Result<Arc<DeskSession>, ApiError> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let session = sessions
            .get(&id)
            .ok_or_else(|| ApiError::new(404, codes::NOT_FOUND, "session not found"))?;
        if session.owner().access_session_id != context.session_id
            || session.owner().principal_id != context.principal.principal_id
        {
            return Err(ApiError::new(404, codes::NOT_FOUND, "session not found"));
        }
        Ok(session.clone())
    }

    /// 关闭并移除会话；重复关闭幂等返回。
    pub fn close(
        &self,
        context: &AccessContext,
        id: uuid::Uuid,
        reason: &str,
    ) -> Result<(), ApiError> {
        let session = self.find(context, id)?;
        session.close(reason);
        session.join_worker(Duration::from_secs(2));
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.remove(&id);
        Ok(())
    }

    /// 访问会话退出时关闭它创建的全部 desk 会话。
    pub fn close_all_for(&self, access_session_id: u64, reason: &str) {
        let targets: Vec<Arc<DeskSession>> = {
            let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .values()
                .filter(|s| s.owner().access_session_id == access_session_id)
                .cloned()
                .collect()
        };
        for session in &targets {
            session.close(reason);
            session.join_worker(Duration::from_secs(2));
        }
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        for session in targets {
            sessions.remove(&session.id);
        }
        drop(sessions);
        let mut chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        for room in chats.values() {
            room.leave(access_session_id);
        }
        chats.retain(|_, room| !room.is_closed());
    }

    /// 关闭全部会话（进程关闭路径）。
    ///
    /// 采集线程是 `spawn_blocking` 任务，不随 manager 一起结束：
    /// 不显式关闭会让测试或重启进程留下仍在轮询桌面的线程。
    pub fn close_all(&self, reason: &str) {
        for room in self
            .chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, room)| room)
        {
            room.close();
        }
        let targets: Vec<Arc<DeskSession>> = {
            let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions.values().cloned().collect()
        };
        for session in &targets {
            session.close(reason);
            session.join_worker(Duration::from_secs(2));
        }
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        for session in targets {
            sessions.remove(&session.id);
        }
    }

    /// 访问续期后同步所有该访问会话创建的业务截止时间。
    pub fn update_access_expiry_for(
        &self,
        access_session_id: u64,
        expires_at: SystemTime,
        lease_version: u64,
    ) {
        for room in self
            .chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            room.renew_member(access_session_id, expires_at);
        }
        let sessions: Vec<Arc<DeskSession>> = {
            let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .values()
                .filter(|s| s.owner().access_session_id == access_session_id)
                .cloned()
                .collect()
        };
        for session in sessions {
            session.update_access_expiry(expires_at, lease_version);
        }
    }

    /// 所有活跃会话的快照，用于状态观测。
    pub fn snapshots(&self) -> Vec<SessionInfo> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.values().map(|s| s.info()).collect()
    }
}

impl Drop for DeskSessionManager {
    fn drop(&mut self) {
        for room in self
            .chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            room.close();
        }
        // 只发停止信号，不 join：`Drop` 可能在异步运行时关闭期间执行，
        // 阻塞会放大关闭延迟。采集线程会在下一次轮询后自行退出。
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        for session in sessions.values() {
            session.stop.store(true, Ordering::SeqCst);
        }
    }
}

/// 创建会话时允许的 options 子集。
#[derive(Debug, Clone, Default)]
pub struct CreateOptions {
    /// 显示器 id（十进制字符串）。
    pub display_id: Option<String>,
    /// 只读会话：不注入输入。
    pub read_only: bool,
}

impl CreateOptions {
    /// 从 `SessionCreate.options` 解析；未知字段已在严格 Schema 层拒绝，
    /// 这里只取本能力实际支持的字段。
    pub fn from_json(options: &serde_json::Value) -> Result<Self, ApiError> {
        let object = options.as_object().ok_or_else(|| {
            ApiError::new(400, codes::INVALID_REQUEST, "options must be an object")
        })?;
        let display_id = match object.get("display_id") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(value)) => Some(value.clone()),
            Some(_) => {
                return Err(ApiError::new(
                    400,
                    codes::INVALID_REQUEST,
                    "display_id must be a string",
                ))
            }
        };
        let read_only = match object.get("read_only") {
            None | Some(serde_json::Value::Null) => false,
            Some(serde_json::Value::Bool(value)) => *value,
            Some(_) => {
                return Err(ApiError::new(
                    400,
                    codes::INVALID_REQUEST,
                    "read_only must be a boolean",
                ))
            }
        };
        Ok(Self {
            display_id,
            read_only,
        })
    }
}

/// 引擎订阅 id 的分配器。
///
/// 每个 desk 会话必须有自己的 id：重连时新旧会话会短暂并存，共用同一 id 会让旧会话
/// 的收尾把新会话的订阅一起撤掉。
#[cfg(feature = "desk-engine")]
fn next_engine_conn() -> i32 {
    use std::sync::atomic::{AtomicI32, Ordering};
    static NEXT: AtomicI32 = AtomicI32::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

/// 引擎轮询窗口。
///
/// x264 在静态桌面上会延迟输出首帧（需要累积参考帧），但窗口太长又会让光标消息
/// 在等待帧期间积压。200ms 是两者的折中：首帧最多多轮几次，光标延迟保持在可感知
/// 范围之外。
#[cfg(feature = "desk-engine")]
const ENGINE_POLL_TIMEOUT: Duration = Duration::from_millis(200);

/// 引擎后端的主循环：直接转发引擎已编码的 H.264。
///
/// 引擎自行完成采集、YUV 转换与编码，这里只负责把码流包装成 `hbb.Message`
/// 并广播给订阅者，因此与自研后端的“采集 + 编码”路径互不复用。
#[cfg(feature = "desk-engine")]
fn run_engine_loop(
    engine: &crate::desk::engine::EngineBackend,
    session: &DeskSession,
    engine_conn: i32,
    keyframe_requests: std::sync::mpsc::Receiver<()>,
    cursor_requests: std::sync::mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
) {
    use crate::proto::{EncodedVideoFrame, EncodedVideoFrames, Message, VideoFrame};
    use protobuf::Message as _;

    let mut first_frame = true;
    tracing::info!(session = %session.id, display = session.display, "引擎采集开始");
    let mut attach_deadline = Some(std::time::Instant::now() + STREAM_ATTACH_TIMEOUT);
    while !stop.load(Ordering::SeqCst) {
        session.expire_unattached_stream(&mut attach_deadline);
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if SystemTime::now()
            >= *session
                .access_expires_at
                .lock()
                .unwrap_or_else(|e| e.into_inner())
        {
            tracing::info!(session = %session.id, "访问到期，结束 desk 会话");
            session.close("access_expired");
            break;
        }
        // 新观众需要可解码的起始帧；引擎按 GOP 输出关键帧，这里等到下一帧即可，
        // 不重编码，避免与引擎自身的编码状态冲突。
        // 新观众加入：重放最近的关键帧，否则广播里只有后续小帧（没有 SPS/PPS）。
        while keyframe_requests.try_recv().is_ok() {
            if let Err(err) = engine.replay_keyframe(engine_conn) {
                tracing::debug!("关键帧重放失败: {err}");
            }
        }
        // 新观众加入：重放最近的光标形状。RustDesk 对每个形状只发一次 CursorData，
        // 错过就只剩 CursorId 引用，浏览器画不出光标。
        while cursor_requests.try_recv().is_ok() {
            if let Err(err) = engine.replay_cursor(engine_conn) {
                tracing::debug!("光标重放失败: {err}");
            }
        }
        // 光标与帧共用一条业务流。每轮最多取少量光标消息：光标可能持续到达，
        // 无上限的排干会让帧轮询永远轮不到。
        for _ in 0..8 {
            match engine.poll_cursor(engine_conn) {
                Ok(Some((cursor, is_shape))) => {
                    let _ = session.cursors.send(Arc::new(cursor));
                    let _ = is_shape;
                }
                Ok(None) => break,
                Err(err) => {
                    tracing::debug!("光标轮询失败: {err}");
                    break;
                }
            }
        }
        for _ in 0..32 {
            match engine.poll_audio(engine_conn, false) {
                Ok(Some(bytes)) => {
                    // 上游与冻结快照的音频字段兼容；解析保证不会把其它消息误发到音频通道。
                    if let Ok(msg) = Message::parse_from_bytes(&bytes) {
                        let is_format = matches!(&msg.union, Some(crate::proto::message_v1::message::Union::Misc(m)) if matches!(m.union, Some(crate::proto::message_v1::misc::Union::AudioFormat(_))));
                        let bytes = Arc::new(bytes);
                        if is_format {
                            *session
                                .audio_format
                                .lock()
                                .unwrap_or_else(|e| e.into_inner()) = Some(bytes.clone());
                        }
                        let _ = session.audio.send(bytes);
                    } else {
                        tracing::warn!(session = %session.id, "引擎音频消息不符合冻结协议");
                        session.fail("audio_protocol_error");
                        break;
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!(%err,"音频轮询失败");
                    break;
                }
            }
        }
        // 轮询窗口同时决定首帧延迟与光标延迟：太短会让首帧前空转多次，
        // 太长会让光标消息在等待帧时积压。
        match engine.poll(engine_conn, ENGINE_POLL_TIMEOUT) {
            Ok(Some((data, key, pts))) => {
                tracing::trace!(
                    "引擎取到帧 session={} bytes={} key={}",
                    session.id,
                    data.len(),
                    key
                );
                if first_frame {
                    first_frame = false;
                    tracing::info!(session = %session.id, bytes = data.len(), key, "Desk 收到首个 H.264 帧");
                    session.publish_state(SessionStateKind::TransportReady, true);
                    session.publish_state(SessionStateKind::Active, true);
                }
                let mut encoded = EncodedVideoFrame::new();
                encoded.data = data;
                encoded.key = key;
                encoded.pts = pts;
                let mut frames = EncodedVideoFrames::new();
                frames.frames.push(encoded);
                let mut video = VideoFrame::new();
                video.set_h264s(frames);
                video.display = session.display as i32;
                let mut message = Message::new();
                message.set_video_frame(video);
                if let Ok(bytes) = message.write_to_bytes() {
                    if session.frames.send(Arc::new(bytes)).is_err() {
                        // 未打开网页流时释放首帧；订阅者加入会请求完整的新 IDR。
                        let _ = engine.ack_video(engine_conn, pts);
                    }
                }
            }
            Ok(None) => {
                if crate::desk::engine::rpc::enabled() {
                    // 在共享IPC锁外维持空帧节奏，让输入、剪贴板和ACK可立即进入。
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Err(err) if err.status == 429 => {
                // 未进入 worker 的非破坏性轮询可在下一拍重试，不能因此关闭会话。
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(err) => {
                tracing::warn!(session = %session.id, %err, "引擎采集失败");
                session.fail("capture_failed");
                break;
            }
        }
    }
    {
        let _guard = session
            .endpoint_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _media = session.media_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = engine.unsubscribe_conn(engine_conn) {
            tracing::warn!(%err, "引擎会话资源回收失败");
        }
    }
    session.stop.store(true, Ordering::SeqCst);
    session.publish_state(SessionStateKind::Closed, false);
    tracing::info!(session = %session.id, "引擎采集结束");
}

/// 读取一次当前画面并作为关键帧广播。
///
/// 返回 `Ok(true)` 表示确实编码并发送了帧；`Ok(false)` 表示编码器没有产出可发送内容。
fn capture_snapshot(
    capturer: &mut Box<dyn crate::desk::Capturer>,
    encoder: &mut H264Encoder,
    session: &DeskSession,
) -> Result<bool, ApiError> {
    let frame = capturer
        .snapshot()
        .map_err(|err| ApiError::new(502, codes::DESK_ENGINE, err.to_string()))?;
    encoder.request_keyframe();
    let encoded = encoder.encode(frame.width, frame.height, &frame.data)?;
    if let Some(message) = to_message(&encoded, frame.width, frame.height) {
        let _ = session.frames.send(Arc::new(message));
        return Ok(true);
    }
    Ok(false)
}

/// 把光标状态转换为 `hbb.Message` 列表。
///
/// 形状变化时先发 `cursor_data`（新形状 + 热点），再发 `cursor_position`；
/// 只有位置变化时仅发 `cursor_position`。浏览器据此缓存形状、移动绘制。
fn cursor_messages(frame: &crate::desk::CursorFrame) -> Vec<Vec<u8>> {
    use crate::proto::{CursorData, CursorPosition, Message};
    use protobuf::Message as _;

    let mut messages = Vec::new();
    if let Some(rgba) = &frame.rgba {
        let mut data = CursorData::new();
        data.id = frame.id;
        data.hotx = frame.hotx;
        data.hoty = frame.hoty;
        data.width = frame.width as i32;
        data.height = frame.height as i32;
        // 协议字段名是 `colors`，这里是 RGBA8；浏览器按 width/height 解析。
        data.colors = rgba.clone();
        let mut message = Message::new();
        message.set_cursor_data(data);
        if let Ok(bytes) = message.write_to_bytes() {
            messages.push(bytes);
        }
    }
    if let Some((x, y)) = frame.position {
        let mut position = CursorPosition::new();
        position.x = x;
        position.y = y;
        let mut message = Message::new();
        message.set_cursor_position(position);
        if let Ok(bytes) = message.write_to_bytes() {
            messages.push(bytes);
        }
    }
    messages
}

/// 把编码结果包装为一个 `hbb.Message`（`video_frame.h264s`）。
///
/// WS 每条 binary 就是一个 `hbb.Message`；这里同时设置 `display` 与 `pts`，
/// 浏览器侧不需要再猜测时间基准。
fn to_message(encoded: &EncodedFrame, width: u32, height: u32) -> Option<Vec<u8>> {
    use crate::proto::{EncodedVideoFrame, EncodedVideoFrames, Message, VideoFrame};
    use protobuf::Message as _;

    let mut frame = EncodedVideoFrame::new();
    frame.data = encoded.data.clone();
    frame.key = encoded.key;
    frame.pts = encoded.pts_ms;
    let mut frames = EncodedVideoFrames::new();
    frames.frames.push(frame);
    let mut video = VideoFrame::new();
    video.set_h264s(frames);
    video.display = 0;
    let _ = (width, height);
    let mut message = Message::new();
    message.set_video_frame(video);
    message.write_to_bytes().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{AccessMode, LocalPrincipal};

    fn context() -> AccessContext {
        AccessContext {
            session_id: 7,
            principal: LocalPrincipal {
                principal_id: "local".into(),
                scopes: vec!["session.connect".into(), "screen.view".into()],
            },
            mode: AccessMode::Cookie,
            csrf: "csrf".into(),
            expires_at: SystemTime::UNIX_EPOCH + Duration::from_secs(10_000),
            absolute_expires_at: SystemTime::UNIX_EPOCH + Duration::from_secs(20_000),
            created_at: SystemTime::UNIX_EPOCH,
        }
    }

    fn manager() -> DeskSessionManager {
        DeskSessionManager::new(Arc::new(crate::desk::UnsupportedBackend {
            platform: "test",
        }))
    }

    // 输入工厂暂停两个并发创建，保证二者都已通过第一次容量检查。
    struct ConcurrentBackend {
        barrier: Option<std::sync::Barrier>,
        fail_input: AtomicBool,
    }

    impl Backend for ConcurrentBackend {
        fn name(&self) -> &'static str {
            "concurrent-test"
        }
        fn displays(&self) -> Result<Vec<crate::desk::DisplayGeometry>, ApiError> {
            Ok(vec![crate::desk::DisplayGeometry {
                index: 0,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                name: "test".into(),
                primary: true,
            }])
        }
        fn capabilities(&self) -> crate::desk::DeskCapabilities {
            crate::desk::DeskCapabilities::none()
        }
        fn create_capturer(&self, _: u32) -> Result<Box<dyn crate::desk::Capturer>, ApiError> {
            Err(ApiError::new(500, codes::INTERNAL, "test capture failure"))
        }
        fn create_input(&self) -> Result<Box<dyn InputSink>, ApiError> {
            if self.fail_input.swap(false, Ordering::SeqCst) {
                return Err(ApiError::new(500, codes::INTERNAL, "test input failure"));
            }
            if let Some(barrier) = &self.barrier {
                barrier.wait();
            }
            Ok(Box::new(TestInput))
        }
        fn supports_input(&self) -> bool {
            true
        }
        fn supports_h264(&self) -> bool {
            false
        }
    }

    struct TestInput;
    impl InputSink for TestInput {
        fn mouse(&mut self, _: crate::desk::input::MouseInput) -> Result<(), ApiError> {
            Ok(())
        }
        fn key(&mut self, _: crate::desk::input::KeyInput) -> Result<(), ApiError> {
            Ok(())
        }
        fn release_all(&mut self) {}
    }

    #[tokio::test]
    async fn concurrent_creates_cannot_exceed_session_limit() {
        let manager = Arc::new(DeskSessionManager::with_settings(
            Arc::new(ConcurrentBackend {
                barrier: Some(std::sync::Barrier::new(2)),
                fail_input: AtomicBool::new(false),
            }),
            EncoderSettings::default(),
            1,
            "test",
        ));
        let mut owner = context();
        owner.expires_at = SystemTime::now() + Duration::from_secs(60);
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let manager = manager.clone();
            let owner = owner.clone();
            tasks.push(tokio::task::spawn_blocking(move || {
                manager.create(&owner, "local", &CreateOptions::default())
            }));
        }
        let mut accepted = 0;
        let mut rejected = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => accepted += 1,
                Err(error) => {
                    assert_eq!(error.code, codes::CAPACITY_EXCEEDED);
                    rejected += 1;
                }
            }
        }
        assert_eq!((accepted, rejected), (1, 1));
        assert_eq!(manager.session_count(), 1);
    }

    #[tokio::test]
    async fn failed_initialization_and_capture_release_session_capacity() {
        let manager = DeskSessionManager::with_settings(
            Arc::new(ConcurrentBackend {
                barrier: None,
                fail_input: AtomicBool::new(true),
            }),
            EncoderSettings::default(),
            1,
            "test",
        );
        let mut owner = context();
        owner.expires_at = SystemTime::now() + Duration::from_secs(60);
        assert_eq!(
            manager
                .create(&owner, "local", &CreateOptions::default())
                .unwrap_err()
                .code,
            codes::INTERNAL
        );
        assert_eq!(manager.session_count(), 0);
        let session = manager
            .create(&owner, "local", &CreateOptions::default())
            .unwrap();
        // blocking采集任务明确失败并发出done后，再验证新创建可以回收名额。
        tokio::task::spawn_blocking(move || session.join_worker(Duration::from_secs(5)))
            .await
            .unwrap();
        assert!(manager
            .create(&owner, "local", &CreateOptions::default())
            .is_ok());
        assert_eq!(manager.session_count(), 1);
    }

    #[test]
    fn create_requires_supported_backend_and_local_target() {
        let manager = manager();
        let err = manager
            .create(&context(), "local", &CreateOptions::default())
            .unwrap_err();
        assert_eq!(err.code, codes::UNSUPPORTED_FEATURE);
        let err = manager
            .create(&context(), "remote-host", &CreateOptions::default())
            .unwrap_err();
        // 目标校验先于后端能力，`local` 才继续到采集。
        assert_eq!(err.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn expired_desktop_session_rejects_input_even_before_capture_notices_expiry() {
        let manager = DeskSessionManager::with_settings(
            Arc::new(ConcurrentBackend {
                barrier: None,
                fail_input: AtomicBool::new(false),
            }),
            EncoderSettings::default(),
            1,
            "test",
        );
        let mut owner = context();
        owner.expires_at = SystemTime::now() + Duration::from_secs(60);
        let session = manager
            .create(&owner, "local", &CreateOptions::default())
            .unwrap();
        let joined = session.clone();
        tokio::task::spawn_blocking(move || joined.join_worker(Duration::from_secs(5)))
            .await
            .unwrap();
        {
            let mut runtime = session.runtime.lock().unwrap();
            *runtime = Runtime::new();
            runtime.transition(SessionStateKind::TransportReady, true);
        }
        session.stop.store(false, Ordering::SeqCst);
        *session.input.lock().unwrap() = Some(Box::new(TestInput));
        *session.access_expires_at.lock().unwrap() = SystemTime::UNIX_EPOCH;
        assert_eq!(
            session
                .key(crate::desk::input::KeyInput::Text("x".into()))
                .unwrap_err()
                .status,
            409
        );
    }

    #[tokio::test]
    async fn bulk_cleanup_does_not_remove_sessions_created_while_waiting() {
        struct BlockingRelease(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);
        impl InputSink for BlockingRelease {
            fn mouse(&mut self, _: crate::desk::input::MouseInput) -> Result<(), ApiError> {
                Ok(())
            }
            fn key(&mut self, _: crate::desk::input::KeyInput) -> Result<(), ApiError> {
                Ok(())
            }
            fn release_all(&mut self) {
                self.0.send(()).unwrap();
                self.1.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        }

        let manager = Arc::new(DeskSessionManager::with_settings(
            Arc::new(ConcurrentBackend {
                barrier: None,
                fail_input: AtomicBool::new(false),
            }),
            EncoderSettings::default(),
            2,
            "test",
        ));
        let mut owner = context();
        owner.expires_at = SystemTime::now() + Duration::from_secs(60);
        let old = manager
            .create(&owner, "local", &CreateOptions::default())
            .unwrap();
        let joined = old.clone();
        tokio::task::spawn_blocking(move || joined.join_worker(Duration::from_secs(5)))
            .await
            .unwrap();
        *old.runtime.lock().unwrap() = Runtime::new();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *old.input.lock().unwrap() = Some(Box::new(BlockingRelease(entered_tx, release_rx)));
        let closer = manager.clone();
        let cleanup = tokio::task::spawn_blocking(move || closer.close_all("worker_replaced"));
        tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(3)))
            .await
            .unwrap()
            .unwrap();
        let new = manager
            .create(&owner, "local", &CreateOptions::default())
            .unwrap();
        release_tx.send(()).unwrap();
        cleanup.await.unwrap();
        let sessions = manager.sessions.lock().unwrap();
        assert!(!sessions.contains_key(&old.id));
        assert!(sessions.contains_key(&new.id));
    }

    #[tokio::test]
    async fn stream_subscription_and_last_stream_close_are_atomic() {
        let manager = DeskSessionManager::with_settings(
            Arc::new(ConcurrentBackend {
                barrier: None,
                fail_input: AtomicBool::new(false),
            }),
            EncoderSettings::default(),
            1,
            "test",
        );
        let mut owner = context();
        owner.expires_at = SystemTime::now() + Duration::from_secs(60);
        let session = manager
            .create(&owner, "local", &CreateOptions::default())
            .unwrap();
        let joined = session.clone();
        tokio::task::spawn_blocking(move || joined.join_worker(Duration::from_secs(5)))
            .await
            .unwrap();
        // 此处只测试已就绪状态下的订阅/关闭锁序，不启动真实采集。
        for _ in 0..128 {
            *session.runtime.lock().unwrap() = Runtime::new();
            session
                .runtime
                .lock()
                .unwrap()
                .transition(SessionStateKind::TransportReady, true);
            let barrier = std::sync::Barrier::new(2);
            let receiver = std::thread::scope(|scope| {
                let subscriber = scope.spawn(|| {
                    barrier.wait();
                    session.subscribe_frames()
                });
                let closer = scope.spawn(|| {
                    barrier.wait();
                    session.close_if_no_streams();
                });
                let receiver = subscriber.join().unwrap();
                closer.join().unwrap();
                receiver
            });
            let terminal = session.runtime.lock().unwrap().state.is_terminal();
            assert_eq!(
                receiver.is_err(),
                terminal,
                "new subscriber must remain active or be rejected before admission"
            );
            drop(receiver);
        }
    }

    #[test]
    fn options_parse_rejects_wrong_types() {
        let options = serde_json::json!({"display_id": 1});
        assert!(CreateOptions::from_json(&options).is_err());
        let options = serde_json::json!({"read_only": "yes"});
        assert!(CreateOptions::from_json(&options).is_err());
        let options = serde_json::json!({"display_id": "1", "read_only": true});
        let parsed = CreateOptions::from_json(&options).unwrap();
        assert_eq!(parsed.display_id.as_deref(), Some("1"));
        assert!(parsed.read_only);
    }

    #[test]
    fn state_revision_is_monotonic_and_terminal_is_stable() {
        let runtime = Mutex::new(Runtime::new());
        {
            let mut r = runtime.lock().unwrap();
            assert_eq!(r.transition(SessionStateKind::Connecting, false), 2);
            assert_eq!(r.transition(SessionStateKind::Active, true), 3);
        }
        let manager = manager();
        let _ = manager;
        assert!(SessionStateKind::Closed.is_terminal());
    }

    #[test]
    fn message_encodes_as_hbb_video_frame() {
        use crate::proto::{message::Union, Message};
        use protobuf::Message as _;
        let encoded = EncodedFrame {
            data: vec![0, 0, 0, 1, 0x67, 1, 2, 3],
            key: true,
            pts_ms: 42,
        };
        let bytes = to_message(&encoded, 1280, 720).unwrap();
        let message = Message::parse_from_bytes(&bytes).unwrap();
        let Some(Union::VideoFrame(video)) = message.union else {
            panic!("expected video frame");
        };
        let Some(crate::proto::video_frame::Union::H264s(frames)) = video.union else {
            panic!("expected h264s");
        };
        assert_eq!(frames.frames.len(), 1);
        assert_eq!(frames.frames[0].pts, 42);
        assert!(frames.frames[0].key);
        assert_eq!(&frames.frames[0].data[..], &[0, 0, 0, 1, 0x67, 1, 2, 3]);
    }
}
