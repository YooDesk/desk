// Included in the isolated RustDesk Android JNI module by build_android.py.
// RustDesk's JNI callback passes the buffer to the sink before returning to ImageReader.
type AndroidFrameSink = unsafe extern "C" fn(u64, u32, u32, usize, *const u8, usize) -> i32;
type AndroidStream = (u64, u32, u32, usize, AndroidFrameSink);
static YOODESK_ANDROID_STREAM: Mutex<Option<AndroidStream>> = Mutex::new(None);

#[no_mangle]
pub extern "C" fn yd_rustdesk_android_set_stream(
    epoch: u64,
    width: u32,
    height: u32,
    stride: usize,
    sink: Option<AndroidFrameSink>,
) {
    *YOODESK_ANDROID_STREAM.lock().unwrap() = sink.map(|sink| (epoch, width, height, stride, sink));
}

fn yoodesk_android_video(data: *const u8, len: usize) -> bool {
    let stream = YOODESK_ANDROID_STREAM.lock().unwrap();
    if let Some((epoch, width, height, stride, sink)) = *stream {
        // JNI owns the direct buffer through this synchronous call.
        unsafe { sink(epoch, width, height, stride, data, len) };
        true
    } else {
        false
    }
}
