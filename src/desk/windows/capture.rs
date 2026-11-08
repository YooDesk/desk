//! Windows 桌面采集：DXGI Desktop Duplication 优先，GDI 回退。
//!
//! 这是本产品自己的最小实现，只覆盖被控端需要的“把某块屏幕变成 BGRA 帧”的能力。
//! 不搬运 RustDesk/scrap 的源码（AGPL），因此不带其 HDR 色调映射、旋转 GPU 路径、
//! 隐私屏、VRAM 编码等扩展；这些能力如需支持应在后续任务中单独实现并留有证据。
//!
//! 关键点：
//! - `AcquireNextFrame` 超时返回 `WouldBlock`，调用方据此刷新心跳而非报错；
//! - 画面无变化的重复帧同样返回 `WouldBlock`，避免空转编码；
//! - DXGI 设备丢失（分辨率切换、UAC 安全桌面、锁屏）先尝试重建 duplication；
//!   重建失败才降级到 GDI，并明确标记 `is_gdi()`。
//!
//! 本模块直接调用 Win32/DXGI，因此需要 `unsafe`；这是平台 FFI 的固有部分，
//! 只在 `windows` 目标下编译，crate 其余部分仍禁止 `unsafe`。

#![allow(unsafe_code)]

use std::io;
use std::mem;
use std::ptr;
use std::slice;
use std::time::{Duration, SystemTime};

use winapi::shared::dxgi::*;
use winapi::shared::dxgi1_2::*;
use winapi::shared::dxgiformat::DXGI_FORMAT_B8G8R8A8_UNORM;
use winapi::shared::dxgitype::*;
use winapi::shared::minwindef::{FALSE, UINT};
use winapi::shared::ntdef::LONG;
use winapi::shared::windef::{HBITMAP, HDC};
use winapi::shared::winerror::*;
use winapi::um::d3d11::*;
use winapi::um::d3dcommon::D3D_DRIVER_TYPE_UNKNOWN;
use winapi::um::unknwnbase::IUnknown;
use winapi::um::wingdi::*;
use winapi::um::winnt::HRESULT;

use crate::desk::{CaptureMode, Capturer, DisplayGeometry, RgbaFrame};
use crate::json::{codes, ApiError};

/// COM 智能指针：只做 Release，不 clone。
struct ComPtr<T>(*mut T);

impl<T> ComPtr<T> {
    fn null() -> Self {
        Self(ptr::null_mut())
    }

    fn is_null(&self) -> bool {
        self.0.is_null()
    }

    unsafe fn release(&mut self) {
        if !self.0.is_null() {
            (*(self.0 as *mut IUnknown)).Release();
            self.0 = ptr::null_mut();
        }
    }
}

impl<T> Drop for ComPtr<T> {
    fn drop(&mut self) {
        unsafe { self.release() }
    }
}

fn wrap_hresult(hr: HRESULT) -> io::Result<()> {
    if hr >= 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!("HRESULT 0x{:08X}", hr as u32)))
    }
}

/// 找到的 DXGI 输出及其适配器。
struct DxgiOutput {
    adapter: ComPtr<IDXGIAdapter1>,
    output: ComPtr<IDXGIOutput>,
}

impl DxgiOutput {
    /// 交出原始指针所有权，调用方负责最终 Release。
    fn into_raw(self) -> (*mut IDXGIAdapter1, *mut IDXGIOutput) {
        let adapter = self.adapter.0;
        let output = self.output.0;
        std::mem::forget(self);
        (adapter, output)
    }
}

// SAFETY: 这些 COM 指针只在本结构内部使用；采集器本身是 `Send`，
// 但同一 duplication 只被一个线程轮询（会话按显示器独占采集器）。
unsafe impl Send for DxgiOutput {}

