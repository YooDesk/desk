#pragma once
#include <array>
#include <functional>
extern "C" {
#include <libavutil/frame.h>
#include <libavutil/buffer.h>
}

// Own exactly three independent input surfaces. FFmpeg's NVENC in_ref holds
// buf[0] until output completes; an application-only reference is writable.
class YooDeskNvencInputPool {
  std::array<AVFrame *, 3> slots_{};
  size_t next_ = 0;
public:
  YooDeskNvencInputPool() = default;
  YooDeskNvencInputPool(const YooDeskNvencInputPool &) = delete;
  YooDeskNvencInputPool &operator=(const YooDeskNvencInputPool &) = delete;
  ~YooDeskNvencInputPool() { clear(); }

  // Takes ownership of first even on allocation failure, for uniform cleanup.
  bool init(AVFrame *first, const std::function<int(AVFrame *)> &allocate) {
    if (!first || slots_[0]) return false;
    slots_[0] = first;
    for (size_t i = 1; i < slots_.size(); ++i) {
      AVFrame *f = slots_[i] = av_frame_alloc();
      if (!f) return false;
      f->format = first->format;
      f->width = first->width;
      f->height = first->height;
      if (av_frame_copy_props(f, first) < 0 || allocate(f) < 0) return false;
    }
    return true;
  }
  AVFrame *acquire() {
    for (size_t offset = 0; offset < slots_.size(); ++offset) {
      size_t index = (next_ + offset) % slots_.size();
      AVFrame *f = slots_[index];
      if (f && f->buf[0] && av_buffer_get_ref_count(f->buf[0]) == 1) {
        next_ = (index + 1) % slots_.size();
        return f;
      }
    }
    return nullptr;
  }
  bool owns(const AVFrame *f) const {
    if (!f) return false;
    for (auto slot : slots_) if (slot == f) return true;
    return false;
  }
  void clear() {
    for (auto &slot : slots_) av_frame_free(&slot);
    next_ = 0;
  }
};
