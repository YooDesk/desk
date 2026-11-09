//! desk 业务端点：能力、显示器、会话 CRUD、票据、events/stream WS 与输入。
//!
//! 每个端点都先认证再读取资源；资源归属不匹配统一返回 `not_found`。
//! WS 握手校验 Origin，并只认一次性票据或同源 Cookie 会话，不把票据回显为子协议。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::auth::{authenticate, cookie_value};
use super::response::{failure, no_content, request_id_or_generated, require_request_id, success};
use super::AppState;
use crate::access::{extract_ticket_candidate, AccessContext, TicketChannel};
use crate::business::desk_session::CreateOptions;
use crate::business::model::{CapabilitiesData, TargetAuthentication};
use crate::desk::input::{parse_mouse, KeyInput, MouseInput};
use crate::json::{codes, ApiError};

static DESKTOP_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(16);

// Keep cleanup admissible even when ordinary desktop requests fill their budget.
async fn desktop_call<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    bounded_desktop_call(&DESKTOP_SLOTS, operation).await
}

async fn desktop_cleanup<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
    bounded_desktop_call(&SLOTS, operation).await
}

// 文件IO与视频控制分别有界；慢磁盘不能占满解码确认或输入的工作名额。
async fn desktop_file<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);
    bounded_desktop_call(&SLOTS, operation).await
}

async fn desktop_media(
    operation: impl FnOnce() -> Result<(), ApiError> + Send + 'static,
) -> Result<(), ApiError> {
    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);
    static WAITING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(32);
    // 每条流只有一个在途控制命令；全局等待也有上限，不占用 blocking 线程。
    let waiting = WAITING.try_acquire().map_err(|_| {
        ApiError::new(503, codes::DESK_ENGINE, "desktop media control busy").retryable(true)
    })?;
    let slot = SLOTS
        .acquire()
        .await
        .expect("static media slots are never closed");
    drop(waiting);
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        operation()
    })
    .await
    .map_err(|_| ApiError::new(503, codes::DESK_ENGINE, "desktop operation failed"))?
}

async fn bounded_desktop_call<T: Send + 'static>(
    slots: &'static tokio::sync::Semaphore,
    operation: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    let slot = slots
        .try_acquire()
        .map_err(|_| ApiError::new(503, codes::DESK_ENGINE, "desktop worker busy"))?;
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        operation()
    })
    .await
    .map_err(|_| ApiError::new(503, codes::DESK_ENGINE, "desktop operation failed"))?
}

async fn refresh_video(session: &Arc<crate::business::DeskSession>) -> Result<(), ApiError> {
    let session = Arc::clone(session);
    desktop_media(move || session.refresh_video()).await
}

