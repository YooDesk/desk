use std::ffi::c_void;
#[link(name = "avrt")]
unsafe extern "system" {
    fn AvSetMmThreadCharacteristicsW(task: *const u16, index: *mut u32) -> *mut c_void;
    fn AvSetMmThreadPriority(handle: *mut c_void, priority: i32) -> i32;
    fn AvRevertMmThreadCharacteristics(handle: *mut c_void) -> i32;
}
// Constructed and dropped by the dedicated capture thread.
pub struct Guard(*mut c_void);
impl Guard {
    pub fn new() -> Option<Self> {
        let name: Vec<u16> = "Capture\0".encode_utf16().collect();
        let mut index = 0;
        let handle = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut index) };
        if std::env::var_os("YOODESK_VIDEO_PIPELINE_DIAGNOSTICS").is_some() {
            eprintln!("Desk capture scheduling: mmcss={}", !handle.is_null());
        }
        if handle.is_null() { return None; }
        unsafe { AvSetMmThreadPriority(handle, 1); } // AVRT_PRIORITY_HIGH
        Some(Self(handle))
    }
}
impl Drop for Guard {
    fn drop(&mut self) { unsafe { AvRevertMmThreadCharacteristics(self.0); } }
}
