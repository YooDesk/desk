//! 同主体的双向 HTTP 聊天。历史上限参照 UniRemoteLite，身份与生命周期使用本产品规则。
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use serde_json::{json, Value};

use crate::json::{codes, ApiError};

/// 每个主体只保留最近两百条消息。
pub const MAX_MESSAGES: usize = 200;
/// 文本限制按 UTF-8 字节执行，兼容浏览器和服务端。
pub const MAX_TEXT_BYTES: usize = 4096;

/// sender 由可信入口决定，客户端不能伪造本机发言。
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    /// 房间内单调序号，用于游标和去重。
    pub sequence: u64,
    /// 调用方生成的幂等 UUID。
    pub message_id: uuid::Uuid,
    /// 可信入口标记 viewer 或 host。
    pub sender: &'static str,
    /// 纯文本，不解析 HTML。
    pub text: String,
    /// 服务端生成的 UTC 时间。
    pub timestamp: String,
}

struct History {
    messages: VecDeque<ChatMessage>,
    sequence: u64,
    recent: VecDeque<Instant>,
}

/// 同主体的业务重连共享历史；其他身份获得独立房间。
pub struct ChatRoom {
    history: Mutex<History>,
    expiry: Mutex<SystemTime>,
    closed: AtomicBool,
    members: Mutex<std::collections::HashMap<u64, SystemTime>>,
}

impl ChatRoom {
    /// 建立有界历史与访问截止时间。
    pub fn new(expiry: SystemTime) -> Arc<Self> {
        Arc::new(Self {
            history: Mutex::new(History {
                messages: VecDeque::new(),
                sequence: 0,
                recent: VecDeque::new(),
            }),
            expiry: Mutex::new(expiry),
            closed: AtomicBool::new(false),
            members: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// 显式撤销或真实时间到期后，管理页也不再允许发言。
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
            || SystemTime::now() >= *self.expiry.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 最后一位退出时清除历史，不影响其他主体的聊天。
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .messages
            .clear();
    }

    /// 同主体的管理页和 viewer 共用聊天；访问续期不改变历史。
    pub fn join(&self, access_id: u64, expiry: SystemTime) {
        let mut members = self.members.lock().unwrap_or_else(|e| e.into_inner());
        members.retain(|_, time| *time > SystemTime::now());
        members.insert(access_id, expiry);
        *self.expiry.lock().unwrap_or_else(|e| e.into_inner()) = *members.values().max().unwrap();
    }

    /// 某个登录退出不影响其他在线参与者；最后一位退出时清除房间。
    pub fn leave(&self, access_id: u64) {
        let mut members = self.members.lock().unwrap_or_else(|e| e.into_inner());
        members.remove(&access_id);
        members.retain(|_, time| *time > SystemTime::now());
        if let Some(expiry) = members.values().max() {
            *self.expiry.lock().unwrap_or_else(|e| e.into_inner()) = *expiry;
        } else {
            self.close();
        }
    }

    /// 仅续期已经加入的登录，避免让其他主体加入当前房间。
    pub fn renew_member(&self, access_id: u64, expiry: SystemTime) {
        let mut members = self.members.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(member) = members.get_mut(&access_id) {
            *member = expiry;
            *self.expiry.lock().unwrap_or_else(|e| e.into_inner()) =
                *members.values().max().unwrap();
        }
    }

    /// 返回游标之后的有界快照，sequence 用于网页去重和界面刷新。
    pub fn read(&self, after: u64) -> Result<Value, ApiError> {
        if self.is_closed() {
            return Err(ApiError::new(409, codes::STATE_CONFLICT, "chat is closed"));
        }
        let history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        Ok(
            json!({"items": history.messages.iter().filter(|m| m.sequence > after).collect::<Vec<_>>(), "sequence": history.sequence}),
        )
    }

    /// UTF-8 字节限额和 NUL 检查先于历史写入，避免资源滥用。
    pub fn validate_text(text: &str) -> Result<(), ApiError> {
        if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES || text.contains('\0') {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "chat text must contain 1..4096 UTF-8 bytes without NUL",
            ));
        }
        Ok(())
    }

