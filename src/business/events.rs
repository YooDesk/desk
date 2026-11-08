//! 类型化事件：`session.snapshot` / `session.state` / `session.closed` 等。
//!
//! 格式：`{v:2,type,event_id,business_session_id,payload}`，event_id 为单调递增十进制字符串。
//! 每个订阅者一个有界通道；写满后慢消费者被断开而不是无限缓冲（见 RULES 的开发纪律）。
//!
//! 设计上 events 首包必须是完整 snapshot，且带 `state_revision`；旧状态不能覆盖新状态。
//! 本模块保证：订阅时先发送当前快照，之后只发送更大 revision 的状态变化。

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

use super::model::{SessionInfo, SessionState, SessionStateKind, TargetAuthentication};
use crate::json::codes;

/// 单订阅者事件通道容量。
///
/// 语义：状态事件合并，实际需要的队列很短；16 足够覆盖一次性突发。
pub const EVENT_CHANNEL_CAPACITY: usize = 16;

/// 事件主体。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// 完整会话快照（WS 首包）。
    #[serde(rename = "session.snapshot")]
    Snapshot {
        /// 会话快照。
        payload: SessionInfo,
    },
    /// 状态变化。
    #[serde(rename = "session.state")]
    State {
        /// 状态子集。
        payload: SessionState,
    },
    /// 会话已关闭。
    #[serde(rename = "session.closed")]
    Closed {
        /// 关闭原因（稳定码）。
        payload: ClosedPayload,
    },
    /// 访问已续期。
    #[serde(rename = "access.renewed")]
    AccessRenewed {
        /// 新的截止信息。
        payload: RenewedPayload,
    },
}

/// 关闭事件载荷。
#[derive(Debug, Clone, Serialize)]
pub struct ClosedPayload {
    /// 稳定原因码。
    pub reason: String,
}

/// 续期事件载荷，字段与 `business-v2` 的 `access.renewed` 一致。
#[derive(Debug, Clone, Serialize)]
pub struct RenewedPayload {
    /// 租约版本（本地访问使用单调递增字符串）。
    pub lease_version: String,
    /// 新到期时间。
    #[serde(serialize_with = "crate::timefmt::system_time_as_rfc3339")]
    pub expires_at: std::time::SystemTime,
    /// 关联的状态版本。
    pub state_revision: String,
}

/// 事件信封。
#[derive(Debug, Clone, Serialize)]
pub struct EventEnvelope {
    /// 协议版本。
    pub v: u8,
    /// 事件类型与载荷。
    #[serde(flatten)]
    pub kind: EventKind,
    /// 事件 id，单调递增十进制字符串。
    pub event_id: String,
    /// 业务会话 id。
    pub business_session_id: uuid::Uuid,
}

impl EventEnvelope {
    /// 构造事件信封。
    pub fn new(business_session_id: uuid::Uuid, event_id: u64, kind: EventKind) -> Self {
        Self {
            v: 2,
            kind,
            event_id: event_id.to_string(),
            business_session_id,
        }
    }
}

/// 事件总线：广播给当前订阅者。
///
/// 使用 `broadcast` 而不是逐订阅者队列：状态事件会被合并，慢消费者收到 `Lagged` 时
/// 由调用方断开连接并让浏览器重连获取新快照，避免旧事件覆盖新状态。
#[derive(Debug)]
pub struct EventBus {
    sender: broadcast::Sender<Arc<EventEnvelope>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(EVENT_CHANNEL_CAPACITY)
    }
}

impl EventBus {
    /// 按容量创建事件总线。
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    /// 订阅；返回的接收者只看到订阅之后的事件，快照由调用方单独发送。
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<EventEnvelope>> {
        self.sender.subscribe()
    }

    /// 发布事件；无订阅者时静默丢弃（不是错误）。
    pub fn publish(&self, event: EventEnvelope) {
        let _ = self.sender.send(Arc::new(event));
    }

    /// 当前订阅者数量，用于测试与观测。
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

/// 订阅端需要的会话状态来源。
pub trait SnapshotSource: Send + Sync {
    /// 当前快照；会话已删除时返回 `None`。
    fn snapshot(&self) -> Option<SessionInfo>;
    /// 当前状态与 revision。
    fn state(&self) -> Option<(SessionStateKind, u64, bool, TargetAuthentication)>;
}

/// 订阅时发送首包快照，并在快照缺失时返回明确错误。
///
/// 返回 `(snapshot, revision)`；调用方必须保证后续事件 revision 更大。
pub fn initial_snapshot(
    source: &dyn SnapshotSource,
) -> Result<(SessionInfo, u64), crate::json::ApiError> {
    let snapshot = source
        .snapshot()
        .ok_or_else(|| crate::json::ApiError::new(404, codes::NOT_FOUND, "session not found"))?;
    let revision = source
        .state()
        .map(|(_, revision, _, _)| revision)
        .unwrap_or(0);
    Ok((snapshot, revision))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_envelope_matches_contract() {
        let event = EventEnvelope::new(
            uuid::Uuid::nil(),
            7,
            EventKind::Closed {
                payload: ClosedPayload {
                    reason: "client_closed".into(),
                },
            },
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["v"], 2);
        assert_eq!(value["type"], "session.closed");
        assert_eq!(value["event_id"], "7");
        assert_eq!(value["business_session_id"], uuid::Uuid::nil().to_string());
        assert_eq!(value["payload"]["reason"], "client_closed");
    }

    #[test]
    fn event_bus_delivers_to_subscribers_only_after_subscribe() {
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe();
        bus.publish(EventEnvelope::new(
            uuid::Uuid::nil(),
            1,
            EventKind::Closed {
                payload: ClosedPayload {
                    reason: "test".into(),
                },
            },
        ));
        let received = rx.try_recv().expect("event delivered");
        assert_eq!(received.event_id, "1");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn event_bus_reports_lag_instead_of_unbounded_growth() {
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        for id in 1..=5 {
            bus.publish(EventEnvelope::new(
                uuid::Uuid::nil(),
                id,
                EventKind::Closed {
                    payload: ClosedPayload {
                        reason: "test".into(),
                    },
                },
            ));
        }
        // 容量 2，慢消费者应看到 Lagged，而不是无限缓冲。
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
    }
}