/// DXGI 采集器。
pub struct DxgiCapturer {
    geometry: DisplayGeometry,
    device: ComPtr<ID3D11Device>,
    context: ComPtr<ID3D11DeviceContext>,
    duplication: ComPtr<IDXGIOutputDuplication>,
    output: ComPtr<IDXGIOutput>,
    adapter: ComPtr<IDXGIAdapter1>,
    staging: ComPtr<ID3D11Texture2D>,
    staging_size: (u32, u32),
    /// 上一帧采样摘要，用于跳过完全相同的重复帧。
    last_hash: u64,
    /// GDI 回退采集器；`Some` 表示当前走 GDI。
    gdi: Option<GdiCapturer>,
    /// 是否已经交付过至少一帧。
    ///
    /// DXGI 只在桌面**发生变化**时报告帧；静止桌面会一直 `WAIT_TIMEOUT`。
    /// 因此首帧必须用 GDI 主动读取一次当前画面，否则浏览器连上后可能长时间空白。
    /// 这不是兜底：之后仍然完全依赖 DXGI 的增量更新。
    delivered_first: bool,
    /// 专门用于首帧/按需快照的 GDI 读取器，与 `gdi`（回退）分开。
    seed: Option<GdiCapturer>,
}

impl DxgiCapturer {
    /// 为指定显示器创建采集器。
    pub fn new(geometry: DisplayGeometry) -> io::Result<Self> {
        let output = find_output(&geometry)?;
        let (device, context) = create_device(output.adapter.0)?;
        let duplication = duplicate_output(output.output.0, device)?;
        // `output` 仍持有 adapter/output 的所有权；这里显式交出指针，
        // 否则它会在本函数返回时再 Release 一次，导致 COM 对象被提前销毁。
        let (adapter_ptr, output_ptr) = output.into_raw();
        Ok(Self {
            geometry,
            device: ComPtr(device),
            context: ComPtr(context),
            duplication: ComPtr(duplication),
            output: ComPtr(output_ptr),
            adapter: ComPtr(adapter_ptr),
            staging: ComPtr::null(),
            staging_size: (0, 0),
            last_hash: 0,
            gdi: None,
            delivered_first: false,
            seed: None,
        })
    }

    /// 重新建立 duplication；用于设备丢失后的恢复。
    fn recreate(&mut self) -> io::Result<()> {
        self.duplication = ComPtr::null();
        self.staging = ComPtr::null();
        self.staging_size = (0, 0);
        let (device, context) = create_device(self.adapter.0)?;
        self.device = ComPtr(device);
        self.context = ComPtr(context);
        let duplication = duplicate_output(self.output.0, self.device.0)?;
        self.duplication = ComPtr(duplication);
        Ok(())
    }

    fn fallback_to_gdi(&mut self, reason: &str) {
        tracing::warn!(%reason, display = self.geometry.index, "DXGI 不可用，降级到 GDI 采集");
        self.gdi = GdiCapturer::new(&self.geometry).ok();
        self.duplication = ComPtr::null();
    }

