//! RemoteDesktop Clipboard v1适配；只在用户授予同一Portal会话的剪贴板权限后工作。
use super::wayland_portal::Session;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use dbus::{
    arg::{OwnedFd, PropMap, RefArg, Variant},
    blocking::SyncConnection,
    message::MatchRule,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
const INTERFACE: &str = "org.freedesktop.portal.Clipboard";
const MAX: usize = 16 << 20;
type Formats = BTreeMap<String, Arc<Vec<u8>>>;
#[derive(Default)]
struct Clipboard {
    mimes: Vec<String>,
    owner: bool,
    formats: Formats,
    value: Option<Value>,
    generation: u64,
}
static CLIPBOARD: Mutex<Clipboard> = Mutex::new(Clipboard {
    mimes: Vec::new(),
    owner: false,
    formats: BTreeMap::new(),
    value: None,
    generation: 0,
});
static TRANSFERS: AtomicUsize = AtomicUsize::new(0);

/// 标准vardict动态参数，不把剪贴板数据输出到日志。
fn option<T: RefArg + 'static>(name: &str, value: T) -> PropMap {
    [(name.into(), Variant(Box::new(value) as Box<dyn RefArg>))]
        .into_iter()
        .collect()
}
/// 标准as及GNOME实际返回的(as)都只展开一层；不把任意嵌套数据当作MIME列表。
fn mime_types(value: &dyn RefArg) -> Vec<String> {
    let values = if value.arg_type() == dbus::arg::ArgType::Struct {
        let Some(mut fields) = value.as_iter() else {
            return Vec::new();
        };
        let Some(array) = fields.next() else {
            return Vec::new();
        };
        if fields.next().is_some() {
            return Vec::new();
        }
        array.as_iter()
    } else {
        value.as_iter()
    };
    values
        .map(|it| it.filter_map(|m| m.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}
/// 在Start前安装信号，避免漏掉初始SelectionOwnerChanged；精确校验sender及session。
pub(super) fn listen(
    conn: Arc<SyncConnection>,
    path: dbus::Path<'static>,
    owner: &str,
) -> Result<(), String> {
    let expected = path.clone();
    let sender = owner.to_owned();
    conn.add_match::<(dbus::Path<'static>, PropMap), _>(
        MatchRule::new_signal(INTERFACE, "SelectionOwnerChanged"),
        move |(session, options), _, message| {
            if session != expected || message.sender().is_none_or(|s| s.to_string() != sender) {
                return true;
            }
            let mimes = options
                .get("mime_types")
                .map(|v| mime_types(&*v.0))
                .unwrap_or_default();
            let is_owner = options
                .get("session_is_owner")
                .and_then(|v| v.0.as_u64())
                .is_some_and(|n| n != 0);
            let mut state = CLIPBOARD.lock().unwrap();
            state.mimes = mimes;
            state.owner = is_owner;
            if !is_owner {
                state.generation = state.generation.wrapping_add(1);
                state.formats.clear();
                state.value = None;
            }
            true
        },
    )
    .map_err(|_| "cannot subscribe to Portal clipboard owner")?;
    let expected = path.clone();
    let sender = owner.to_owned();
    let worker_conn = conn.clone();
    conn.add_match::<(dbus::Path<'static>, String, u32), _>(
        MatchRule::new_signal(INTERFACE, "SelectionTransfer"),
        move |(session, mime, serial), _, message| {
            if session != expected || message.sender().is_none_or(|s| s.to_string() != sender) {
                return true;
            }
            let payload = CLIPBOARD.lock().unwrap().formats.get(&mime).cloned();
            if TRANSFERS.fetch_add(1, Ordering::AcqRel) >= 4 {
                TRANSFERS.fetch_sub(1, Ordering::AcqRel);
                complete(&worker_conn, &expected, serial, false);
                return true;
            }
            let conn = worker_conn.clone();
            let path = expected.clone();
            let spawned = std::thread::Builder::new()
                .name("portal-selection-write".into())
                .spawn(move || {
                    let ok = payload
                        .is_some_and(|data| selection_write(&conn, &path, serial, &data).is_ok());
                    {
                        let _io = super::wayland_portal::IO.lock().unwrap();
                        complete(&conn, &path, serial, ok);
                    }
                    TRANSFERS.fetch_sub(1, Ordering::AcqRel);
                });
            if spawned.is_err() {
                TRANSFERS.fetch_sub(1, Ordering::AcqRel);
                complete(&worker_conn, &expected, serial, false);
            }
            true
        },
    )
    .map_err(|_| "cannot subscribe to Portal clipboard transfer")?;
    Ok(())
}
/// 每一个SelectionWrite都确认成功或失败，避免接收端无限等待。
fn complete(conn: &SyncConnection, path: &dbus::Path<'static>, serial: u32, ok: bool) {
    let result: Result<(), dbus::Error> = scrap::wayland::pipewire::get_portal(conn).method_call(
        INTERFACE,
        "SelectionWriteDone",
        (path.clone(), serial, ok),
    );
    if result.is_err() {
        log::warn!("Portal clipboard completion failed");
    }
}
/// 以有界非阻塞方式读写Portal提供的pipe，防止坏接收端卡死worker。
fn pipe(mut file: File, output: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let fd = file.as_raw_fd();
    unsafe {
        let flags = hbb_common::libc::fcntl(fd, hbb_common::libc::F_GETFL);
        if flags < 0
            || hbb_common::libc::fcntl(
                fd,
                hbb_common::libc::F_SETFL,
                flags | hbb_common::libc::O_NONBLOCK,
            ) < 0
        {
            return Err("cannot configure Portal clipboard pipe".into());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut data = Vec::new();
    let mut offset = 0;
    loop {
        if Instant::now() >= deadline {
            return Err("Portal clipboard transfer timed out".into());
        }
        let result = if let Some(bytes) = output {
            if offset == bytes.len() {
                return Ok(data);
            }
            file.write(&bytes[offset..])
        } else {
            let mut chunk = [0u8; 65536];
            match file.read(&mut chunk) {
                Ok(0) => return Ok(data),
                Ok(n) => {
                    if data.len() + n > 24 << 20 {
                        return Err("Portal clipboard payload too large".into());
                    }
                    data.extend_from_slice(&chunk[..n]);
                    Ok(n)
                }
                Err(e) => Err(e),
            }
        };
        match result {
            Ok(0) => return Err("Portal clipboard pipe closed".into()),
            Ok(n) => offset += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let mut item = hbb_common::libc::pollfd {
                    fd,
                    events: if output.is_some() {
                        hbb_common::libc::POLLOUT
                    } else {
                        hbb_common::libc::POLLIN
                    },
                    revents: 0,
                };
                if unsafe { hbb_common::libc::poll(&mut item, 1, 50) } < 0 {
                    return Err("Portal clipboard poll failed".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err("Portal clipboard pipe failed".into()),
        }
    }
}
/// 响应一次本会话的剪贴板传输，fd关闭后才发送完成通知。
fn selection_write(
    conn: &SyncConnection,
    path: &dbus::Path<'static>,
    serial: u32,
    data: &[u8],
) -> Result<(), String> {
    let (fd,): (OwnedFd,) = {
        let _io = super::wayland_portal::IO.lock().unwrap();
        scrap::wayland::pipewire::get_portal(conn)
            .method_call(INTERFACE, "SelectionWrite", (path.clone(), serial))
            .map_err(|_| "Portal SelectionWrite failed")?
    };
    pipe(unsafe { File::from_raw_fd(fd.into_fd()) }, Some(data))?;
    Ok(())
}
/// 将前端RGBA按既有16MiB限制编码为标准PNG报价。
fn encode(value: &Value) -> Result<Formats, String> {
    let mut formats = Formats::new();
    match value["kind"].as_str() {
        Some("text") => {
            let text = value["text"].as_str().ok_or("text required")?;
            if text.len() > MAX {
                return Err("clipboard text too large".into());
            }
            let bytes = Arc::new(text.as_bytes().to_vec());
            for mime in ["text/plain;charset=utf-8", "text/plain"] {
                formats.insert(mime.into(), bytes.clone());
            }
        }
        Some("image") => {
            let w = value["width"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= 16384)
                .ok_or("invalid clipboard width")? as u32;
            let h = value["height"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= 16384)
                .ok_or("invalid clipboard height")? as u32;
            let count = (w as usize)
                .checked_mul(h as usize)
                .and_then(|n| n.checked_mul(4))
                .filter(|n| *n <= MAX)
                .ok_or("clipboard image too large")?;
            let encoded = value["data"]
                .as_str()
                .filter(|s| s.len() <= MAX * 4 / 3 + 4)
                .ok_or("clipboard image data required")?;
            let pixels = STANDARD
                .decode(encoded)
                .map_err(|_| "invalid clipboard image encoding")?;
            if pixels.len() != count {
                return Err("invalid clipboard image size".into());
            }
            let mut bytes = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut bytes, w, h);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().map_err(|_| "PNG header failed")?;
                writer
                    .write_image_data(&pixels)
                    .map_err(|_| "PNG encode failed")?;
            }
            formats.insert("image/png".into(), Arc::new(bytes));
        }
        _ => return Err("invalid clipboard kind".into()),
    }
    Ok(formats)
}
/// 将经Portal读取的PNG恢复为前端既有RGBA DTO，限制展开后的图像尺寸。
fn decode_png(bytes: &[u8]) -> Result<Value, String> {
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    decoder.set_limits(png::Limits { bytes: MAX });
    let mut reader = decoder.read_info().map_err(|_| "invalid clipboard PNG")?;
    let info = reader.info();
    let count = (info.width as usize)
        .checked_mul(info.height as usize)
        .and_then(|n| n.checked_mul(4))
        .filter(|n| *n <= MAX)
        .ok_or("clipboard PNG too large")?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|_| "PNG decode failed")?;
    let mut rgba = Vec::with_capacity(count);
    match info.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(&buffer[..info.buffer_size()]),
        png::ColorType::Rgb => {
            for p in buffer[..info.buffer_size()].chunks_exact(3) {
                rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            for p in &buffer[..info.buffer_size()] {
                rgba.extend_from_slice(&[*p, *p, *p, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for p in buffer[..info.buffer_size()].chunks_exact(2) {
                rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
        }
        _ => return Err("unsupported clipboard PNG color".into()),
    }
    Ok(json!({"kind":"image","width":info.width,"height":info.height,"data":STANDARD.encode(rgba)}))
}
/// Portal权限拒绝时明确403，绝不偷偷转到X11读取用户未授权的剪贴板。
pub(super) fn endpoint(session: &Session, value: &Value) -> Result<Value, (u16, String)> {
    if !session.clipboard {
        return Err((403, "Wayland clipboard permission was not granted".into()));
    }
    let proxy = scrap::wayland::pipewire::get_portal(&session.conn);
    if value["action"] == "clipboard.write" {
        let formats = encode(&value["content"]).map_err(|e| (400, e))?;
        let mimes = formats.keys().cloned().collect::<Vec<_>>();
        let generation = {
            let mut state = CLIPBOARD.lock().unwrap();
            state.generation = state.generation.wrapping_add(1);
            state.formats = formats;
            state.owner = false;
            state.value = Some(value["content"].clone());
            state.generation
        };
        let result: Result<(), dbus::Error> = {
            let _io = super::wayland_portal::IO.lock().unwrap();
            proxy.method_call(
                INTERFACE,
                "SetSelection",
                (session.path.clone(), option("mime_types", mimes)),
            )
        };
        {
            let mut state = CLIPBOARD.lock().unwrap();
            if state.generation == generation {
                state.owner = result.is_ok();
                if result.is_err() {
                    state.formats.clear();
                    state.value = None;
                }
            }
        }
        result.map_err(|_| (503, "Portal SetSelection failed".into()))?;
        return Ok(json!({"applied":true}));
    }
    let mimes = {
        let state = CLIPBOARD.lock().unwrap();
        if state.owner {
            return Ok(state.value.clone().unwrap_or(Value::Null));
        }
        state.mimes.clone()
    };
    let mime = ["text/plain;charset=utf-8", "text/plain", "image/png"]
        .into_iter()
        .find(|m| mimes.iter().any(|s| s == m));
    let Some(mime) = mime else {
        return Ok(Value::Null);
    };
    let (fd,): (OwnedFd,) = {
        let _io = super::wayland_portal::IO.lock().unwrap();
        proxy
            .method_call(INTERFACE, "SelectionRead", (session.path.clone(), mime))
            .map_err(|_| (503, "Portal SelectionRead failed".into()))?
    };
    let bytes = pipe(unsafe { File::from_raw_fd(fd.into_fd()) }, None).map_err(|e| (503, e))?;
    if mime == "image/png" {
        decode_png(&bytes).map_err(|e| (503, e))
    } else {
        if bytes.len() > MAX {
            return Err((413, "clipboard text too large".into()));
        }
        Ok(
            json!({"kind":"text","text":String::from_utf8(bytes).map_err(|_|(503,"invalid UTF-8 clipboard text".into()))?}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 用D-Bus原生类型覆盖标准及GNOME包装，不依赖字符串化的模拟信号。
    #[test]
    fn mime_list_accepts_standard_and_gnome_tuple() {
        let expected = vec!["text/plain".to_owned(), "image/png".to_owned()];
        assert_eq!(mime_types(&expected), expected);
        assert_eq!(mime_types(&(expected.clone(),)), expected);
        assert!(mime_types(&(expected.clone(), expected)).is_empty());
    }
    /// RGBA经PNG报价再读取保持像素和尺寸，包含透明像素。
    #[test]
    fn portal_png_roundtrip_preserves_rgba() {
        let value = json!({"kind":"image","width":2,"height":1,"data":STANDARD.encode([255u8,0,0,255,0,255,0,0])});
        let formats = encode(&value).unwrap();
        assert_eq!(decode_png(&formats["image/png"]).unwrap(), value);
    }
    /// 文本报价保持UTF-8，不把未知格式或过大数据发送到Portal。
    #[test]
    fn text_is_utf8_and_invalid_formats_are_rejected() {
        let formats = encode(&json!({"kind":"text","text":"Wayland剪贴板"})).unwrap();
        assert_eq!(
            &**formats.get("text/plain;charset=utf-8").unwrap(),
            "Wayland剪贴板".as_bytes()
        );
        assert!(encode(&json!({"kind":"file"})).is_err());
        assert!(encode(&json!({"kind":"image","width":16384,"height":16384,"data":""})).is_err());
    }
    /// 使用真实Unix pipe验证读取遇EOF结束，而非依赖剪贴板源永远在线。
    #[test]
    fn clipboard_pipe_finishes_at_eof() {
        use std::os::fd::IntoRawFd;
        let (source, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || peer.write_all(b"clipboard pipe").unwrap());
        let bytes = pipe(unsafe { File::from_raw_fd(source.into_raw_fd()) }, None).unwrap();
        thread.join().unwrap();
        assert_eq!(bytes, b"clipboard pipe");
    }
}
