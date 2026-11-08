mod auth;
mod desk;
mod response;
use crate::{
    access::{AccessAuthority, LoginLimiter},
    business::BusinessHub,
    config::Config,
};
use axum::{
    extract::ConnectInfo,
    routing::{any, get, post},
    Router,
};
pub(crate) use desk::{clipboard_from_json, clipboard_to_json};
pub use response::ApiResponse;
use std::{net::SocketAddr, sync::Arc};
/// 应用共享状态。
pub struct AppState {
    /// 服务配置。
    pub config: Config,
    /// 访问权威。
    pub access: Arc<AccessAuthority>,
    /// 业务会话集合。
    pub business: Arc<BusinessHub>,
    /// 登录失败限流。
    pub limiter: LoginLimiter,
}

impl AppState {
    /// 产品前缀，例如 `/yoodesk`。
    pub fn product_prefix(&self) -> String {
        format!("/{}", self.config.product_id)
    }

    /// 允许的浏览器 Origin（精确匹配）。
    pub fn public_origin(&self) -> &str {
        &self.config.public_origin
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let desk_api = Router::new()
        .route("/capabilities/", get(desk::capabilities))
        .route("/displays/", get(desk::displays))
        .route(
            "/sessions/",
            post(desk::create_session).get(desk::list_sessions),
        )
        .route(
            "/sessions/{session_id}/",
            get(desk::get_session).delete(desk::close_session),
        )
        .route(
            "/sessions/{session_id}/stream-tickets/",
            post(desk::create_ticket),
        )
        .route("/sessions/{session_id}/events/", any(desk::events_ws))
        .route("/sessions/{session_id}/stream/", any(desk::stream_ws))
        .route("/sessions/{session_id}/input/", post(desk::input))
        .route("/sessions/{session_id}/endpoint/", post(desk::endpoint))
        .route(
            "/sessions/{session_id}/clipboard/",
            get(desk::clipboard)
                .post(desk::clipboard)
                .layer(axum::extract::DefaultBodyLimit::max(
                    desk::MAX_CLIPBOARD_IMAGE_BYTES.div_ceil(3) * 4 + 4096,
                )),
        );

    Router::new()
        .nest(
            &format!("{}/webclient/desk/api/v2", state.product_prefix()),
            desk_api,
        )
        .with_state(state)
}
pub fn peer_addr(info: Option<&ConnectInfo<SocketAddr>>) -> Option<SocketAddr> {
    info.map(|ConnectInfo(addr)| *addr)
}
// A peer that stops reading must not pin a stream subscription and its capture session.
pub(crate) async fn send_ws_message(
    socket: &mut axum::extract::ws::WebSocket,
    message: axum::extract::ws::Message,
) -> Result<(), ()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), socket.send(message))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}

#[cfg(test)]
#[tokio::test]
async fn websocket_write_deadline_releases_stalled_connection() {
    use axum::extract::ws::{Message, WebSocketUpgrade};
    use axum::{routing::get, Router};
    use std::time::Duration;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let done_tx = Arc::new(std::sync::Mutex::new(Some(done_tx)));
    let app = Router::new().route(
        "/",
        get(move |upgrade: WebSocketUpgrade| {
            let done_tx = done_tx.lock().unwrap().take().unwrap();
            async move {
                upgrade.on_upgrade(move |mut socket| async move {
                    // Fill the TCP receive window without relying on one frame's size.
                    let payload = axum::body::Bytes::from(vec![0; 1 << 20]);
                    let mut result = Ok(());
                    for _ in 0..256 {
                        result =
                            send_ws_message(&mut socket, Message::Binary(payload.clone())).await;
                        if result.is_err() {
                            break;
                        }
                    }
                    drop(socket);
                    let _ = done_tx.send(result);
                })
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (client, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(15), done_rx)
        .await
        .expect("stalled socket was not released")
        .unwrap();
    assert!(result.is_err());
    assert!(started.elapsed() >= Duration::from_secs(9));
    drop(client);
    server.abort();
}