    /// 确保 staging 纹理与当前显示器尺寸匹配。
    fn ensure_staging(&mut self) -> io::Result<()> {
        let (width, height) = (self.geometry.width.max(1), self.geometry.height.max(1));
        if !self.staging.is_null() && self.staging_size == (width, height) {
            return Ok(());
        }
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ,
                MiscFlags: 0,
            };
            let mut texture: *mut ID3D11Texture2D = ptr::null_mut();
            wrap_hresult((*self.device.0).CreateTexture2D(&desc, ptr::null(), &mut texture))?;
            self.staging.release();
            self.staging.0 = texture;
            self.staging_size = (width, height);
            self.last_hash = 0;
        }
        Ok(())
    }

    /// 采集一帧并拷贝到紧凑 BGRA 缓冲。
    fn capture_dxgi(&mut self, timeout: Duration) -> io::Result<RgbaFrame> {
        self.ensure_staging()?;
        unsafe {
            let mut info: DXGI_OUTDUPL_FRAME_INFO = mem::zeroed();
            let mut frame: *mut IDXGIResource = ptr::null_mut();
            let hr = (*self.duplication.0).AcquireNextFrame(
                timeout.as_millis().min(u32::MAX as u128) as UINT,
                &mut info,
                &mut frame,
            );
            if hr == DXGI_ERROR_WAIT_TIMEOUT {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            if hr == DXGI_ERROR_ACCESS_LOST {
                if !frame.is_null() {
                    (*(frame as *mut IUnknown)).Release();
                }
                self.recreate()?;
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            wrap_hresult(hr)?;
            let frame = ComPtr(frame);
            // `LastPresentTime == 0` 表示只有鼠标移动、没有桌面更新。
            if *info.LastPresentTime.QuadPart() == 0 {
                (*self.duplication.0).ReleaseFrame();
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            let mut texture: *mut ID3D11Texture2D = ptr::null_mut();
            (*frame.0).QueryInterface(
                &IID_ID3D11Texture2D,
                &mut texture as *mut *mut _ as *mut *mut _,
            );
            let texture = ComPtr(texture);
            if texture.is_null() {
                (*self.duplication.0).ReleaseFrame();
                return Err(io::Error::other("frame is not a texture"));
            }
            let mut desc: D3D11_TEXTURE2D_DESC = mem::zeroed();
            (*texture.0).GetDesc(&mut desc);
            if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
                // HDR/浮点桌面需要色调映射，超出本批次范围：明确失败而不是送出错误格式。
                (*self.duplication.0).ReleaseFrame();
                return Err(io::Error::other(format!(
                    "unsupported desktop format {}",
                    desc.Format
                )));
            }
            let (width, height) = (desc.Width, desc.Height);
            if width != self.staging_size.0 || height != self.staging_size.1 {
                (*self.duplication.0).ReleaseFrame();
                self.geometry.width = width;
                self.geometry.height = height;
                self.staging = ComPtr::null();
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            (*self.context.0).CopyResource(self.staging.0 as *mut _, texture.0 as *mut _);
            (*self.duplication.0).ReleaseFrame();

            let mut mapped: D3D11_MAPPED_SUBRESOURCE = mem::zeroed();
            let map_hr =
                (*self.context.0).Map(self.staging.0 as *mut _, 0, D3D11_MAP_READ, 0, &mut mapped);
            wrap_hresult(map_hr)?;
            let pitch = mapped.RowPitch as usize;
            let row_bytes = width as usize * 4;
            let mut data = Vec::with_capacity(row_bytes * height as usize);
            {
                let base = mapped.pData as *const u8;
                for row in 0..height as usize {
                    let src = slice::from_raw_parts(base.add(row * pitch), row_bytes);
                    data.extend_from_slice(src);
                }
            }
            (*self.context.0).Unmap(self.staging.0 as *mut _, 0);

            // 跳过与上一帧完全相同的画面；采样摘要成本与帧大小成正比但步长固定。
            let hash = hash_bytes(&data);
            if hash == self.last_hash {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            self.last_hash = hash;
            self.delivered_first = true;
            Ok(RgbaFrame {
                display: self.geometry.index,
                width,
                height,
                data,
                captured_at: SystemTime::now(),
            })
        }
    }
}

// SAFETY: `DxgiCapturer` 持有 DXGI/D3D11 COM 指针，但这些对象只被创建它的线程使用；
// `Capturer: Send` 只用于把采集器移动到阻塞采集线程，不会并发访问同一 duplication。
unsafe impl Send for DxgiCapturer {}

impl Capturer for DxgiCapturer {
    fn geometry(&self) -> &DisplayGeometry {
        &self.geometry
    }

    fn next_frame(&mut self, timeout: Duration) -> io::Result<RgbaFrame> {
        if let Some(gdi) = &mut self.gdi {
            return gdi.next_frame(timeout);
        }
        // 首帧：DXGI 没有“当前画面”的同步读取，静止桌面只会超时；
        // 这里用一次 GDI BitBlt 得到初始快照，之后恢复正常 DXGI 增量路径。
        if !self.delivered_first {
            if self.seed.is_none() {
                match GdiCapturer::new(&self.geometry) {
                    Ok(seed) => self.seed = Some(seed),
                    Err(err) => {
                        tracing::warn!(error = %err, "首帧 GDI 采集器创建失败");
                    }
                }
            }
            if let Some(seed) = &mut self.seed {
                match seed.next_frame(timeout) {
                    Ok(frame) => {
                        self.delivered_first = true;
                        self.last_hash = hash_bytes(&frame.data);
                        return Ok(frame);
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        return Err(err);
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "首帧 GDI 读取失败，继续等待 DXGI 更新");
                    }
                }
            }
        }
        if self.duplication.is_null() {
            self.fallback_to_gdi("duplication unavailable");
            return self
                .gdi
                .as_mut()
                .ok_or_else(|| io::Error::other("no capturer"))?
                .next_frame(timeout);
        }
        match self.capture_dxgi(timeout) {
            Ok(frame) => Ok(frame),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => Err(err),
            Err(err) => {
                let message = err.to_string();
                // 先尝试重建一次（分辨率切换等可恢复错误）；仍失败才降级。
                if self.recreate().is_ok() {
                    if let Ok(frame) = self.capture_dxgi(Duration::from_millis(0)) {
                        return Ok(frame);
                    }
                }
                self.fallback_to_gdi(&message);
                self.gdi
                    .as_mut()
                    .ok_or_else(|| io::Error::other("no capturer"))?
                    .next_frame(timeout)
            }
        }
    }

    fn mode(&self) -> CaptureMode {
        if self.gdi.is_some() {
            CaptureMode::Gdi
        } else {
            CaptureMode::DesktopDuplication
        }
    }

    fn is_gdi(&self) -> bool {
        self.gdi.is_some()
    }

    fn snapshot(&mut self) -> io::Result<RgbaFrame> {
        if let Some(gdi) = &mut self.gdi {
            return gdi.next_frame(Duration::from_millis(0));
        }
        // 首次需要快照时创建（或复用）GDI 读取器；之后每帧快照都用它，
        // 因为它读取的是屏幕当前内容而不是“下一次更新”。
        if self.seed.is_none() {
            self.seed = Some(GdiCapturer::new(&self.geometry)?);
        }
        let frame = self
            .seed
            .as_mut()
            .expect("刚创建")
            .next_frame(Duration::from_millis(0))?;
        self.delivered_first = true;
        self.last_hash = hash_bytes(&frame.data);
        Ok(frame)
    }
}

impl Drop for DxgiCapturer {
    fn drop(&mut self) {
        // COM 对象必须按依赖顺序释放：先放掉依赖 D3D11 设备的 duplication/staging，
        // 再放 context/device，最后才是 DXGI output/adapter。字段声明顺序释放会让
        // 设备先于 duplication 消失，在 Windows 上表现为退出时的访问冲突。
        self.gdi.take();
        self.seed.take();
        unsafe {
            self.staging.release();
            self.duplication.release();
            self.context.release();
            self.device.release();
            self.output.release();
            self.adapter.release();
        }
    }
}

/// FNV-1a 64 位采样摘要：足够过滤重复帧，不需要密码学强度。
fn hash_bytes(data: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    let step = (data.len() / 8192).max(1);
    for byte in data.iter().step_by(step) {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn create_device(
    adapter: *mut IDXGIAdapter1,
) -> io::Result<(*mut ID3D11Device, *mut ID3D11DeviceContext)> {
    unsafe {
        let mut device: *mut ID3D11Device = ptr::null_mut();
        let mut context: *mut ID3D11DeviceContext = ptr::null_mut();
        wrap_hresult(D3D11CreateDevice(
            adapter as *mut _,
            D3D_DRIVER_TYPE_UNKNOWN,
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            0,
            D3D11_SDK_VERSION,
            &mut device,
            ptr::null_mut(),
            &mut context,
        ))?;
        Ok((device, context))
    }
}

fn duplicate_output(
    output: *mut IDXGIOutput,
    device: *mut ID3D11Device,
) -> io::Result<*mut IDXGIOutputDuplication> {
    unsafe {
        let output1 = output as *mut IDXGIOutput1;
        let mut duplication: *mut IDXGIOutputDuplication = ptr::null_mut();
        wrap_hresult((*output1).DuplicateOutput(device as *mut _, &mut duplication))?;
        Ok(duplication)
    }
}

/// 按声明几何找到对应 DXGI 输出。
///
/// 以显示名称匹配优先；名称缺失时退回矩形匹配。找不到就明确失败，
/// 不允许“随便挑一个显示器”这类静默错配。
fn find_output(geometry: &DisplayGeometry) -> io::Result<DxgiOutput> {
    unsafe {
        let mut factory: *mut IDXGIFactory1 = ptr::null_mut();
        wrap_hresult(CreateDXGIFactory1(
            &IID_IDXGIFactory1,
            &mut factory as *mut *mut _ as *mut *mut _,
        ))?;
        let factory = ComPtr(factory);
        let mut adapter_index = 0u32;
        loop {
            let mut adapter: *mut IDXGIAdapter1 = ptr::null_mut();
            let hr = (*factory.0).EnumAdapters1(adapter_index, &mut adapter);
            if hr == DXGI_ERROR_NOT_FOUND {
                break;
            }
            wrap_hresult(hr)?;
            let adapter_ptr = adapter;
            let mut output_index = 0u32;
            loop {
                let mut output: *mut IDXGIOutput = ptr::null_mut();
                let hr = (*adapter_ptr).EnumOutputs(output_index, &mut output);
                if hr == DXGI_ERROR_NOT_FOUND {
                    break;
                }
                wrap_hresult(hr)?;
                output_index += 1;
                let output_ptr = output;
                let mut desc: DXGI_OUTPUT_DESC = mem::zeroed();
                (*output_ptr).GetDesc(&mut desc);
                if desc.AttachedToDesktop == FALSE {
                    (*(output_ptr as *mut IUnknown)).Release();
                    continue;
                }
                let rect = desc.DesktopCoordinates;
                let width = (rect.right - rect.left).max(0) as u32;
                let height = (rect.bottom - rect.top).max(0) as u32;
                let name = wide_to_string(&desc.DeviceName);
                let match_name = !geometry.name.is_empty() && name == geometry.name;
                let match_rect = rect.left == geometry.x
                    && rect.top == geometry.y
                    && width == geometry.width
                    && height == geometry.height;
                if match_name || (geometry.name.is_empty() && match_rect) {
                    return Ok(DxgiOutput {
                        adapter: ComPtr(adapter_ptr),
                        output: ComPtr(output_ptr),
                    });
                }
                (*(output_ptr as *mut IUnknown)).Release();
            }
            (*(adapter_ptr as *mut IUnknown)).Release();
            adapter_index += 1;
        }
        Err(io::Error::other("display output not found"))
    }
}

/// GDI 回退采集：兼容老显卡/远程会话，速度慢但可用。
pub struct GdiCapturer {
    geometry: DisplayGeometry,
    screen_dc: HDC,
    memory_dc: HDC,
    bitmap: HBITMAP,
    pixels: Vec<u8>,
}

// SAFETY: GDI 句柄仅在采集线程内部使用；不跨线程共享。
unsafe impl Send for GdiCapturer {}

impl GdiCapturer {
    /// 针对显示器创建 GDI 采集器，按虚拟桌面坐标裁剪。
    pub fn new(geometry: &DisplayGeometry) -> io::Result<Self> {
        unsafe {
            let driver = wide("DISPLAY");
            // 注意 `CreateDCW` 的第二个参数才是设备名（Windows 的 API 签名把
            // `pwszDriver`/`pwszDevice` 这两个名字与它的实际用途对调了）。
            let screen_dc = CreateDCW(driver.as_ptr(), driver.as_ptr(), ptr::null(), ptr::null());
            if screen_dc.is_null() {
                let code = winapi::um::errhandlingapi::GetLastError();
                return Err(io::Error::other(format!("CreateDCW failed (err {code})")));
            }
            let memory_dc = CreateCompatibleDC(screen_dc);
            if memory_dc.is_null() {
                DeleteDC(screen_dc);
                return Err(io::Error::other("CreateCompatibleDC failed"));
            }
            let width = geometry.width.max(1) as i32;
            let height = geometry.height.max(1) as i32;
            let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
            if bitmap.is_null() {
                DeleteDC(memory_dc);
                DeleteDC(screen_dc);
                return Err(io::Error::other("CreateCompatibleBitmap failed"));
            }
            SelectObject(memory_dc, bitmap as *mut _);
            Ok(Self {
                geometry: geometry.clone(),
                screen_dc,
                memory_dc,
                bitmap,
                pixels: Vec::new(),
            })
        }
    }
}

impl Capturer for GdiCapturer {
    fn geometry(&self) -> &DisplayGeometry {
        &self.geometry
    }

    fn next_frame(&mut self, _timeout: Duration) -> io::Result<RgbaFrame> {
        let width = self.geometry.width.max(1) as i32;
        let height = self.geometry.height.max(1) as i32;
        self.pixels.resize(width as usize * height as usize * 4, 0);
        unsafe {
            let ok = BitBlt(
                self.memory_dc,
                0,
                0,
                width,
                height,
                self.screen_dc,
                self.geometry.x,
                self.geometry.y,
                SRCCOPY | CAPTUREBLT,
            );
            if ok == 0 {
                return Err(io::Error::other("BitBlt failed"));
            }
            let mut info: BITMAPINFO = mem::zeroed();
            info.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width;
            // 负数高度表示自上而下的行序，正好匹配浏览器 ImageData。
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let lines = GetDIBits(
                self.memory_dc,
                self.bitmap,
                0,
                height as u32,
                self.pixels.as_mut_ptr() as *mut _,
                &mut info,
                DIB_RGB_COLORS,
            );
            if lines == 0 {
                return Err(io::Error::other("GetDIBits failed"));
            }
        }
        Ok(RgbaFrame {
            display: self.geometry.index,
            width: width as u32,
            height: height as u32,
            data: std::mem::take(&mut self.pixels),
            captured_at: SystemTime::now(),
        })
    }

    fn mode(&self) -> CaptureMode {
        CaptureMode::Gdi
    }

    fn is_gdi(&self) -> bool {
        true
    }
}

impl Drop for GdiCapturer {
    fn drop(&mut self) {
        unsafe {
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap as *mut _);
            }
            if !self.memory_dc.is_null() {
                DeleteDC(self.memory_dc);
            }
            if !self.screen_dc.is_null() {
                DeleteDC(self.screen_dc);
            }
        }
    }
}

/// 把 Windows 宽字符显示名转成 UTF-8；非法序列直接丢弃，不 panic。
fn wide_to_string(name: &[u16]) -> String {
    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    String::from_utf16_lossy(&name[..end])
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 枚举所有在线显示器，主显示器排在最前。
///
/// 只返回真实附加到桌面的输出；不把离线/镜像残留项算作可用显示器。
pub fn enumerate_displays() -> Result<Vec<DisplayGeometry>, ApiError> {
    let primary = primary_display_name();
    let mut displays = Vec::new();
    unsafe {
        let mut factory: *mut IDXGIFactory1 = ptr::null_mut();
        wrap_hresult(CreateDXGIFactory1(
            &IID_IDXGIFactory1,
            &mut factory as *mut *mut _ as *mut *mut _,
        ))
        .map_err(engine_error)?;
        let factory = ComPtr(factory);
        let mut adapter_index = 0u32;
        loop {
            let mut adapter: *mut IDXGIAdapter1 = ptr::null_mut();
            let hr = (*factory.0).EnumAdapters1(adapter_index, &mut adapter);
            if hr == DXGI_ERROR_NOT_FOUND {
                break;
            }
            wrap_hresult(hr).map_err(engine_error)?;
            let adapter_ptr = adapter;
            let mut output_index = 0u32;
            loop {
                let mut output: *mut IDXGIOutput = ptr::null_mut();
                let hr = (*adapter_ptr).EnumOutputs(output_index, &mut output);
                if hr == DXGI_ERROR_NOT_FOUND {
                    break;
                }
                wrap_hresult(hr).map_err(engine_error)?;
                output_index += 1;
                let output_ptr = output;
                let mut desc: DXGI_OUTPUT_DESC = mem::zeroed();
                (*output_ptr).GetDesc(&mut desc);
                let attached = desc.AttachedToDesktop != FALSE;
                let rect = desc.DesktopCoordinates;
                let width = (rect.right - rect.left).max(0) as u32;
                let height = (rect.bottom - rect.top).max(0) as u32;
                let name = wide_to_string(&desc.DeviceName);
                (*(output_ptr as *mut IUnknown)).Release();
                if !attached || width == 0 || height == 0 {
                    continue;
                }
                displays.push(DisplayGeometry {
                    index: 0,
                    x: rect.left,
                    y: rect.top,
                    width,
                    height,
                    primary: primary.as_deref() == Some(name.as_str()),
                    name,
                });
            }
            (*(adapter_ptr as *mut IUnknown)).Release();
            adapter_index += 1;
        }
    }
    if displays.is_empty() {
        return Err(ApiError::new(
            503,
            codes::DESK_ENGINE,
            "no attached display",
        ));
    }
    // 主显示器优先，其余按虚拟桌面位置排序，保证索引稳定。
    displays.sort_by_key(|d| (!d.primary, d.x, d.y));
    for (index, display) in displays.iter_mut().enumerate() {
        display.index = index as u32;
    }
    Ok(displays)
}

/// 主显示器的 DXGI 名称；失败不影响枚举，只是“主显示器”标记缺失。
fn primary_display_name() -> Option<String> {
    unsafe {
        let mut factory: *mut IDXGIFactory1 = ptr::null_mut();
        if wrap_hresult(CreateDXGIFactory1(
            &IID_IDXGIFactory1,
            &mut factory as *mut *mut _ as *mut *mut _,
        ))
        .is_err()
        {
            return None;
        }
        let factory = ComPtr(factory);
        let mut adapter: *mut IDXGIAdapter1 = ptr::null_mut();
        if (*factory.0).EnumAdapters1(0, &mut adapter) != 0 {
            return None;
        }
        let adapter_ptr = adapter;
        let mut output: *mut IDXGIOutput = ptr::null_mut();
        if (*adapter_ptr).EnumOutputs(0, &mut output) != 0 {
            (*(adapter_ptr as *mut IUnknown)).Release();
            return None;
        }
        let mut desc: DXGI_OUTPUT_DESC = mem::zeroed();
        (*output).GetDesc(&mut desc);
        (*(output as *mut IUnknown)).Release();
        (*(adapter_ptr as *mut IUnknown)).Release();
        Some(wide_to_string(&desc.DeviceName))
    }
}

fn engine_error(err: io::Error) -> ApiError {
    tracing::error!(error = %err, "枚举显示器失败");
    ApiError::new(503, codes::DESK_ENGINE, "display enumeration failed")
}

/// 非负化，仅用于几何计算。
#[allow(dead_code)]
fn non_negative(value: LONG) -> i32 {
    value.max(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_with_content() {
        let a = vec![0u8; 4096];
        let mut b = a.clone();
        b[100] = 7;
        assert_ne!(hash_bytes(&a), hash_bytes(&b));
    }

    #[test]
    fn wide_conversion_stops_at_nul() {
        let mut name = [0u16; 32];
        for (i, ch) in "\\\\.\\DISPLAY1".encode_utf16().enumerate() {
            name[i] = ch;
        }
        assert_eq!(wide_to_string(&name), "\\\\.\\DISPLAY1");
    }
}
