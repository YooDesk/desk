#include "../engine-patches/nvenc_input_pool.h"
#undef NDEBUG
#include <cassert>
extern "C" {
#include <libavutil/pixfmt.h>
}

static int allocate(AVFrame *f) {
  f->buf[0] = av_buffer_alloc(32);
  if (!f->buf[0]) return -1;
  f->data[0] = f->buf[0]->data;
  f->linesize[0] = 4;
  return 0;
}
static AVFrame *first_frame() {
  AVFrame *f = av_frame_alloc();
  assert(f);
  f->format = AV_PIX_FMT_GRAY8;
  f->width = f->height = 4;
  assert(allocate(f) == 0);
  return f;
}
int main() {
  YooDeskNvencInputPool pool;
  assert(pool.init(first_frame(), allocate));
  AVFrame *slots[3] = {}, *inflight[3] = {};
  for (int i = 0; i < 3; ++i) {
    slots[i] = pool.acquire();
    assert(slots[i] && pool.owns(slots[i]));
    for (int j = 0; j < i; ++j) assert(slots[i] != slots[j]);
    // The same reference FFmpeg NVENC retains until output completes.
    inflight[i] = av_frame_clone(slots[i]);
    assert(inflight[i]);
    assert(av_buffer_get_ref_count(slots[i]->buf[0]) == 2);
  }
  assert(!pool.acquire()); // Bounded exhaustion never overwrites an input.
  av_frame_free(&inflight[1]);
  assert(pool.acquire() == slots[1]);
  assert(av_buffer_get_ref_count(slots[1]->buf[0]) == 1);
  av_frame_free(&inflight[0]);
  av_frame_free(&inflight[2]);
  pool.clear();
  pool.clear();
  assert(!pool.acquire());
  // Partial initialization is still owned and safely reclaimable.
  assert(!pool.init(first_frame(), [](AVFrame *) { return -1; }));
  pool.clear();
  return 0;
}
