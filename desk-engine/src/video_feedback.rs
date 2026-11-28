//! Decode ACKs release credits immediately; QoS consumes one fresh sample per second.
use std::time::{Duration, Instant};

pub const INTERVAL: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct Feedback {
    latest: Option<u32>,
    waiting_since: Option<Instant>,
    last_ack_at: Option<Instant>,
}

impl Feedback {
    pub fn ack(&mut self, source: Instant, now: Instant) {
        self.latest = Some(now.saturating_duration_since(source).as_millis().min(u32::MAX as u128) as u32);
        self.waiting_since = None;
        self.last_ack_at = Some(now);
    }

    /// Never replay a sample: RustDesk counts consecutive replies and baseline samples.
    /// No outstanding video means an idle desktop, not a stalled viewer.
    pub fn tick(&mut self, oldest: Option<Instant>, now: Instant) -> (Option<u32>, Option<u128>) {
        let sample = self.latest.take();
        // Refresh can discard an entire reference chain without viewer progress.
        // Keep that stall's origin until a genuine ACK arrives; repeated refreshes
        // must not restart the SDK's progressively stronger timeout brake.
        let stalled = oldest.map(|at| {
            let since = self.waiting_since.get_or_insert(at);
            *since = (*since).min(at);
            // An ACK may advance a queued stream without draining it. That is
            // progress, not a missing reply; queue delay is handled by the sample.
            let progress = self.last_ack_at.map_or(*since, |ack| ack.max(*since));
            now.saturating_duration_since(progress).as_millis().min(u32::MAX as u128)
        })
            .filter(|elapsed| *elapsed > 2000);
        (sample, stalled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_rate_acks_produce_one_fresh_qos_sample_without_replay() {
        let start = Instant::now();
        let mut f = Feedback::default();
        for n in 0..144 {
            let at = start + Duration::from_millis(n * 6);
            f.ack(at, at + Duration::from_millis(25));
        }
        assert_eq!(f.tick(None, start + INTERVAL), (Some(25), None));
        assert_eq!(f.tick(None, start + INTERVAL * 2), (None, None));
    }

    #[test]
    fn missing_ack_brakes_even_when_no_more_frames_arrive() {
        let start = Instant::now();
        let mut f = Feedback::default();
        assert_eq!(f.tick(Some(start), start + INTERVAL * 2), (None, None));
        assert_eq!(f.tick(Some(start), start + INTERVAL * 3), (None, Some(3000)));
        assert_eq!(f.tick(Some(start), start + INTERVAL * 4), (None, Some(4000)));
        f.ack(start, start + INTERVAL * 4);
        assert_eq!(f.tick(None, start + INTERVAL * 5), (Some(4000), None));
        assert_eq!(f.tick(None, start + INTERVAL * 6), (None, None));
    }

    #[test]
    fn latest_ack_replaces_a_transient_spike_and_idle_never_brakes() {
        let start = Instant::now();
        let mut f = Feedback::default();
        f.ack(start, start + Duration::from_millis(800));
        f.ack(start + Duration::from_millis(900), start + Duration::from_millis(920));
        assert_eq!(f.tick(None, start + INTERVAL), (Some(20), None));
        assert_eq!(f.tick(None, start + INTERVAL * 60), (None, None));
    }

    #[test]
    fn repeated_keyframe_refreshes_cannot_hide_a_stalled_viewer() {
        let start = Instant::now();
        let mut f = Feedback::default();
        f.tick(Some(start), start + INTERVAL);
        assert_eq!(f.tick(Some(start + INTERVAL * 3), start + INTERVAL * 4), (None, Some(4000)));
        assert_eq!(f.tick(None, start + INTERVAL * 5), (None, None));
        assert_eq!(f.tick(Some(start + INTERVAL * 5), start + INTERVAL * 6), (None, Some(6000)));
        f.ack(start + INTERVAL * 6, start + INTERVAL * 6);
        assert_eq!(f.tick(Some(start + INTERVAL * 6), start + INTERVAL * 7), (Some(0), None));
    }

    #[test]
    fn progressing_queue_uses_delay_samples_instead_of_double_braking() {
        let start = Instant::now();
        let mut f = Feedback::default();
        f.tick(Some(start), start + INTERVAL);
        f.ack(start, start + INTERVAL * 3);
        assert_eq!(f.tick(Some(start), start + INTERVAL * 3), (Some(3000), None));
        assert_eq!(f.tick(Some(start), start + INTERVAL * 4), (None, None));
        assert_eq!(f.tick(Some(start), start + INTERVAL * 6), (None, Some(3000)));
        f.ack(start, start + INTERVAL * 6);
        f.tick(None, start + INTERVAL * 7);
        assert_eq!(f.tick(Some(start + INTERVAL * 60), start + INTERVAL * 61), (None, None));
    }
}