    /// 服务端生成 sender；同 message_id 同内容幂等，不同内容拒绝。每秒最多十条。
    pub fn append(
        &self,
        id: uuid::Uuid,
        sender: &'static str,
        text: &str,
    ) -> Result<ChatMessage, ApiError> {
        Self::validate_text(text)?;
        if self.is_closed() {
            return Err(ApiError::new(409, codes::STATE_CONFLICT, "chat is closed"));
        }
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_closed() {
            return Err(ApiError::new(409, codes::STATE_CONFLICT, "chat is closed"));
        }
        if let Some(existing) = history.messages.iter().find(|m| m.message_id == id) {
            if existing.sender == sender && existing.text == text {
                return Ok(existing.clone());
            }
            return Err(ApiError::new(
                409,
                codes::STATE_CONFLICT,
                "chat message id conflicts",
            ));
        }
        let now = Instant::now();
        while history
            .recent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(1))
        {
            history.recent.pop_front();
        }
        if history.recent.len() >= 10 {
            return Err(ApiError::new(
                429,
                codes::CAPACITY_EXCEEDED,
                "chat rate limit",
            ));
        }
        history.recent.push_back(now);
        history.sequence += 1;
        let message = ChatMessage {
            sequence: history.sequence,
            message_id: id,
            sender,
            text: text.to_owned(),
            timestamp: crate::timefmt::format_rfc3339(SystemTime::now()),
        };
        history.messages.push_back(message.clone());
        if history.messages.len() > MAX_MESSAGES {
            history.messages.pop_front();
        }
        Ok(message)
    }

    /// 网页操作与既有 endpoint 认证/CSRF 共用，不引入匿名聊天通道。
    pub fn endpoint(self: &Arc<Self>, value: &Value, read_only: bool) -> Result<Value, ApiError> {
        match value["action"].as_str() {
            Some("chat.read") => {
                let after = value["after"]
                    .as_u64()
                    .filter(|n| *n <= 9_007_199_254_740_991)
                    .ok_or_else(|| {
                        ApiError::new(400, codes::INVALID_REQUEST, "invalid chat cursor")
                    })?;
                self.read(after)
            }
            Some("chat.send") => {
                if read_only {
                    return Err(ApiError::new(403, codes::FORBIDDEN, "session is view-only"));
                }
                let id = value["message_id"]
                    .as_str()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .ok_or_else(|| {
                        ApiError::new(400, codes::INVALID_REQUEST, "invalid chat message id")
                    })?;
                let text = value["text"].as_str().ok_or_else(|| {
                    ApiError::new(400, codes::INVALID_REQUEST, "missing chat text")
                })?;
                Self::validate_text(text)?;
                Ok(json!({"message": self.append(id, "viewer", text)?}))
            }
            _ => Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "unknown chat action",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 幂等重试不能产生重复发言，也不能借相同 id 修改历史。
    #[test]
    fn deduplication_conflicts_and_identity_isolation() {
        let room = ChatRoom::new(SystemTime::now() + Duration::from_secs(60));
        let other = ChatRoom::new(SystemTime::now() + Duration::from_secs(60));
        let id = uuid::Uuid::new_v4();
        assert_eq!(room.append(id, "viewer", "你好").unwrap().sequence, 1);
        assert_eq!(room.append(id, "viewer", "你好").unwrap().sequence, 1);
        assert_eq!(room.append(id, "host", "你好").unwrap_err().status, 409);
        assert_eq!(other.read(0).unwrap()["items"].as_array().unwrap().len(), 0);
        room.close();
        assert!(room.read(0).is_err());
        assert!(room.append(uuid::Uuid::new_v4(), "host", "late").is_err());
    }

    /// 无效文本、游标与只看发送在写入历史前拒绝。
    #[test]
    fn validates_bytes_cursor_and_readonly() {
        let room = ChatRoom::new(SystemTime::now() + Duration::from_secs(60));
        for text in ["", "  ", "a\0b", &"中".repeat(1366)] {
            assert!(ChatRoom::validate_text(text).is_err());
        }
        assert!(room
            .endpoint(&json!({"action":"chat.read","after":-1}), false)
            .is_err());
        assert_eq!(
            room.endpoint(&json!({"action":"chat.send"}), true)
                .unwrap_err()
                .status,
            403
        );
        for _ in 0..10 {
            room.append(uuid::Uuid::new_v4(), "host", "test").unwrap();
        }
        assert_eq!(
            room.append(uuid::Uuid::new_v4(), "viewer", "test")
                .unwrap_err()
                .status,
            429
        );
    }
}
