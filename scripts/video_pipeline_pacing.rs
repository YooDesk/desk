use std::time::{Duration, Instant};

/// Anchor each frame to the media clock so timer oversleep does not accumulate.
pub struct Pacer {
    next: Option<Instant>,
    interval: Duration,
    #[cfg(windows)]
    _priority: Option<capture_priority::Guard>,
}

impl Pacer {
    pub fn new(high_fps: bool) -> Self {
        #[cfg(not(windows))]
        let _ = high_fps;
        Self {
            next: None, interval: Duration::ZERO,
            #[cfg(windows)]
            _priority: if high_fps && std::env::var("YOODESK_EMBEDDED_ENGINE").as_deref() == Ok("1") {
                capture_priority::Guard::new()
            } else { None },
        }
    }

    pub fn wait(&mut self, frame_start: Instant, interval: Duration, embedded: bool, source_rate: Option<f64>) {
        if !embedded {
            if let Some(remaining) = interval.checked_sub(frame_start.elapsed()) {
                std::thread::sleep(remaining);
            }
            return;
        }
        if source_clock_satisfies_cap(source_rate, interval) {
            // AcquireNextFrame already waits for this display's refresh. A
            // second CPU timer can miss a refresh through scheduling jitter.
            self.next = None;
            self.interval = interval;
            return;
        }
        let due = if self.interval != interval {
            frame_start + interval
        } else {
            self.next.unwrap_or(frame_start + interval)
        };
        if let Some(remaining) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(remaining);
        }
        self.next = Some(next_deadline(due, Instant::now(), interval));
        self.interval = interval;
    }
}

#[cfg(windows)]
mod capture_priority { include!("video_capture_priority.rs"); }

fn source_clock_satisfies_cap(source_rate: Option<f64>, interval: Duration) -> bool {
    if interval.is_zero() { return false; }
    source_rate.is_some_and(|hz| hz.is_finite() && hz > 0.0 && hz <= 1.0 / interval.as_secs_f64() + 0.01)
}

fn next_deadline(due: Instant, now: Instant, interval: Duration) -> Instant {
    if now.saturating_duration_since(due) > interval {
        // An encoder stall is not a reason to burst stale frames into the viewer.
        now + interval
    } else {
        due + interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_refresh_paces_only_when_it_respects_the_requested_cap() {
        let cap144 = Duration::from_secs_f64(1.0 / 144.0);
        assert!(source_clock_satisfies_cap(Some(144.001), cap144));
        assert!(source_clock_satisfies_cap(Some(60.0), cap144));
        assert!(!source_clock_satisfies_cap(Some(240.0), cap144));
        assert!(!source_clock_satisfies_cap(Some(144.0), Duration::from_secs_f64(1.0 / 60.0)));
        for rate in [None, Some(0.0), Some(f64::NAN), Some(f64::INFINITY)] {
            assert!(!source_clock_satisfies_cap(rate, cap144));
        }
        assert!(!source_clock_satisfies_cap(Some(144.0), Duration::ZERO));
    }

    #[test]
    fn timer_oversleep_does_not_accumulate_into_a_lower_frame_rate() {
        let start = Instant::now();
        let interval = Duration::from_nanos(1_000_000_000 / 144);
        let mut due = start + interval;
        for _ in 0..1440 {
            due = next_deadline(due, due + Duration::from_micros(100), interval);
        }
        assert_eq!(due, start + interval * 1441);
    }

    #[test]
    fn a_stall_rebases_the_clock_without_a_catchup_burst() {
        let due = Instant::now();
        let interval = Duration::from_millis(7);
        let now = due + Duration::from_millis(300);
        assert_eq!(next_deadline(due, now, interval), now + interval);
    }
}
