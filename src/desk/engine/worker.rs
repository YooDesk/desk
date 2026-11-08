//! Shared bounded desktop-worker codec and native dispatch.
use super::{rpc::Reply, EngineBackend};
use crate::desk::{Backend, InputSink};
use crate::json::{codes, ApiError};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    time::Duration,
};
const LIMIT: usize = 24 << 20;
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn engine_error() -> ApiError {
    ApiError::new(503, codes::DESK_ENGINE, "desktop worker operation failed")
}
pub(crate) fn write_frame<S: Read + Write>(
    file: &mut S,
    metadata: &Value,
    bytes: &[u8],
) -> io::Result<()> {
    let header = serde_json::to_vec(metadata).map_err(|_| invalid("IPC header"))?;
    if header.len() > 4096 || bytes.len() > LIMIT {
        return Err(invalid("IPC frame limit"));
    }
    file.write_all(&(header.len() as u32).to_le_bytes())?;
    file.write_all(&(bytes.len() as u32).to_le_bytes())?;
    file.write_all(&header)?;
    file.write_all(bytes)
}
pub(crate) fn read_frame<S: Read + Write>(file: &mut S) -> io::Result<(Value, Vec<u8>)> {
    let mut lengths = [0; 8];
    file.read_exact(&mut lengths)?;
    let header = u32::from_le_bytes(lengths[..4].try_into().unwrap()) as usize;
    let size = u32::from_le_bytes(lengths[4..].try_into().unwrap()) as usize;
    if header == 0 || header > 4096 || size > LIMIT {
        return Err(invalid("IPC frame limit"));
    }
    let mut metadata = vec![0; header];
    file.read_exact(&mut metadata)?;
    let mut bytes = vec![0; size];
    file.read_exact(&mut bytes)?;
    Ok((
        serde_json::from_slice(&metadata).map_err(|_| invalid("IPC header"))?,
        bytes,
    ))
}

