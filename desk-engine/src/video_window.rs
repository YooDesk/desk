//! Bound SDK fetch credits independently from true browser decode acknowledgments.
use std::{collections::VecDeque, time::Instant};

// H.264 permits up to16 DPB pictures. Leave two slots for decode/output work;
// a zero-reorder decoder normally uses only one, rather than filling the window.
pub const MAX_FRAMES: usize = 18;
const MAX_BYTES: usize = 32 << 20;

struct Pending { pts: i64, at: Instant, bytes: usize, fetched: bool }

#[derive(Default)]
pub struct Window {
    pending: VecDeque<Pending>,
    bytes: usize,
    blocked: bool,
}

pub struct Ack { pub at: Instant, pub release_sdk: bool }

impl Window {
    // SDK receive credit can advance after taking bounded ownership, before
    // the host polls. Decoded ACKs still require a real host fetch below.
    pub fn admit(&mut self, pts: i64, at: Instant, bytes: usize) -> Option<bool> {
        if self.pending.len() >= MAX_FRAMES || bytes > MAX_BYTES.saturating_sub(self.bytes) { return None; }
        if self.pending.back().is_some_and(|p| pts <= p.pts) { return None; }
        self.pending.push_back(Pending { pts, at, bytes, fetched: false });
        self.bytes += bytes;
        let room = self.pending.len() < MAX_FRAMES && self.bytes < MAX_BYTES;
        self.blocked = !room;
        Some(room)
    }

    pub fn fetched(&mut self, pts: i64) {
        if let Some(pending) = self.pending.iter_mut().find(|p| p.pts == pts) {
            pending.fetched = true;
        }
    }

    pub fn ack(&mut self, pts: i64) -> Option<Ack> {
        let index = self.pending.iter().position(|p| p.pts == pts && p.fetched)?;
        if self.pending.iter().take(index + 1).any(|p| !p.fetched) { return None; }
        let at = self.pending[index].at;
        for _ in 0..=index { self.bytes -= self.pending.pop_front().unwrap().bytes; }
        let release_sdk = self.blocked && self.pending.len() < MAX_FRAMES && self.bytes < MAX_BYTES;
        if release_sdk { self.blocked = false; }
        Some(Ack { at, release_sdk })
    }

    pub fn only(&self, pts: i64) -> bool { self.pending.len() == 1 && self.pending[0].pts == pts }
    pub fn clear(&mut self) { self.pending.clear(); self.bytes = 0; self.blocked = false; }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffered_decoder_can_receive_its_initial_pictures_without_claiming_decode() {
        let mut window = Window::default();
        for pts in 0..16 {
            assert_eq!(window.admit(pts, Instant::now(), 1000), Some(true));
            assert!(window.ack(pts).is_none());
            window.fetched(pts);
        }
        assert_eq!(window.pending.len(), 16); // Fetch is not a decoded-frame ACK.
        assert!(!window.ack(0).unwrap().release_sdk); // Credit was already granted once.
        assert_eq!(window.pending.len(), 15);
    }

    #[test]
    fn slow_viewer_is_bounded_and_only_real_fetched_acks_release_credit_once() {
        let mut window = Window::default();
        for pts in 0..MAX_FRAMES as i64 {
            assert_eq!(window.admit(pts, Instant::now(), 1000), Some(pts + 1 < MAX_FRAMES as i64));
            assert!(window.admit(pts, Instant::now(), 1000).is_none());
            window.fetched(pts);
            window.fetched(pts);
        }
        assert!(window.admit(100, Instant::now(), 1).is_none());
        assert!(window.ack(100).is_none());
        assert!(window.ack(2).unwrap().release_sdk);
        assert!(window.ack(2).is_none());
        assert!(!window.ack(3).unwrap().release_sdk);
        assert_eq!(window.admit(100, Instant::now(), 1), Some(true));
        assert!(window.ack(100).is_none()); // Not fetched, even if its timestamp is guessed.
    }

    #[test]
    fn byte_limit_and_refresh_invalidate_pending_credits() {
        let mut window = Window::default();
        assert_eq!(window.admit(1, Instant::now(), MAX_BYTES), Some(false));
        window.fetched(1);
        assert!(window.admit(2, Instant::now(), 1).is_none());
        window.clear();
        assert!(window.ack(1).is_none());
        assert_eq!(window.admit(2, Instant::now(), 1), Some(true));
        window.fetched(2);
    }
}
