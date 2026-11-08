//! 业务 DTO：与 `protocol/business-v2.schema.json` 逐字段对齐。
//!
//! 这些结构是跨端契约的一部分，字段增减必须同时更新 Schema 与向量；
//! 序列化使用 `deny_unknown_fields` 之外的显式 `#[serde]`，避免拼写漂移。

use serde::{Deserialize, Serialize};

/// 会话状态枚举，取值与 `SessionState` 定义一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStateKind {
    /// 已创建，尚未开始连接。
    Created,
    /// 正在建立采集/编码通道。
    Connecting,
    /// 传输就绪，可打开数据流。
    TransportReady,
    /// 正在对目标侧认证（desk 不适用，保留枚举）。
    Authenticating,
    /// 等待目标侧挑战（desk 不适用，保留枚举）。
    WaitingChallenge,
    /// 数据连接运行中。
    Active,
    /// 正在关闭。
    Closing,
    /// 已关闭。
    Closed,
    /// 失败终态。
    Failed,
}

impl SessionStateKind {
    /// 是否终态。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Failed)
    }

    /// 字符串形式，用于事件 DTO。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Connecting => "connecting",
            Self::TransportReady => "transport_ready",
            Self::Authenticating => "authenticating",
            Self::WaitingChallenge => "waiting_challenge",
            Self::Active => "active",
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::Failed => "failed",
        }
    }
}

/// 目标侧认证状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetAuthentication {
    /// 无法由服务端确认。
    Unknown,
    /// 正在认证。
    Pending,
    /// 已确认。
    Verified,
    /// 失败。
    Failed,
}

impl TargetAuthentication {
    /// 字符串形式。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Pending => "pending",
            Self::Verified => "verified",
            Self::Failed => "failed",
        }
    }
}

/// `SessionInfo`：GET/WS 观察到的权威会话快照。
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    /// 业务会话 id（UUID），不是 Gateway 的 remote session_id。
    pub business_session_id: uuid::Uuid,
    /// 能力名。
    pub capability: &'static str,
    /// 目标 id。
    pub target_id: String,
    /// 状态。
    pub state: SessionStateKind,
    /// 状态版本，单调递增的十进制字符串。
    pub state_revision: String,
    /// 是否可打开 stream。
    pub stream_available: bool,
    /// 目标认证状态。
    pub target_authentication: TargetAuthentication,
    /// 原生 stream profile。
    pub stream_profile: &'static str,
    /// 会话实际支持的功能。
    pub features: Vec<String>,
    /// events WS 地址。
    pub event_url: String,
    /// stream WS 地址；当前不可用时为 `null`。
    pub stream_url: Option<String>,
    /// 访问模式。
    pub access_mode: &'static str,
    /// 访问截止时间。
    #[serde(serialize_with = "crate::timefmt::system_time_as_rfc3339")]
    pub access_expires_at: std::time::SystemTime,
}

/// 会话状态子集，用于 `session.state` 事件。
#[derive(Debug, Clone, Serialize)]
pub struct SessionState {
    /// 状态。
    pub state: SessionStateKind,
    /// 状态版本。
    pub state_revision: String,
    /// 是否可打开 stream。
    pub stream_available: bool,
    /// 目标认证状态。
    pub target_authentication: TargetAuthentication,
}

/// `GET capabilities/` 的 `data`。
#[derive(Debug, Clone, Serialize)]
pub struct CapabilitiesData {
    /// 协议版本，固定 2。
    pub protocol_version: u8,
    /// 真实可用的功能标识。
    pub supported_features: Vec<String>,
    /// 端点上限（整数）。
    pub limits: std::collections::BTreeMap<String, u64>,
    /// 原生 stream profile。
    pub stream_profile: &'static str,
    /// 媒体状态：`not_applicable`/`provisional`/`verified`。
    ///
    /// desk 在 T02 实测前保持 `provisional`，不宣称媒体互通已验证。
    pub media_status: &'static str,
    /// 被控端后端能力（跨平台可读，前端据此裁剪 UI）。
    pub capabilities: crate::desk::DeskCapabilities,
    /// 平台与采集方式。
    pub platform: crate::desk::PlatformInfo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_serialize_snake_case() {
        let json = serde_json::to_string(&SessionStateKind::TransportReady).unwrap();
        assert_eq!(json, "\"transport_ready\"");
        assert!(SessionStateKind::Closed.is_terminal());
        assert!(!SessionStateKind::Active.is_terminal());
    }

    #[test]
    fn session_info_matches_contract_shape() {
        let info = SessionInfo {
            business_session_id: uuid::Uuid::nil(),
            capability: "desk",
            target_id: "local".into(),
            state: SessionStateKind::Active,
            state_revision: "1".into(),
            stream_available: true,
            target_authentication: TargetAuthentication::Unknown,
            stream_profile: "rustdesk-message.v1",
            features: vec!["desk.capture".into()],
            event_url: "/yoodesk/webclient/desk/api/v2/sessions/x/events/".into(),
            stream_url: Some("/yoodesk/webclient/desk/api/v2/sessions/x/stream/".into()),
            access_mode: "local",
            access_expires_at: std::time::SystemTime::UNIX_EPOCH,
        };
        let value = serde_json::to_value(info).unwrap();
        for key in [
            "business_session_id",
            "capability",
            "target_id",
            "state",
            "state_revision",
            "stream_available",
            "target_authentication",
            "stream_profile",
            "features",
            "event_url",
            "stream_url",
            "access_mode",
            "access_expires_at",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        assert_eq!(value["business_session_id"], uuid::Uuid::nil().to_string());
        assert_eq!(value["state"], "active");
    }
}
