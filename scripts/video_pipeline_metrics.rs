use std::time::{Duration, Instant};

pub struct Metrics {
    enabled: bool,
    start: Instant,
    captured: u64,
    encoded: u64,
    capture_us: u128,
    encode_us: u128,
}

impl Metrics {
    pub fn new() -> Self {
        Self { enabled: std::env::var_os("YOODESK_VIDEO_PIPELINE_DIAGNOSTICS").is_some(), start: Instant::now(), captured: 0, encoded: 0, capture_us: 0, encode_us: 0 }
    }
    pub fn capture(&mut self, elapsed: Duration) {
        self.captured += 1;
        self.capture_us += elapsed.as_micros();
    }
    pub fn encode(&mut self, elapsed: Duration, produced: bool) {
        self.encode_us += elapsed.as_micros();
        self.encoded += u64::from(produced);
    }
    pub fn report(&mut self, display: usize, hardware: bool) {
        let elapsed = self.start.elapsed().as_secs_f64();
        if !self.enabled || elapsed < 1.0 { return; }
        eprintln!("Desk pipeline: display={} captured_fps={:.2} encoded_fps={:.2} capture_ms={:.3} encode_ms={:.3} hardware={}", display, self.captured as f64 / elapsed, self.encoded as f64 / elapsed, self.capture_us as f64 / self.captured.max(1) as f64 / 1000.0, self.encode_us as f64 / self.captured.max(1) as f64 / 1000.0, hardware);
        self.start = Instant::now(); self.captured = 0; self.encoded = 0; self.capture_us = 0; self.encode_us = 0;
    }
}
