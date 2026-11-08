//! 业务会话：WebService v2 的会话状态机、事件流与 desk 数据流。
//!
//! 契约来源：`protocol/webservice-v2.openapi.json`、`protocol/business-v2.schema.json`
//! 与 `docs/design/04-webservice.md` §3。
//!
//! desk与原生VNC/SSH独立管理，SFTP复用SSH连接；未接入能力不宣告支持。
//! 会话按访问身份与能力隔离，跨访问会话读取返回`not_found`。

pub mod chat;
pub mod desk_session;
pub mod events;
pub mod model;
pub mod remote {
    #[derive(Default)]
    pub struct RemoteHub;
    impl RemoteHub {
        pub fn close_owner(&self, _: u64) {}
        pub fn close_all(&self) {}
    }
}

pub use desk_session::{DeskSession, DeskSessionManager};
pub use model::{
    CapabilitiesData, SessionInfo, SessionState, SessionStateKind, TargetAuthentication,
};

use std::sync::Arc;

/// 会话管理器集合：按能力路由。
///
/// 目前只装配 desk；其他能力在路由层明确拒绝而不是返回空实现。
pub struct BusinessHub {
    /// VNC/SSH 的独立连接集合。
    pub remote: remote::RemoteHub,
    /// desk 会话管理器。
    pub desk: Arc<DeskSessionManager>,
}

impl BusinessHub {
    /// 用给定后端构建业务集合。
    pub fn new(backend: Arc<dyn crate::desk::Backend>) -> Self {
        Self {
            remote: Default::default(),
            desk: Arc::new(DeskSessionManager::new(backend)),
        }
    }

    /// 进程配置必须进入实际编码器和会话限额，不能只用于 HTTP 展示。
    pub fn with_config(
        backend: Arc<dyn crate::desk::Backend>,
        config: &crate::config::Config,
    ) -> Self {
        Self {
            remote: Default::default(),
            desk: Arc::new(DeskSessionManager::with_settings(
                backend,
                crate::desk::encode::EncoderSettings {
                    fps: config.fps,
                    bitrate_bps: config.bitrate_bps,
                    ..Default::default()
                },
                config.max_sessions,
                &config.product_id,
            )),
        }
    }
}
