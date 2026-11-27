"""Bound the FFmpeg NVENC input lifetime while overlapping one GPU frame."""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def patch_nvenc_pipeline(tree: Path) -> None:
    source = tree / "cpp/ffmpeg_vram/ffmpeg_vram_encode.cpp"
    text = source.read_text(encoding="utf-8")

    def replace(old, new):
        nonlocal text
        if text.count(old) != 1:
            raise RuntimeError("Review NVENC frame-pool adapter: " + old[:70])
        text = text.replace(old, new, 1)

    text = '#include "yoodesk_nvenc_input_pool.h"\n' + text
    replace('  bool has_frame_ = false;', '''  bool has_frame_ = false;
  bool pipelined_ = false;
  YooDeskNvencInputPool input_pool_;''')
    replace('    util_encode::set_others(c_->priv_data, encoder_->name_);', '''    util_encode::set_others(c_->priv_data, encoder_->name_);
    pipelined_ = encoder_->driver_ == EncoderDriver::NVENC && dataFormat_ == H264 && framerate_ > 60;
    // FFmpeg delay=2 retains one pending input (delay=1 is still synchronous).
    // It does not introduce B pictures or decoder reordering.
    if (pipelined_ && av_opt_set_int(c_->priv_data, "delay", 2, 0) < 0) return false;''')
    replace('      frames_ctx->initial_pool_size = 1;', '''      // RENDER_TARGET below selects separate textures with array slice zero.
      // Reserve metadata for exactly three application-owned inputs.
      frames_ctx->initial_pool_size = pipelined_ ? 3 : 1;''')
    replace('''    return true;
  }

  int encode(void *texture''', '''    if (pipelined_) {
      if (!input_pool_.init(frame_, [this](AVFrame *f) {
            return av_hwframe_get_buffer(c_->hw_frames_ctx, f, 0);
          })) return false;
      LOG_INFO(std::string("bounded NVENC pipeline: slots=3 pending=1"));
    }
    return true;
  }

  int encode(void *texture''')
    replace('''    if (texture) {
      if (!convert(texture)) return -1;''', '''    AVFrame *previous = frame_;
    if (pipelined_) {
      AVFrame *writable = input_pool_.acquire();
      if (!writable) {
        LOG_ERROR(std::string("NVENC input pool exhausted"));
        return AVERROR(ENOMEM);
      }
      frame_ = writable;
      encode_texture_ = reinterpret_cast<ID3D11Texture2D *>(frame_->data[0]);
      if (!texture && has_frame_ && previous != frame_) {
        // Static refresh copies only encoder-owned NV12, never a released
        // DXGI pointer or a surface still being written by the application.
        native_->context_->CopyResource(encode_texture_,
            reinterpret_cast<ID3D11Texture2D *>(previous->data[0]));
      }
    }
    if (texture) {
      if (!convert(texture)) return -1;''')
    replace('''    if (frame_)
      av_frame_free(&frame_);''', '''    if (input_pool_.owns(frame_)) {
      frame_ = nullptr;
      input_pool_.clear();
    } else if (frame_) {
      av_frame_free(&frame_);
    }''')
    replace('    return encoded ? 0 : -1;', '''    // An accepted input can legitimately precede its output by one frame.
    return encoded || (pipelined_ && ret == AVERROR(EAGAIN)) ? 0 : -1;''')
    source.write_text(text, encoding="utf-8")
    (source.parent / "yoodesk_nvenc_input_pool.h").write_bytes(
        (ROOT / "engine-patches/nvenc_input_pool.h").read_bytes())