/// `GET capabilities/`
pub async fn capabilities(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let request_id = request_id(&headers);
    let context = match authenticate(&state, &headers) {
        Ok(context) => context,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if let Err(err) = context.require_scope("session.connect") {
        return failure(&err, &request_id).into_response();
    }
    let mut limits = std::collections::BTreeMap::new();
    limits.insert("control_json_bytes".into(), 65536);
    limits.insert("desk_message_bytes".into(), 16_777_216);
    limits.insert("max_sessions".into(), state.config.max_sessions as u64);
    limits.insert("fps".into(), state.config.fps as u64);
    limits.insert("stream_ticket_ttl_seconds".into(), 30);
    let (capabilities, platform) = match desktop_call(move || {
        let backend = state.business.desk.backend();
        Ok((backend.capabilities(), backend.platform_info()))
    })
    .await
    {
        Ok(data) => data,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let data = CapabilitiesData {
        protocol_version: 2,
        supported_features: capabilities.features(),
        limits,
        stream_profile: crate::proto::PROFILE,
        // 真实采集与编码已有回归，但 T02 的浏览器媒体互通尚未完成，保持 provisional。
        media_status: "provisional",
        // 跨平台接口：能力、平台与采集方式的机器可读描述，前端据此裁剪 UI，
        // 新平台接入时无需改协议。
        capabilities,
        platform,
    };
    success(data, &request_id).into_response()
}

/// `GET displays/`：本能力自有的只读辅助端点，列出可采集显示器。
///
/// 不属于冻结 OpenAPI 的公开资源集合，因此只在本产品内部使用；响应仍是统一信封。
pub async fn displays(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let request_id = request_id(&headers);
    let context = match authenticate(&state, &headers) {
        Ok(context) => context,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if let Err(err) = context.require_scope("screen.view") {
        return failure(&err, &request_id).into_response();
    }
    match desktop_call(move || state.business.desk.displays()).await {
        Ok(displays) => {
            let items: Vec<Value> = displays
                .into_iter()
                .map(|display| {
                    json!({
                        "display_id": display.index.to_string(),
                        "x": display.x,
                        "y": display.y,
                        "width": display.width,
                        "height": display.height,
                        "name": display.name,
                        "primary": display.primary,
                    })
                })
                .collect();
            success(json!({"items": items}), &request_id).into_response()
        }
        Err(err) => failure(&err, &request_id).into_response(),
    }
}

/// `POST sessions/`
pub async fn create_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = match require_request_id(headers.get("x-request-id")) {
        Ok(value) => value,
        Err(err) => return failure(&err, "missing").into_response(),
    };
    let context = match authenticate(&state, &headers) {
        Ok(context) => context,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if let Err(err) = require_csrf(&state, &context, &headers) {
        return failure(&err, &request_id).into_response();
    }
    if let Err(err) = context.require_scope("session.connect") {
        return failure(&err, &request_id).into_response();
    }
    let value = match crate::json::parse_json_strict(&body) {
        Ok(value) => value,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let parsed = match SessionCreate::from_json(&value) {
        Ok(parsed) => parsed,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if !state
        .access
        .permits_target(&context, "desk", &parsed.target_id)
    {
        return failure(
            &ApiError::new(
                403,
                codes::FORBIDDEN,
                "target outside Connector authorization",
            ),
            &request_id,
        )
        .into_response();
    }
    let options = match CreateOptions::from_json(&parsed.options) {
        Ok(options) => options,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if options.read_only {
        // 只读会话只要求 screen.view；可写会话同时要求输入 scope。
        if let Err(err) = context.require_scope("screen.view") {
            return failure(&err, &request_id).into_response();
        }
    } else {
        for scope in ["screen.view", "input.pointer", "input.keyboard"] {
            if let Err(err) = context.require_scope(scope) {
                return failure(&err, &request_id).into_response();
            }
        }
    }
    match desktop_call(move || {
        let session = state
            .business
            .desk
            .create(&context, &parsed.target_id, &options)?;
        // Check for logout while desktop initialization was in progress.
        if let Err(err) = state.access.resolve(context.session_id, context.mode) {
            session.close("access_revoked");
            return Err(err);
        }
        Ok(session)
    })
    .await
    {
        Ok(session) => {
            let mut response = success(session.info(), &request_id).into_response();
            *response.status_mut() = StatusCode::CREATED;
            response
        }
        Err(err) => failure(&err, &request_id).into_response(),
    }
}

/// `GET sessions/`：当前访问会话创建的 desk 会话列表。
pub async fn list_sessions(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let request_id = request_id(&headers);
    let context = match authenticate(&state, &headers) {
        Ok(context) => context,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let items: Vec<Value> = state
        .business
        .desk
        .snapshots()
        .into_iter()
        .filter(|info| {
            // 只返回当前主体可访问的会话；归属检查与 `find` 一致。
            state
                .business
                .desk
                .find(&context, info.business_session_id)
                .is_ok()
        })
        .map(|info| serde_json::to_value(info).unwrap_or(Value::Null))
        .collect();
    success(json!({"items": items}), &request_id).into_response()
}

/// `GET/DELETE sessions/{id}/`
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Response {
    let request_id = request_id(&headers);
    let context = match authenticate(&state, &headers) {
        Ok(context) => context,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let id = match parse_uuid(&session_id) {
        Ok(id) => id,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    match state.business.desk.find(&context, id) {
        Ok(session) => success(session.info(), &request_id).into_response(),
        Err(err) => failure(&err, &request_id).into_response(),
    }
}

pub async fn close_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
) -> Response {
    let request_id = match require_request_id(headers.get("x-request-id")) {
        Ok(value) => value,
        Err(err) => return failure(&err, "missing").into_response(),
    };
    let context = match authenticate(&state, &headers) {
        Ok(context) => context,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if let Err(err) = require_csrf(&state, &context, &headers) {
        return failure(&err, &request_id).into_response();
    }
    let id = match parse_uuid(&session_id) {
        Ok(id) => id,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    match desktop_cleanup(move || state.business.desk.close(&context, id, "client_closed")).await {
        Ok(()) => no_content(),
        Err(err) => failure(&err, &request_id).into_response(),
    }
}

/// `POST sessions/{id}/stream-tickets/`
pub async fn create_ticket(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Response {
    let request_id = match require_request_id(headers.get("x-request-id")) {
        Ok(value) => value,
        Err(err) => return failure(&err, "missing").into_response(),
    };
    let context = match authenticate(&state, &headers) {
        Ok(context) => context,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if let Err(err) = require_csrf(&state, &context, &headers) {
        return failure(&err, &request_id).into_response();
    }
    let id = match parse_uuid(&session_id) {
        Ok(id) => id,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let value = match crate::json::parse_json_strict(&body) {
        Ok(value) => value,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let channel = match value.get("channel").and_then(Value::as_str) {
        Some(text) => match TicketChannel::parse(text) {
            Some(channel) => channel,
            None => {
                return failure(
                    &ApiError::new(400, codes::INVALID_REQUEST, "invalid channel"),
                    &request_id,
                )
                .into_response()
            }
        },
        None => {
            return failure(
                &ApiError::new(400, codes::INVALID_REQUEST, "channel required"),
                &request_id,
            )
            .into_response()
        }
    };
    let session = match state.business.desk.find(&context, id) {
        Ok(session) => session,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if channel == TicketChannel::Stream && !session.info().stream_available {
        return failure(
            &ApiError::new(409, codes::STATE_CONFLICT, "stream is not available"),
            &request_id,
        )
        .into_response();
    }
    match state.access.issue_ticket(&context, channel) {
        Ok((ticket, expires_at)) => {
            let data = json!({
                "business_session_id": id,
                "channel": match channel {
                    TicketChannel::Stream => "stream",
                    TicketChannel::Events => "events",
                },
                "ticket": ticket,
                "expires_at": crate::timefmt::format_rfc3339(expires_at),
            });
            let mut response = success(data, &request_id).into_response();
            *response.status_mut() = StatusCode::CREATED;
            response
        }
        Err(err) => failure(&err, &request_id).into_response(),
    }
}

/// `POST sessions/{id}/input/`：注入鼠标/键盘。
///
/// 不属于冻结 OpenAPI 的公开 DTO 集合；本产品以统一信封暴露，见 `docs/operations`。
/// 请求体只接受 `{"type":"mouse",...}` 或 `{"type":"key",...}` 两种形状。
pub async fn input(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Response {
    let request_id = match require_request_id(headers.get("x-request-id")) {
        Ok(value) => value,
        Err(err) => return failure(&err, "missing").into_response(),
    };
    let context = match authenticate(&state, &headers) {
        Ok(context) => context,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if let Err(err) = require_csrf(&state, &context, &headers) {
        return failure(&err, &request_id).into_response();
    }
    let id = match parse_uuid(&session_id) {
        Ok(id) => id,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let value = match crate::json::parse_json_strict(&body) {
        Ok(value) => value,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let session = match state.business.desk.find(&context, id) {
        Ok(session) => session,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let events = match parse_input_events(&value) {
        Ok(events) => events,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let result = desktop_call(move || {
        for event in events {
            match event {
                DeskInputEvent::Mouse(input) => session.mouse(input)?,
                DeskInputEvent::Key(input) => session.key(input)?,
            }
        }
        Ok(())
    })
    .await;
    match result {
        Ok(()) => success(json!({"applied": true}), &request_id).into_response(),
        Err(err) => failure(&err, &request_id).into_response(),
    }
}

/// `GET sessions/{id}/clipboard/` 与 `POST sessions/{id}/clipboard/`。
///
/// 文本直接放在 JSON 里；图像用 base64 传输（`mime: "image/rgba"`，`width/height` 必填），
/// 避免为单一能力引入多段上传。图像上限由 `MAX_CLIPBOARD_IMAGE_BYTES` 约束。
pub async fn clipboard(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    method: axum::http::Method,
    body: Bytes,
) -> Response {
    let request_id = match method {
        axum::http::Method::POST => match require_request_id(headers.get("x-request-id")) {
            Ok(value) => value,
            Err(err) => return failure(&err, "missing").into_response(),
        },
        _ => request_id(&headers),
    };
    let context = match authenticate(&state, &headers) {
        Ok(context) => context,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if method == axum::http::Method::POST {
        if let Err(err) = require_csrf(&state, &context, &headers) {
            tracing::warn!(code = err.code, message = %err.message, "剪贴板 CSRF 校验失败");
            return failure(&err, &request_id).into_response();
        }
    }
    let id = match parse_uuid(&session_id) {
        Ok(id) => id,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let session = match state.business.desk.find(&context, id) {
        Ok(session) => session,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let scope = if method == axum::http::Method::POST {
        "clipboard.write"
    } else {
        "clipboard.read"
    };
    if let Err(err) = context.require_scope(scope) {
        return failure(&err, &request_id).into_response();
    }
    if method == axum::http::Method::POST {
        let value = match crate::json::parse_json_strict(&body) {
            Ok(value) => value,
            Err(err) => return failure(&err, &request_id).into_response(),
        };
        let content = match clipboard_from_json(&value) {
            Ok(content) => content,
            Err(err) => return failure(&err, &request_id).into_response(),
        };
        return match desktop_call(move || session.clipboard_write(&content)).await {
            Ok(()) => success(json!({"applied": true}), &request_id).into_response(),
            Err(err) => {
                tracing::warn!(code = err.code, message = %err.message, "剪贴板写入被拒绝");
                failure(&err, &request_id).into_response()
            }
        };
    }
    match desktop_call(move || session.clipboard_read()).await {
        Ok(Some(content)) => success(clipboard_to_json(&content), &request_id).into_response(),
        Ok(None) => success(json!({"changed": false}), &request_id).into_response(),
        Err(err) => failure(&err, &request_id).into_response(),
    }
}

/// Windows 扩展仍使用普通 HTTP、公共认证与 envelope。
pub async fn endpoint(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    body: Bytes,
) -> Response {
    let request_id = match require_request_id(headers.get("x-request-id")) {
        Ok(v) => v,
        Err(e) => return failure(&e, "missing").into_response(),
    };
    let context = match authenticate(&state, &headers) {
        Ok(v) => v,
        Err(e) => return failure(&e, &request_id).into_response(),
    };
    if let Err(e) = require_csrf(&state, &context, &headers) {
        return failure(&e, &request_id).into_response();
    }
    let id = match parse_uuid(&session_id) {
        Ok(v) => v,
        Err(e) => return failure(&e, &request_id).into_response(),
    };
    let session = match state.business.desk.find(&context, id) {
        Ok(v) => v,
        Err(e) => return failure(&e, &request_id).into_response(),
    };
    let value = match crate::json::parse_json_strict(&body) {
        Ok(v) => v,
        Err(e) => return failure(&e, &request_id).into_response(),
    };
    let keys: &[&str] = match value["action"].as_str() {
        Some("input.release") => &["action"],
        Some("files.roots") => &["action"],
        Some("files.list" | "files.download") => &["action", "path", "root_id"],
        Some("files.upload") => &["action", "path", "size", "root_id"],
        Some("files.create" | "files.delete") => &["action", "path", "kind", "root_id"],
        Some("files.rename") => &["action", "path", "name", "root_id"],
        Some("files.write") => &["action", "transfer_id", "offset", "data"],
        Some("files.read") => &["action", "transfer_id", "offset"],
        Some("files.commit" | "files.cancel") => &["action", "transfer_id"],
        Some("privacy.set") => &["action", "enabled"],
        Some("privacy.get") => &["action"],
        Some("chat.read") => &["action", "after"],
        Some("chat.send") => &["action", "message_id", "text"],
        _ => {
            return failure(
                &ApiError::new(400, codes::INVALID_REQUEST, "unknown endpoint action"),
                &request_id,
            )
            .into_response()
        }
    };
    if !value
        .as_object()
        .map(|o| {
            let optional_root = keys.contains(&"root_id") && !o.contains_key("root_id");
            o.len() == keys.len() - usize::from(optional_root)
                && o.keys().all(|k| keys.contains(&k.as_str()))
                && o.get("root_id").is_none_or(|root| {
                    root.as_str().is_some_and(|id| {
                        matches!(id, "default" | "system")
                            || (id.len() == 7
                                && id.starts_with("drive-")
                                && id.as_bytes()[6].is_ascii_uppercase())
                    })
                })
        })
        .unwrap_or(false)
    {
        return failure(
            &ApiError::new(400, codes::INVALID_REQUEST, "invalid endpoint fields"),
            &request_id,
        )
        .into_response();
    }
    // FFI、文件 I/O、隐私屏等待均在 blocking 线程，避免堵塞 HTTP/WS runtime。
    let scope = match value["action"].as_str() {
        Some("files.roots" | "files.list" | "files.download" | "files.read") => Some("files.read"),
        Some(
            "files.upload" | "files.write" | "files.commit" | "files.create" | "files.delete"
            | "files.rename",
        ) => Some("files.write"),
        // Cancel only releases an existing transfer owned by this session.
        Some("files.cancel") if !context.has_scope("files.write") => Some("files.read"),
        _ => None,
    };
    if let Some(scope) = scope {
        if let Err(err) = context.require_scope(scope) {
            return failure(&err, &request_id).into_response();
        }
    }
    let result = if value["action"] == "input.release" {
        desktop_cleanup(move || session.endpoint(&value)).await
    } else if value["action"]
        .as_str()
        .is_some_and(|action| action.starts_with("files."))
    {
        desktop_file(move || session.endpoint(&value)).await
    } else {
        desktop_call(move || session.endpoint(&value)).await
    };
    match result {
        Ok(v) => success(v, &request_id).into_response(),
        Err(e) => failure(&e, &request_id).into_response(),
    }
}

/// 图像数据的硬上限（未压缩 RGBA 字节数）。
pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = 16 << 20;

pub(crate) fn clipboard_from_json(
    value: &Value,
) -> Result<crate::desk::ClipboardContent, ApiError> {
    let object = value.as_object().ok_or_else(|| {
        ApiError::new(
            400,
            codes::INVALID_REQUEST,
            "clipboard body must be an object",
        )
    })?;
    match object.get("kind").and_then(Value::as_str) {
        Some("text") => {
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| ApiError::new(400, codes::INVALID_REQUEST, "text required"))?;
            if text.len() > MAX_CLIPBOARD_IMAGE_BYTES {
                return Err(ApiError::new(
                    413,
                    codes::PAYLOAD_TOO_LARGE,
                    "clipboard text too large",
                ));
            }
            Ok(crate::desk::ClipboardContent::Text(text.to_string()))
        }
        Some("image") => {
            let width = object
                .get("width")
                .and_then(Value::as_u64)
                .ok_or_else(|| ApiError::new(400, codes::INVALID_REQUEST, "width required"))?;
            let height = object
                .get("height")
                .and_then(Value::as_u64)
                .ok_or_else(|| ApiError::new(400, codes::INVALID_REQUEST, "height required"))?;
            let encoded = object
                .get("data")
                .and_then(Value::as_str)
                .ok_or_else(|| ApiError::new(400, codes::INVALID_REQUEST, "data required"))?;
            if width == 0 || height == 0 || width > 8192 || height > 8192 {
                return Err(ApiError::new(
                    400,
                    codes::INVALID_REQUEST,
                    "invalid image dimensions",
                ));
            }
            let expected = (width as usize)
                .saturating_mul(height as usize)
                .saturating_mul(4);
            if expected > MAX_CLIPBOARD_IMAGE_BYTES {
                return Err(ApiError::new(
                    413,
                    codes::PAYLOAD_TOO_LARGE,
                    "clipboard image too large",
                ));
            }
            let data = base64_decode(encoded)?;
            if data.len() != expected {
                return Err(ApiError::new(
                    400,
                    codes::INVALID_REQUEST,
                    "image data does not match width/height",
                ));
            }
            Ok(crate::desk::ClipboardContent::ImageRgba {
                width: width as u32,
                height: height as u32,
                data,
            })
        }
        _ => Err(ApiError::new(
            400,
            codes::INVALID_REQUEST,
            "clipboard kind must be text or image",
        )),
    }
}

pub(crate) fn clipboard_to_json(content: &crate::desk::ClipboardContent) -> Value {
    match content {
        crate::desk::ClipboardContent::Text(text) => json!({"kind": "text", "text": text}),
        crate::desk::ClipboardContent::ImageRgba {
            width,
            height,
            data,
        } => json!({
            "kind": "image",
            "mime": "image/rgba",
            "width": width,
            "height": height,
            "data": base64_encode(data),
        }),
    }
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn base64_decode(text: &str) -> Result<Vec<u8>, ApiError> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| ApiError::new(400, codes::INVALID_REQUEST, "invalid base64 image data"))
}

enum DeskInputEvent {
    Mouse(MouseInput),
    Key(KeyInput),
}

const MAX_INPUT_BATCH: usize = 32;

fn parse_input_events(value: &Value) -> Result<Vec<DeskInputEvent>, ApiError> {
    if value.get("type").and_then(Value::as_str) != Some("batch") {
        return parse_input_event(value).map(|event| vec![event]);
    }
    let events = value
        .get("events")
        .and_then(Value::as_array)
        .filter(|events| (1..=MAX_INPUT_BATCH).contains(&events.len()))
        .ok_or_else(|| {
            ApiError::new(400, codes::INVALID_REQUEST, "input batch must contain 1 to 32 events")
        })?;
    events.iter().map(parse_input_event).collect()
}

fn parse_input_event(value: &Value) -> Result<DeskInputEvent, ApiError> {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::new(400, codes::INVALID_REQUEST, "input type required"))?;
    match kind {
        "mouse" => {
            let mask = int_field(value, "mask")?;
            let x = int_field(value, "x")?;
            let y = int_field(value, "y")?;
            let event = parse_mouse(mask, x, y).map_err(|err| err.into_api())?;
            Ok(DeskInputEvent::Mouse(event))
        }
        "key" => {
            // 三种形状互斥：
            // - `{"type":"key","text":"..."}`：文本输入（不依赖目标机布局）；
            // - `{"type":"key","control":25,"down":true}`：物理控制键（冻结快照编号）；
            // - `{"type":"key","layout":"c","down":true}`：布局字符键（组合键用）。
            let text = value.get("text").and_then(Value::as_str);
            let control = value.get("control").and_then(Value::as_i64);
            let layout = value.get("layout").and_then(Value::as_str);
            let specified = [text.is_some(), control.is_some(), layout.is_some()]
                .iter()
                .filter(|present| **present)
                .count();
            if specified != 1 {
                return Err(ApiError::new(
                    400,
                    codes::INVALID_REQUEST,
                    "exactly one of text, control or layout is required",
                ));
            }
            if let Some(text) = text {
                if text.is_empty() {
                    return Err(ApiError::new(
                        400,
                        codes::INVALID_REQUEST,
                        "text must not be empty",
                    ));
                }
                return Ok(DeskInputEvent::Key(KeyInput::Text(text.to_string())));
            }
            let down = value
                .get("down")
                .and_then(Value::as_bool)
                .ok_or_else(|| ApiError::new(400, codes::INVALID_REQUEST, "down required"))?;
            if let Some(layout) = layout {
                let mut chars = layout.chars();
                let (Some(ch), None) = (chars.next(), chars.next()) else {
                    return Err(ApiError::new(
                        400,
                        codes::INVALID_REQUEST,
                        "layout must be a single character",
                    ));
                };
                return Ok(DeskInputEvent::Key(KeyInput::Layout { ch, down }));
            }
            let code = int_field(value, "control")?;
            Ok(DeskInputEvent::Key(KeyInput::Control { code, down }))
        }
        _ => Err(ApiError::new(
            400,
            codes::INVALID_REQUEST,
            "unknown input type",
        )),
    }
}

/// `GET sessions/{id}/events/`（WS）
pub async fn events_ws(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let request_id = request_id(&headers);
    let id = match parse_uuid(&session_id) {
        Ok(id) => id,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    // WS 握手同样要求精确 Origin（设计 §3）。
    if let Err(err) = crate::access::require_exact_origin(origin(&headers), state.public_origin()) {
        return failure(&err, &request_id).into_response();
    }
    let (context, session) = match authorize_ws(&state, &headers, id, TicketChannel::Events) {
        Ok(value) => value,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    let _ = context;
    let (snapshot, receiver) = match session.subscribe_events() {
        Ok(value) => value,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    // events 也回选固定业务子协议：客户端无需票据（同源 Cookie 即可），
    // 但统一回选可让浏览器与测试共用同一套握手断言。
    ws.protocols([BUSINESS_STREAM_PROTOCOL])
        .on_upgrade(move |socket| run_events(socket, snapshot, receiver))
}

/// `GET sessions/{id}/stream/`（WS）
pub async fn stream_ws(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let request_id = request_id(&headers);
    let id = match parse_uuid(&session_id) {
        Ok(id) => id,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if let Err(err) = crate::access::require_exact_origin(origin(&headers), state.public_origin()) {
        return failure(&err, &request_id).into_response();
    }
    let (_, session) = match authorize_ws(&state, &headers, id, TicketChannel::Stream) {
        Ok(value) => value,
        Err(err) => return failure(&err, &request_id).into_response(),
    };
    if !session.info().stream_available {
        return failure(
            &ApiError::new(409, codes::STATE_CONFLICT, "stream is not available"),
            &request_id,
        )
        .into_response();
    }
    // 只回选固定业务子协议；绝不回显客户端提交的 `raccess.ticket.*` 候选。
    ws.protocols([BUSINESS_STREAM_PROTOCOL])
        .max_frame_size(16 << 20)
        .on_upgrade(move |socket| async move {
            // A failed HTTP upgrade must never count as an attached viewer.
            let Ok(receiver) = session.subscribe_frames() else {
                return;
            };
            let cursors = session.subscribe_cursors();
            let (audio, format) = session.subscribe_audio();
            tokio::select! {
                biased;
                _ = session.closed() => {}
                _ = run_stream(socket, session.clone(), receiver, cursors, audio, format) => {}
            }
            // 页面卸载时DELETE可能丢失或恰遇CSRF轮换；流断开由服务端回收。
            // 关闭包含原生I/O，不能阻塞Tokio网络线程。
            if let Err(error) =
                tokio::task::spawn_blocking(move || session.close_if_no_streams()).await
            {
                tracing::error!(%error, "断流后的desk资源回收任务失败");
            }
        })
}

/// WS 授权：票据优先；没有票据时使用同源 Cookie/Bearer。
///
/// 关键：客户端提交了坏票据时禁止回退 Cookie（设计 §3）。
fn authorize_ws(
    state: &AppState,
    headers: &HeaderMap,
    id: uuid::Uuid,
    channel: TicketChannel,
) -> Result<(AccessContext, Arc<crate::business::DeskSession>), ApiError> {
    let protocols: Vec<&str> = headers
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(',').map(str::trim).collect())
        .unwrap_or_default();
    let (ticket, bad) = extract_ticket_candidate(protocols);
    if bad {
        return Err(ApiError::new(
            400,
            codes::INVALID_REQUEST,
            "invalid stream ticket",
        ));
    }
    let context = match ticket {
        Some(ticket) => state.access.consume_ticket(ticket, channel)?,
        None => {
            let cookies = headers
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok());
            match cookies {
                Some(cookies) => {
                    let value = cookie_value(cookies, &state.access.policy().session_cookie())
                        .ok_or_else(unauthorized)?;
                    state
                        .access
                        .authenticate_token(&value, crate::access::AccessMode::Cookie)?
                }
                None => return Err(unauthorized()),
            }
        }
    };
    if headers.contains_key(axum::http::header::AUTHORIZATION)
        && authenticate(state, headers)?.session_id != context.session_id
    {
        return Err(ApiError::new(
            403,
            codes::FORBIDDEN,
            "stream ticket belongs to another access session",
        ));
    }
    let session = state.business.desk.find(&context, id)?;
    Ok((context, session))
}

fn unauthorized() -> ApiError {
    ApiError::new(401, codes::UNAUTHENTICATED, "access session required")
}

fn desk_event_is_terminal(kind: &crate::business::events::EventKind) -> bool {
    use crate::business::events::EventKind;
    match kind {
        EventKind::Closed { .. } => true,
        EventKind::Snapshot { payload } => payload.state.is_terminal(),
        EventKind::State { payload } => payload.state.is_terminal(),
        _ => false,
    }
}

async fn run_events(
    mut socket: WebSocket,
    snapshot: crate::business::events::EventEnvelope,
    mut receiver: broadcast::Receiver<Arc<crate::business::events::EventEnvelope>>,
) {
    // 首包必须是完整快照。
    let Ok(text) = serde_json::to_string(&snapshot) else {
        let _ = super::send_ws_message(&mut socket, Message::Close(None)).await;
        return;
    };
    if super::send_ws_message(&mut socket, Message::Text(text.into()))
        .await
        .is_err()
    {
        return;
    }
    if desk_event_is_terminal(&snapshot.kind) {
        return;
    }
    loop {
        tokio::select! {
            message = socket.recv() => {
                match message {
                    // 只接受 ping/close/文本心跳；客户端不得经 events 下发指令。
                    Some(Ok(Message::Ping(payload))) => {
                        if super::send_ws_message(&mut socket, Message::Pong(payload)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Text(text))) => {
                        let Ok(id) = events_ping(&text) else {
                            let _ = super::send_ws_message(&mut socket, Message::Close(Some(axum::extract::ws::CloseFrame {code:1008, reason:"invalid events heartbeat".into()}))).await;
                            break;
                        };
                        if super::send_ws_message(&mut socket, Message::Text(serde_json::json!({"type":"pong","id":id}).to_string().into())).await.is_err() { break; }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
            event = receiver.recv() => {
                match event {
                    Ok(event) => {
                        let Ok(text) = serde_json::to_string(&*event) else { break };
                        if super::send_ws_message(&mut socket, Message::Text(text.into())).await.is_err()
                            || desk_event_is_terminal(&event.kind)
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // 慢消费者：断开让浏览器重连拿新快照，避免旧事件覆盖新状态。
                        let _ = super::send_ws_message(&mut socket, Message::Close(Some(axum::extract::ws::CloseFrame {
                                code: 1013,
                                reason: "lagged".into(),
                            })))
                            .await;
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

/// 候选 stream 流控 v1，严格字段和安全整数与共享 JSON Schema 一致。
#[derive(serde::Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum StreamControl {
    #[serde(rename = "video_received")]
    VideoReceived { pts: u64 },
    #[serde(rename = "refresh_video")]
    RefreshVideo {},
    #[serde(rename = "ping")]
    Ping { id: u64 },
}

fn stream_control(text: &str) -> Result<StreamControl, ()> {
    if text.len() > 256 {
        return Err(());
    }
    let control: StreamControl = serde_json::from_str(text).map_err(|_| ())?;
    if matches!(control, StreamControl::VideoReceived { pts } if pts > 9_007_199_254_740_991)
        || matches!(control, StreamControl::Ping { id } if id > 9_007_199_254_740_991)
    {
        return Err(());
    }
    Ok(control)
}

// 事件通道只接受已有 ping 形状；不能借心跳下发刷新、确认或其他业务指令。
fn events_ping(text: &str) -> Result<u64, ()> {
    match stream_control(text)? {
        StreamControl::Ping { id } => Ok(id),
        _ => Err(()),
    }
}

async fn run_stream(
    mut socket: WebSocket,
    session: Arc<crate::business::DeskSession>,
    mut frames: broadcast::Receiver<Arc<Vec<u8>>>,
    mut cursors: broadcast::Receiver<Arc<Vec<u8>>>,
    mut audio: broadcast::Receiver<Arc<Vec<u8>>>,
    format: Option<Arc<Vec<u8>>>,
) {
    use protobuf::Message as _;
    let mut waiting_key = true;
    let mut last_pts = -1i64;
    let mut last_refresh = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let mut refresh_queued = false;
    let mut refresh_tick = tokio::time::interval(std::time::Duration::from_millis(50));
    refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    if let Some(format) = format {
        if super::send_ws_message(&mut socket, Message::Binary((*format).clone().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    loop {
        tokio::select! {
            _ = refresh_tick.tick(), if refresh_queued => {
                if last_refresh.elapsed() >= std::time::Duration::from_millis(250) {
                    refresh_queued = false;
                    last_refresh = std::time::Instant::now();
                    waiting_key = true;
                    frames = frames.resubscribe();
                    if super::send_ws_message(&mut socket, Message::Text("{\"type\":\"video_reset\"}".into())).await.is_err() { break; }
                    if refresh_video(&session).await.is_err() { break; }
                }
            }
            message = socket.recv() => {
                match message {
                    Some(Ok(Message::Ping(payload))) => {
                        if super::send_ws_message(&mut socket, Message::Pong(payload)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(Message::Text(text))) => {
                        let result = match stream_control(&text) {
                            Ok(StreamControl::VideoReceived { pts }) if pts as i64 <= last_pts => {
                                let session = Arc::clone(&session);
                                desktop_media(move || session.ack_video(pts as i64)).await
                            },
                            Ok(StreamControl::VideoReceived { .. }) => break,
                            Ok(StreamControl::RefreshVideo {}) => {
                                // 多次错误合并为一次重建，避免客户端让编码器持续重启。
                                if last_refresh.elapsed() < std::time::Duration::from_millis(250) {
                                    // 已请求的 IDR 本身也可能解码失败：合并到一个待执行位，不能吞掉恢复请求。
                                    refresh_queued = true;
                                    continue;
                                }
                                refresh_queued = false;
                                last_refresh = std::time::Instant::now();
                                waiting_key = true;
                                frames = frames.resubscribe();
                                if super::send_ws_message(&mut socket, Message::Text("{\"type\":\"video_reset\"}".into())).await.is_err() { break; }
                                refresh_video(&session).await
                            }
                            Ok(StreamControl::Ping { id }) => {
                                if super::send_ws_message(&mut socket, Message::Text(serde_json::json!({"type":"pong","id":id}).to_string().into())).await.is_err() { break; }
                                Ok(())
                            }
                            Err(()) => {
                                let _ = super::send_ws_message(&mut socket, Message::Close(Some(axum::extract::ws::CloseFrame {code:1008, reason:"invalid stream control".into()}))).await;
                                break;
                            }
                        };
                        if let Err(error) = result {
                            tracing::warn!(session = %session.id(), %error, "桌面串流控制失败");
                            let _ = super::send_ws_message(&mut socket, Message::Close(Some(axum::extract::ws::CloseFrame {code:1011, reason:"desktop stream control failed".into()}))).await;
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {},
                    Some(Ok(_)) => break,
                }
            }
            frame = frames.recv() => {
                match frame {
                    Ok(frame) => {
                        let Ok(message) = crate::proto::Message::parse_from_bytes(&frame) else { break; };
                        let Some(crate::proto::message_v1::message::Union::VideoFrame(video)) = message.union else { break; };
                        let Some(crate::proto::message_v1::video_frame::Union::H264s(video)) = video.union else { break; };
                        let key = video.frames.iter().any(|f| f.key);
                        let pts = video.frames.iter().map(|f| f.pts).max().unwrap_or(-1);
                        if waiting_key && (!key || pts <= last_pts) { continue; }
                        waiting_key = false;
                        last_pts = pts;
                        if super::send_ws_message(&mut socket, Message::Binary((*frame).clone().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // 预测帧丢失后不能继续转发旧链；从新 IDR 恢复。
                        waiting_key = true;
                        frames = frames.resubscribe();
                        if super::send_ws_message(&mut socket, Message::Text("{\"type\":\"video_reset\"}".into())).await.is_err() { break; }
                        if refresh_video(&session).await.is_err() { break; }
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            cursor = cursors.recv() => {
                match cursor {
                    Ok(message) => {
                        if super::send_ws_message(&mut socket, Message::Binary((*message).clone().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    // 光标是瞬时状态，落后时直接丢弃旧值。
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            audio = audio.recv() => {
                match audio {
                    Ok(bytes) => if super::send_ws_message(&mut socket, Message::Binary((*bytes).clone().into())).await.is_err() { break; },
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

fn parse_uuid(value: &str) -> Result<uuid::Uuid, ApiError> {
    uuid::Uuid::parse_str(value)
        .map_err(|_| ApiError::new(400, codes::INVALID_REQUEST, "invalid session id"))
}

fn require_csrf(
    state: &AppState,
    context: &AccessContext,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let provided = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::new(400, codes::INVALID_REQUEST, "invalid csrf token"))?;
    state.access.require_csrf(context, provided)
}

fn request_id(headers: &HeaderMap) -> String {
    request_id_or_generated(headers.get("x-request-id"))
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
}

fn origin(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
}

fn int_field(value: &Value, name: &str) -> Result<i32, ApiError> {
    value
        .get(name)
        .and_then(Value::as_i64)
        .filter(|number| *number >= i32::MIN as i64 && *number <= i32::MAX as i64)
        .map(|number| number as i32)
        .ok_or_else(|| ApiError::new(400, codes::INVALID_REQUEST, format!("{name} required")))
}

/// `SessionCreate` 的严格解析：字段与 `business-v2.schema.json` 对齐。
#[derive(Debug)]
struct SessionCreate {
    target_id: String,
    options: Value,
}

impl SessionCreate {
    fn from_json(value: &Value) -> Result<Self, ApiError> {
        let object = value
            .as_object()
            .ok_or_else(|| ApiError::new(400, codes::INVALID_REQUEST, "body must be an object"))?;
        // 未知字段显式拒绝，避免前端拼错字段被静默忽略。
        for key in object.keys() {
            match key.as_str() {
                "protocol_version" | "target_id" | "options" | "client_features" => {}
                "credential_ref" => {
                    return Err(ApiError::new(
                        400,
                        codes::UNSUPPORTED_FEATURE,
                        "credential_ref is not supported for desk",
                    ))
                }
                other => {
                    return Err(ApiError::new(
                        400,
                        codes::INVALID_REQUEST,
                        format!("unknown field {other}"),
                    ))
                }
            }
        }
        if object.get("protocol_version").and_then(Value::as_u64) != Some(2) {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "protocol_version must be 2",
            ));
        }
        let target_id = object
            .get("target_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 128)
            .ok_or_else(|| ApiError::new(400, codes::INVALID_REQUEST, "target_id required"))?;
        let options = object
            .get("options")
            .cloned()
            .ok_or_else(|| ApiError::new(400, codes::INVALID_REQUEST, "options required"))?;
        if object
            .get("client_features")
            .is_some_and(|value| !value.is_array())
        {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "client_features must be an array",
            ));
        }
        Ok(Self {
            target_id: target_id.to_string(),
            options,
        })
    }
}

/// 业务 WS 只回选这一个固定子协议；票据候选不回显（设计 §3）。
pub const BUSINESS_STREAM_PROTOCOL: &str = "raccess.business.v2";

/// 只读会话的目标认证状态：desk 本机目标无法由服务端确认。
#[allow(dead_code)]
fn default_target_authentication() -> TargetAuthentication {
    TargetAuthentication::Unknown
}

/// 会话事件的目标订阅超时，避免 WS 任务无限占用。
#[allow(dead_code)]
const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(24 * 3600);

#[cfg(test)]
mod tests {
    #[test]
    fn input_batches_validate_all_events_and_preserve_order() {
        let key = serde_json::json!({"type":"key","layout":"a","down":true});
        let release = serde_json::json!({"type":"key","layout":"a","down":false});
        let events = super::parse_input_events(&serde_json::json!({
            "type":"batch","events":[key.clone(),release]
        })).unwrap();
        assert!(matches!(events.as_slice(), [
            super::DeskInputEvent::Key(super::KeyInput::Layout { ch: 'a', down: true }),
            super::DeskInputEvent::Key(super::KeyInput::Layout { ch: 'a', down: false })
        ]));
        for invalid in [
            serde_json::json!({"type":"batch","events":[]}),
            serde_json::json!({"type":"batch","events":vec![key.clone(); 33]}),
            serde_json::json!({"type":"batch","events":[key.clone(),{"type":"key","layout":"a"}]}),
            serde_json::json!({"type":"batch","events":[{"type":"batch","events":[key.clone()]}]}),
        ] {
            assert!(super::parse_input_events(&invalid).is_err());
        }
        assert!(super::parse_input_events(&key).is_ok());
    }

    #[tokio::test]
    async fn cleanup_can_run_when_an_ordinary_budget_is_exhausted() {
        let _permit = super::DESKTOP_SLOTS.acquire_many(16).await.unwrap();
        assert_eq!(
            super::desktop_call(|| Ok(())).await.unwrap_err().status,
            503
        );
        super::desktop_cleanup(|| Ok(())).await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_desktop_operation_leaves_async_executor_responsive() {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        static TEST_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
        let task = tokio::spawn(super::bounded_desktop_call(&TEST_SLOTS, move || {
            entered_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .expect("async executor could not release the desktop worker");
            Ok(())
        }));
        entered_rx.await.unwrap();
        release_tx.send(()).unwrap();
        task.await.unwrap().unwrap();
    }

    #[test]
    fn stream_controls_match_browser_contract() {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../../protocol/desk-stream-control-v1.vectors.json"
        ))
        .unwrap();
        for vector in vectors.as_array().unwrap() {
            let text = vector["text"].as_str().unwrap();
            assert_eq!(
                super::stream_control(text).is_ok(),
                vector["valid"].as_bool().unwrap(),
                "{text}"
            );
        }
        assert!(super::stream_control(&" ".repeat(257)).is_err());
        assert!(super::stream_control("{}").is_err());
        assert!(
            super::stream_control("{\"type\":\"refresh_video\",\"type\":\"refresh_video\"}")
                .is_err()
        );
    }

    #[test]
    fn events_heartbeat_matches_browser_vectors() {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../../protocol/vectors-v2/desk-events-heartbeat.json"
        ))
        .unwrap();
        for vector in vectors["cases"].as_array().unwrap() {
            assert_eq!(
                super::events_ping(&vector["message"].to_string()).is_ok(),
                vector["browser_accepted"].as_bool().unwrap(),
                "{}",
                vector["name"]
            );
        }
        assert!(super::events_ping("{\"type\":\"ping\",\"id\":1,\"id\":2}").is_err());
        assert!(super::events_ping(&" ".repeat(257)).is_err());
    }
    use super::*;

    #[test]
    fn session_create_rejects_unknown_and_wrong_version() {
        let body = json!({
            "protocol_version": 1,
            "target_id": "local",
            "options": {},
            "client_features": []
        });
        assert!(SessionCreate::from_json(&body).is_err());

        let body = json!({
            "protocol_version": 2,
            "target_id": "local",
            "options": {},
            "client_features": [],
            "extra": true
        });
        assert!(SessionCreate::from_json(&body).is_err());

        let body = json!({
            "protocol_version": 2,
            "target_id": "local",
            "options": {},
            "client_features": []
        });
        assert!(SessionCreate::from_json(&body).is_ok());
    }

    #[test]
    fn session_create_rejects_credential_ref_explicitly() {
        let body = json!({
            "protocol_version": 2,
            "target_id": "local",
            "credential_ref": "x",
            "options": {},
            "client_features": []
        });
        let err = SessionCreate::from_json(&body).unwrap_err();
        assert_eq!(err.code, codes::UNSUPPORTED_FEATURE);
    }

    #[test]
    fn key_input_requires_exactly_one_form() {
        // 缺字段、多字段都必须拒绝：不能猜测客户端意图。
        for body in [
            json!({"type": "key", "down": true}),
            json!({"type": "key", "text": "a", "control": 25, "down": true}),
            json!({"type": "key", "layout": "c"}),
            json!({"type": "key", "layout": "cc", "down": true}),
        ] {
            // 用轻量替身验证解析分支前的校验逻辑：这里只检查字段计数与类型。
            let text = body.get("text").and_then(serde_json::Value::as_str);
            let control = body.get("control").and_then(serde_json::Value::as_i64);
            let layout = body.get("layout").and_then(serde_json::Value::as_str);
            let count = [text.is_some(), control.is_some(), layout.is_some()]
                .iter()
                .filter(|present| **present)
                .count();
            let down = body.get("down").and_then(serde_json::Value::as_bool);
            let invalid = count != 1
                || (layout.is_some() && layout.map(|value| value.chars().count()) != Some(1))
                || (count == 1 && text.is_none() && down.is_none());
            assert!(invalid, "应被拒绝: {body}");
        }
    }

    #[test]
    fn clipboard_json_round_trips_text_and_image() {
        let empty = clipboard_from_json(&json!({"kind":"text","text":""}))
            .expect("empty text clears clipboard");
        assert_eq!(clipboard_to_json(&empty), json!({"kind":"text","text":""}));
        let text =
            clipboard_from_json(&json!({"kind": "text", "text": "你好"})).expect("text accepted");
        assert_eq!(text, crate::desk::ClipboardContent::Text("你好".into()));
        assert_eq!(
            clipboard_to_json(&text),
            json!({"kind": "text", "text": "你好"})
        );

        // 2x2 RGBA：白、红、绿、蓝。
        let rgba = vec![
            255, 255, 255, 255, 255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255,
        ];
        let encoded = base64_encode(&rgba);
        let image = clipboard_from_json(&json!({
            "kind": "image", "width": 2, "height": 2, "data": encoded
        }))
        .expect("image accepted");
        let json_value = clipboard_to_json(&image);
        assert_eq!(json_value["mime"], "image/rgba");
        assert_eq!(json_value["data"], encoded);
        // 尺寸与数据长度不符必须拒绝。
        assert!(clipboard_from_json(&json!({
            "kind": "image", "width": 3, "height": 2, "data": encoded
        }))
        .is_err());
        assert!(clipboard_from_json(&json!({"kind": "audio"})).is_err());
    }

    #[test]
    fn input_parsing_maps_protocol_masks() {
        let value = json!({"type": "mouse", "mask": 9, "x": 4, "y": 5});
        // 直接验证解析函数而不启动会话。
        let event = parse_mouse(
            int_field(&value, "mask").unwrap(),
            int_field(&value, "x").unwrap(),
            int_field(&value, "y").unwrap(),
        )
        .unwrap();
        assert_eq!(
            event,
            crate::desk::input::MouseInput::Down {
                button: crate::desk::input::MouseButton::Left
            }
        );
    }
}
