"""开放流描述构造与活动用户缓存刷新API；授权/持久化业务位于YooDesk适配层。"""

from pathlib import Path


def patch(tree: Path) -> None:
    """在隔离构建树添加薄API，不修改上游Portal流程、鉴权判定或参考仓库。"""
    path = tree / "libs/scrap/src/wayland/pipewire.rs"
    text = path.read_text(encoding="utf-8")
    anchor = "impl PwStreamInfo {"
    addition = """impl PwStreamInfo {
    /// 中文注释：允许嵌入端传入标准Portal已授权流描述，采集/编码仍由scrap负责。
    pub fn from_parts(path: u64, source_type: u64, position: (i32, i32), size: (usize, usize)) -> Self {
        Self { path, source_type, position, size }
    }
"""
    if addition not in text:
        if text.count(anchor) != 1:
            raise RuntimeError(
                "PwStreamInfo API changed; review the thin constructor adapter"
            )
        path.write_text(text.replace(anchor, addition, 1), encoding="utf-8")
    # 嵌入宿主自己管理服务生命期，但DRM逐帧peercred仍依赖上游活动UID缓存。
    # 仅公开一次刷新入口，不修改鉴权判定，也不启动RustDesk的完整服务/账号角色。
    path = tree / "src/platform/linux.rs"
    text = path.read_text(encoding="utf-8")
    anchor = "fn update_active_user_lookup_cache(desktop: &Desktop) {"
    api = """pub fn refresh_active_user_lookup_cache() {
    let values = get_values_of_seat0_with_gdm_wayland(&[1, 2]);
    let desktop = Desktop { uid: values[0].clone(), username: values[1].clone(), ..Default::default() };
    update_active_user_lookup_cache(&desktop);
}

"""
    if api not in text:
        if text.count(anchor) != 1:
            raise RuntimeError(
                "Active user cache API changed; review the metadata adapter"
            )
        path.write_text(text.replace(anchor, api + anchor, 1), encoding="utf-8")
    # 有真实新帧证明健康的客户端IDR请求不应被统计为硬件故障重建。
    # 保留零帧失败、demotion及其他真实失败状态；业务层负责验证最近确实收到有效帧。
    path = tree / "src/server/drm_capturer.rs"
    text = path.read_text(encoding="utf-8")
    anchor = "pub(super) fn get_capturer_info("
    api = """pub(crate) fn note_healthy_client_refresh(display_idx: usize) {
    if let Some(info) = display_info_of(display_idx as i32) {
        if let Some(health) = DRM_DISPLAY_HEALTH.lock().unwrap().get_mut(&connector_key(&info)) {
            if health.zero_frame_streak == 0 && !health.demoted() {
                health.rapid_builds = 0;
                health.last_build = None;
            }
        }
    }
}

"""
    if api not in text:
        if text.count(anchor) != 1:
            raise RuntimeError("DRM health metadata API changed; review the adapter")
        path.write_text(text.replace(anchor, api + anchor, 1), encoding="utf-8")
    path = tree / "src/server.rs"
    text = path.read_text(encoding="utf-8")
    api = """
#[cfg(all(target_os = "linux", feature = "drm"))]
pub fn yoodesk_drm_note_healthy_client_refresh(display_idx: usize) {
    drm_capturer::note_healthy_client_refresh(display_idx);
}
"""
    if api not in text:
        path.write_text(text + api, encoding="utf-8")
