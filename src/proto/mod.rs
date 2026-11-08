//! 冻结快照生成的 RustDesk 端侧消息类型。
//!
//! 来源：仓库内 `protocol/rustdesk/message-v1.proto`（由 `protocol/sources.json` 固定上游提交与摘要）。
//! 这里只使用端侧消息定义，不引入 RustDesk 服务端、API Server Pro 或任何 UI。
//!
//! profile 名称：`rustdesk-message.v1`。WS 每个 binary 帧恰好是一个 `hbb.Message`，
//! 不叠加 RustDesk 原生 TCP `BytesCodec` 长度前缀或加密握手。

include!(concat!(env!("OUT_DIR"), "/protos/mod.rs"));

pub use message_v1::*;

/// 端侧消息 profile 标识，用于能力响应与日志。
pub const PROFILE: &str = "rustdesk-message.v1";

/// 媒体互通候选规范名（见 10-revision-v2 §7）。
pub const MEDIA_PROFILE: &str = "desk.media.h264.annexb-ms.v1";
