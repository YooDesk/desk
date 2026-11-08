//! H.264 编码封装：把 BGRA 帧编码为 Annex B access unit。
//!
//! 规范名 `desk.media.h264.annexb-ms.v1`（见 `docs/design/10-revision-v2.md` §7）：
//! 每个 `EncodedVideoFrame.data` 是一个 Annex B access unit，`pts` 为媒体 epoch 起始的毫秒，
//! 单调递增；初始与重建先有 SPS/PPS+IDR，初版禁用 B 帧。
//!
//! 编码器是独立的 `openh264`（BSD-2-Clause），与 RustDesk 的 AGPL 代码树无关。

use std::time::Instant;

use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType,
};
use openh264::formats::{RgbSliceU8, YUVBuffer};
use openh264::OpenH264API;

use crate::json::{codes, ApiError};

/// 编码参数。
#[derive(Debug, Clone, Copy)]
pub struct EncoderSettings {
    /// 目标码率（bit/s）。
    pub bitrate_bps: u32,
    /// 目标帧率。
    pub fps: u32,
    /// 关键帧间隔（秒）；0 表示只按需请求 IDR。
    pub keyframe_interval_secs: u32,
}

impl Default for EncoderSettings {
    fn default() -> Self {
        Self {
            bitrate_bps: 4_000_000,
            fps: 20,
            keyframe_interval_secs: 2,
        }
    }
}

/// 一帧编码结果。
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// Annex B 字节（含 SPS/PPS 的 IDR 首帧）。
    pub data: Vec<u8>,
    /// 是否为关键帧（由编码器标记的 IDR 判断）。
    pub key: bool,
    /// 相对媒体 epoch 起始的毫秒。
    pub pts_ms: i64,
}

/// `openh264` 是否在本构建中可用。
///
/// 依赖 `source` feature；编译期即确定，因此是常量而非运行时探测。
pub fn h264_available() -> bool {
    true
}

/// 单会话 H.264 编码器。
///
/// 分辨率变化时 `openh264` 内部会重新初始化；本封装额外请求 IDR，保证重建后首帧可解。
pub struct H264Encoder {
    encoder: Encoder,
    settings: EncoderSettings,
    epoch: Instant,
    yuv: Option<(u32, u32, YUVBuffer)>,
    rgb: Vec<u8>,
    force_keyframe: bool,
}

impl H264Encoder {
    /// 创建编码器。
    pub fn new(settings: EncoderSettings) -> Result<Self, ApiError> {
        let config = EncoderConfig::new()
            .complexity(Complexity::Low)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(settings.bitrate_bps.max(200_000)))
            .max_frame_rate(FrameRate::from_hz(settings.fps.max(1) as f32))
            .usage_type(UsageType::ScreenContentRealTime)
            .skip_frames(true);
        let encoder =
            Encoder::with_api_config(OpenH264API::from_source(), config).map_err(|err| {
                tracing::error!(error = %err, "初始化 H.264 编码器失败");
                ApiError::new(500, codes::DESK_ENGINE, "h264 encoder unavailable")
            })?;
        Ok(Self {
            encoder,
            settings,
            epoch: Instant::now(),
            yuv: None,
            rgb: Vec::new(),
            force_keyframe: true,
        })
    }

    /// 编码参数。
    pub fn settings(&self) -> EncoderSettings {
        self.settings
    }

    /// 请求下一帧为关键帧；用于重连、分辨率变化或浏览器请求。
    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    /// 编码一帧 BGRA 像素。`width/height` 必须与 `data` 长度一致。
    pub fn encode(
        &mut self,
        width: u32,
        height: u32,
        bgra: &[u8],
    ) -> Result<EncodedFrame, ApiError> {
        let expected = width as usize * height as usize * 4;
        if width == 0 || height == 0 || bgra.len() != expected {
            return Err(ApiError::new(
                500,
                codes::DESK_ENGINE,
                "frame size mismatch",
            ));
        }
        if self.force_keyframe {
            self.encoder.force_intra_frame();
            self.force_keyframe = false;
        }

        // BGRA → RGB24。openh264 的 `read_rgb8` 是整数运算路径，明显快于逐像素 f32 转换。
        self.rgb.clear();
        self.rgb.reserve(width as usize * height as usize * 3);
        // BGRA → RGB24。`as_chunks::<4>()` 让每个像素是定长数组，避免边界检查。
        for pixel in bgra.as_chunks::<4>().0 {
            self.rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }

        let stale = match &self.yuv {
            Some((w, h, _)) => *w != width || *h != height,
            None => true,
        };
        if stale {
            self.yuv = Some((
                width,
                height,
                YUVBuffer::new(width as usize, height as usize),
            ));
        }
        let (_, _, yuv) = self.yuv.as_mut().expect("YUV 缓冲刚在上一步初始化");
        yuv.read_rgb8(RgbSliceU8::new(
            &self.rgb,
            (width as usize, height as usize),
        ));

        let bitstream = self.encoder.encode(yuv).map_err(|err| {
            tracing::warn!(error = %err, "H.264 编码失败");
            ApiError::new(500, codes::DESK_ENGINE, "h264 encode failed")
        })?;
        let data = bitstream.to_vec();
        let key = contains_idr(&data);
        Ok(EncodedFrame {
            data,
            key,
            pts_ms: self.epoch.elapsed().as_millis() as i64,
        })
    }
}

