use std::time::{Duration, Instant};

/// Anchor each frame to the media clock so timer oversleep does not accumulate.
pub struct Pacer {
    next: Option<Instant>,
    interval: Duration,
}

impl Pacer {
    pub fn new() -> Self { Self { next: None, interval: Duration::ZERO } }

    pub fn wait(&mut self, frame_start: Instant, interval: Duration, embedded: bool) {
        if !embedded {
            if let Some(remaining) = interval.checked_sub(frame_start.elapsed()) {
                std::thread::sleep(remaining);
            }
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