pub(crate) fn serve<S: Read + Write>(file: &mut S) -> io::Result<()> {
    let engine = EngineBackend::new();
    let mut inputs: HashMap<u64, Box<dyn InputSink>> = HashMap::new();
    let mut next_input = 1u64;
    let mut previous = 0;
    let result = (|| loop {
        let (request, bytes) = read_frame(file)?;
        let id = request["id"]
            .as_u64()
            .ok_or_else(|| invalid("IPC sequence"))?;
        if request["v"] != 1 || id <= previous {
            return Err(invalid("IPC sequence"));
        }
        previous = id;
        let args: Value = serde_json::from_slice(&bytes).map_err(|_| invalid("IPC args"))?;
        let response = dispatch(
            &engine,
            request["op"].as_str().unwrap_or(""),
            args,
            &mut inputs,
            &mut next_input,
        );
        match response {
            Ok(reply) if reply.bytes.is_empty() => write_frame(
                file,
                &json!({"v":1,"id":id,"json":true}),
                &serde_json::to_vec(&reply.value).map_err(|_| invalid("IPC value"))?,
            )?,
            Ok(reply) => write_frame(
                file,
                &json!({"v":1,"id":id,"value":reply.value}),
                &reply.bytes,
            )?,
            Err(error) => write_frame(
                file,
                &json!({"v":1,"id":id,"error":true}),
                &serde_json::to_vec(&json!({"status":error.status,"message":error.message}))
                    .map_err(|_| invalid("IPC error"))?,
            )?,
        }
    })();
    for input in inputs.values_mut() {
        input.release_all();
    }
    let _ = engine.unsubscribe();
    result
}
fn dispatch(
    engine: &EngineBackend,
    op: &str,
    args: Value,
    inputs: &mut HashMap<u64, Box<dyn InputSink>>,
    next_input: &mut u64,
) -> Result<Reply, ApiError> {
    let conn = i32::try_from(args["conn"].as_i64().unwrap_or(0)).map_err(|_| engine_error())?;
    let mut value = Value::Null;
    let mut bytes = Vec::new();
    match op {
        "displays" => {
            value = serde_json::to_value(engine.displays()?).map_err(|_| engine_error())?
        }
        "capabilities" => {
            value = serde_json::to_value(engine.capabilities()).map_err(|_| engine_error())?
        }
        "subscribe" => engine.subscribe(
            args["display"].as_u64().ok_or_else(engine_error)? as u32,
            conn,
            crate::desk::encode::EncoderSettings {
                fps: args["fps"].as_u64().ok_or_else(engine_error)? as u32,
                bitrate_bps: args["bitrate"].as_u64().ok_or_else(engine_error)? as u32,
                ..Default::default()
            },
        )?,
        "unsubscribe" => engine.unsubscribe()?,
        "unsubscribe_conn" => engine.unsubscribe_conn(conn)?,
        "replay_keyframe" => engine.replay_keyframe(conn)?,
        "replay_cursor" => engine.replay_cursor(conn)?,
        "ack_video" => engine.ack_video(conn, args["pts"].as_i64().ok_or_else(engine_error)?)?,
        "refresh_video" => engine.refresh_video(conn)?,
        "poll" => {
            // 共享IPC还服务输入、剪贴板和解码ACK，不能在锁内等待下一帧。
            if let Some((frame, key, pts)) = engine.poll(conn, Duration::ZERO)? {
                bytes = frame;
                value = json!({"present":true,"key":key,"pts":pts});
            }
        }
        "poll_cursor" => {
            if let Some((cursor, shape)) = engine.poll_cursor(conn)? {
                bytes = cursor;
                value = json!({"present":true,"shape":shape});
            }
        }
        "poll_audio" => {
            if let Some(audio) = engine.poll_audio(conn, args["replay"] == true)? {
                bytes = audio;
                value = json!({"present":true});
            }
        }
        "input.create" => {
            if inputs.len() >= 64 {
                return Err(ApiError::new(
                    429,
                    codes::CAPACITY_EXCEEDED,
                    "desktop input limit",
                ));
            }
            let id = *next_input;
            *next_input += 1;
            inputs.insert(id, engine.create_input()?);
            value = json!(id);
        }
        "input.mouse" | "input.key" | "input.release" | "input.drop" => {
            let id = args["id"].as_u64().ok_or_else(engine_error)?;
            let input = inputs.get_mut(&id).ok_or_else(engine_error)?;
            if op == "input.mouse" {
                input.mouse(
                    serde_json::from_value(args["event"].clone()).map_err(|_| engine_error())?,
                )?;
            }
            if op == "input.key" {
                input.key(
                    serde_json::from_value(args["event"].clone()).map_err(|_| engine_error())?,
                )?;
            }
            if op == "input.release" || op == "input.drop" {
                input.release_all();
            }
            if op == "input.drop" {
                inputs.remove(&id);
            }
        }
        "endpoint" => {
            let action = args["value"]["action"].as_str().ok_or_else(engine_error)?;
            if action != "capabilities"
                && !action.starts_with("clipboard.")
                && !action.starts_with("privacy.")
                && !(cfg!(target_os = "linux") && action.starts_with("files."))
            {
                return Err(ApiError::new(
                    403,
                    codes::FORBIDDEN,
                    "operation belongs to service parent",
                ));
            }
            #[cfg(windows)]
            if matches!(action, "clipboard.read" | "clipboard.write") {
                crate::windows_worker::require_clipboard_desktop()?;
            }
            value = engine.endpoint(conn, &args["value"])?;
        }
        _ => {
            return Err(ApiError::new(
                400,
                codes::INVALID_REQUEST,
                "unknown desktop IPC operation",
            ))
        }
    }
    Ok(Reply { value, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    struct ObservedInput(Arc<Mutex<Vec<&'static str>>>);
    impl InputSink for ObservedInput {
        fn mouse(&mut self, _: crate::desk::input::MouseInput) -> Result<(), ApiError> {
            self.0.lock().unwrap().push("mouse");
            Ok(())
        }
        fn key(&mut self, _: crate::desk::input::KeyInput) -> Result<(), ApiError> {
            self.0.lock().unwrap().push("key");
            Ok(())
        }
        fn release_all(&mut self) {
            self.0.lock().unwrap().push("release");
        }
    }
    #[test]
    fn input_release_preserves_handle_and_drop_returns_capacity() {
        let engine = EngineBackend;
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut inputs: HashMap<u64, Box<dyn InputSink>> = HashMap::new();
        inputs.insert(1, Box::new(ObservedInput(events.clone())));
        let mut next = 2;
        dispatch(
            &engine,
            "input.release",
            json!({"id":1}),
            &mut inputs,
            &mut next,
        )
        .unwrap();
        dispatch(
            &engine,
            "input.key",
            json!({"id":1,"event":{"Text":"after release"}}),
            &mut inputs,
            &mut next,
        )
        .unwrap();
        dispatch(
            &engine,
            "input.mouse",
            json!({"id":1,"event":{"Move":{"x":0,"y":0}}}),
            &mut inputs,
            &mut next,
        )
        .unwrap();
        dispatch(
            &engine,
            "input.drop",
            json!({"id":1}),
            &mut inputs,
            &mut next,
        )
        .unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec!["release", "key", "mouse", "release"]
        );
        assert!(inputs.is_empty());
        assert!(dispatch(
            &engine,
            "input.key",
            json!({"id":1,"event":{"Text":"closed"}}),
            &mut inputs,
            &mut next
        )
        .is_err());
        // 超过一代64句柄额度的反复建立/释放/销毁，验证普通会话关闭不会累积耗尽额度。
        for _ in 0..128 {
            let id = dispatch(&engine, "input.create", json!({}), &mut inputs, &mut next)
                .unwrap()
                .value;
            dispatch(
                &engine,
                "input.release",
                json!({"id":id}),
                &mut inputs,
                &mut next,
            )
            .unwrap();
            dispatch(
                &engine,
                "input.drop",
                json!({"id":id}),
                &mut inputs,
                &mut next,
            )
            .unwrap();
        }
        assert!(inputs.is_empty());
    }
    #[test]
    fn codec_preserves_full_endpoint_payload_and_rejects_untrusted_lengths() {
        let payload = vec![0x5a; 24 << 20];
        let mut cursor = io::Cursor::new(Vec::new());
        write_frame(&mut cursor, &json!({"v":1,"id":7}), &payload).unwrap();
        cursor.set_position(0);
        let (metadata, actual) = read_frame(&mut cursor).unwrap();
        assert_eq!(metadata["id"], 7);
        assert_eq!(actual, payload);
        let mut malformed = Vec::new();
        malformed.extend_from_slice(&2u32.to_le_bytes());
        malformed.extend_from_slice(&((24u32 << 20) + 1).to_le_bytes());
        assert_eq!(
            read_frame(&mut io::Cursor::new(malformed))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let mut malformed = Vec::new();
        malformed.extend_from_slice(&4097u32.to_le_bytes());
        malformed.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(
            read_frame(&mut io::Cursor::new(malformed))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