/// 判断 Annex B 码流中是否含 IDR slice（NAL 类型 5）。
///
/// 只看 NAL 头，不解码载荷；用于把“关键帧”标记给浏览器，便于触发解码器重建。
fn contains_idr(data: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 <= data.len() {
        // 起始码 00 00 01 或 00 00 00 01。
        let sc3 = data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1;
        let sc4 = i + 5 <= data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1;
        if sc3 {
            let nal = data[i + 3] & 0x1f;
            if nal == 5 {
                return true;
            }
            i += 3;
        } else if sc4 {
            let nal = data[i + 4] & 0x1f;
            if nal == 5 {
                return true;
            }
            i += 4;
        } else {
            i += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_bgra(width: u32, height: u32, b: u8, g: u8, r: u8) -> Vec<u8> {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            data.extend_from_slice(&[b, g, r, 0xff]);
        }
        data
    }

    #[test]
    fn encodes_real_frame_with_idr_first() {
        let mut encoder = H264Encoder::new(EncoderSettings::default()).unwrap();
        let frame = solid_bgra(320, 240, 20, 30, 40);
        let encoded = encoder.encode(320, 240, &frame).unwrap();
        assert!(!encoded.data.is_empty());
        assert!(encoded.key, "首帧必须是 IDR");
        assert!(contains_idr(&encoded.data));
        // Annex B 起始码。
        assert_eq!(&encoded.data[..4], &[0, 0, 0, 1]);
    }

    #[test]
    fn rejects_size_mismatch() {
        let mut encoder = H264Encoder::new(EncoderSettings::default()).unwrap();
        let frame = solid_bgra(16, 16, 0, 0, 0);
        assert!(encoder.encode(32, 16, &frame).is_err());
    }

    #[test]
    fn keyframe_request_produces_idr() {
        let mut encoder = H264Encoder::new(EncoderSettings::default()).unwrap();
        let frame = solid_bgra(160, 120, 0, 0, 0);
        let _ = encoder.encode(160, 120, &frame).unwrap();
        // 改变内容，避免编码器输出空帧。
        let other = solid_bgra(160, 120, 200, 10, 10);
        let _ = encoder.encode(160, 120, &other).unwrap();
        encoder.request_keyframe();
        let encoded = encoder.encode(160, 120, &frame).unwrap();
        assert!(encoded.key);
    }

    #[test]
    fn pts_is_monotonic() {
        let mut encoder = H264Encoder::new(EncoderSettings::default()).unwrap();
        let frame = solid_bgra(64, 64, 0, 0, 0);
        let first = encoder.encode(64, 64, &frame).unwrap();
        let second = encoder.encode(64, 64, &frame).unwrap();
        assert!(second.pts_ms >= first.pts_ms);
    }
}
