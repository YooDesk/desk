//! 与C++探测门槛一致：逐活动CRTC实际抓帧，而非仅检测库或显示器列表。
/// 必须在root短子进程内执行，父进程负责10秒总期限及回收。
pub(super) fn capture() -> Result<(), String> {
    if unsafe { hbb_common::libc::geteuid() } != 0 {
        return Err("DRM probe requires root".into());
    }
    let lib = scrap::drmtap_dl::get().ok_or("trusted libdrmtap unavailable")?;
    let mut paths = std::fs::read_dir("/dev/dri")
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_prefix("card")
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|e| e.path())
        .collect::<Vec<_>>();
    paths.sort();
    let mut active = 0;
    for path in paths {
        use std::os::unix::ffi::OsStrExt;
        let name =
            std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        let cfg = scrap::drmtap_dl::drmtap_config {
            device_path: name.as_ptr(),
            ..Default::default()
        };
        let ctx = unsafe { (lib.open)(&cfg) };
        if ctx.is_null() {
            continue;
        }
        let mut displays: [scrap::drmtap_dl::drmtap_display; 64] = unsafe { std::mem::zeroed() };
        let count = unsafe { (lib.list_displays)(ctx, displays.as_mut_ptr(), 64) };
        unsafe {
            (lib.close)(ctx);
        }
        if count < 0 || count as usize > displays.len() {
            return Err("invalid DRM enumeration".into());
        }
        for display in displays
            .iter()
            .take(count as usize)
            .filter(|d| d.active != 0)
        {
            active += 1;
            let cfg = scrap::drmtap_dl::drmtap_config {
                device_path: name.as_ptr(),
                crtc_id: display.crtc_id,
                ..Default::default()
            };
            let ctx = unsafe { (lib.open)(&cfg) };
            if ctx.is_null() {
                return Err("active CRTC open failed".into());
            }
            let mut frame: scrap::drmtap_dl::drmtap_frame_info = unsafe { std::mem::zeroed() };
            let rc = unsafe { (lib.grab_mapped)(ctx, &mut frame) };
            let supported = rc == 0
                && !frame.data.is_null()
                && frame.width > 0
                && frame.height > 0
                && frame.width <= 16384
                && frame.height <= 16384
                && frame.stride >= frame.width.saturating_mul(4)
                && frame.stride <= i32::MAX as u32
                && [*b"XR24", *b"AR24", *b"XB24", *b"AB24"]
                    .iter()
                    .any(|f| u32::from_le_bytes(*f) == frame.format);
            if rc == 0 {
                unsafe {
                    (lib.frame_release)(ctx, &mut frame);
                }
            }
            unsafe {
                (lib.close)(ctx);
            }
            if !supported {
                return Err(format!(
                    "actual DRM grab/format failed device={} crtc={} rc={rc}",
                    path.display(),
                    display.crtc_id
                ));
            }
        }
    }
    if active == 0 {
        return Err("no active DRM CRTC".into());
    }
    Ok(())
}
