//! One hardware-selection policy for the desktop SDK; upstream owns device selection.
use scrap::hwcodec::HwCodecConfig;

pub fn initialize() {
    let software_only = std::env::var("YOODESK_VIDEO_ENCODER").as_deref() == Ok("software");
    let detected = if software_only {
        HwCodecConfig::default()
    } else {
        // Service deployments execute this on the desktop worker side; a local
        // in-process console uses the same upstream probe without an IPC server.
        serde_json::from_str(&scrap::hwcodec::check_available_hwcodec()).unwrap_or_default()
    };
    log::info!("Desk codec probe: hardware_ram={}", detected.ram_encode.len());
    HwCodecConfig::set(serde_json::to_string(&detected).unwrap_or_default());
    // This appends libx264/libx265 at lowest priority without dropping GPU candidates.
    librustdesk::video_service::register_software_encoders();
}

#[cfg(test)]
mod tests {
    use super::*;
    use scrap::codec::{Encoder, EncoderCfg, EncodingUpdate};
    use scrap::hwcodec::{HwRamEncoder, HwRamEncoderConfig};
    use scrap::raw_hwcodec::ffmpeg_ram::{CodecInfo, Priority};
    use base::message_proto::{supported_decoding::PreferCodec, SupportedDecoding};

    #[test]
    fn failed_hardware_retains_a_working_h264_software_encoder() {
        struct RestoreName(String);
        impl Drop for RestoreName {
            fn drop(&mut self) { *hbb_common::config::APP_NAME.write().unwrap() = self.0.clone(); }
        }
        let _restore = RestoreName(hbb_common::config::APP_NAME.read().unwrap().clone());
        *hbb_common::config::APP_NAME.write().unwrap() = format!("YooDeskCodecTest-{}", std::process::id());
        std::env::set_var("YOODESK_EMBEDDED_ENGINE", "1");
        let hardware = CodecInfo { name: "h264_nonexistent_nvenc".into(), priority: Priority::Best as i32, ..Default::default() };
        let config = HwCodecConfig { ram_encode: vec![hardware], ..Default::default() };
        HwCodecConfig::set(serde_json::to_string(&config).unwrap());
        librustdesk::video_service::register_software_encoders();
        Encoder::update(EncodingUpdate::Update(78901, SupportedDecoding { ability_h264: 1, prefer: PreferCodec::H264.into(), ..Default::default() }));
        assert_eq!(HwRamEncoder::try_get(scrap::CodecFormat::H264).unwrap().name, "h264_nonexistent_nvenc");
        let make = |name| EncoderCfg::HWRAM(HwRamEncoderConfig { name, mc_name: None, width: 64, height: 64, quality: 1.0, keyframe_interval: None, fps: 144 });
        assert!(Encoder::new(make("h264_nonexistent_nvenc".into()), false).is_err());
        assert_eq!(Encoder::negotiated_codec(), scrap::CodecFormat::H264);
        assert_eq!(HwRamEncoder::try_get(scrap::CodecFormat::H264).unwrap().name, "libx264");
        assert!(Encoder::new(make("libx264".into()), false).is_ok());
        Encoder::update(EncodingUpdate::Remove(78901));
    }
}
