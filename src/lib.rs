//! YooDesk WebService：独立的局域网业务进程与 desk 被控端接入。
//!
//! 设计入口见 `docs/design/04-webservice.md` 与 `docs/design/10-revision-v2.md`。
//! 本 crate 不验证 Gateway JWT、不连接云端账号库，没有 Gateway/Connector 也能独立提供本机服务。

#![warn(missing_docs)]

pub mod access;
pub mod business;
pub mod config;
pub mod connections;
pub mod desk;
pub mod http;
pub mod json;
pub mod proto;
pub mod timefmt;
pub mod tls_policy;
#[cfg(all(target_os = "linux", feature = "desk-engine"))]
pub mod unix_worker;
#[cfg(all(windows, feature = "desk-engine"))]
pub mod windows_worker;

#[allow(dead_code)]
#[path = "../protocol/rust/backend_context.rs"]
mod backend_context;

#[cfg(target_os = "linux")]
mod linux_uinput_access;
#[cfg(target_os = "linux")]
mod linux_worker_host;
#[cfg(target_os = "linux")]
mod linux_x11_probe;
mod plugin_abi;
mod plugin_runtime;
mod remote_access;
#[cfg(windows)]
mod windows_worker_host;
