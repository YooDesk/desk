use std::time::Duration;

// DXGI accepts whole milliseconds. Do not truncate a 144 Hz wait to 6 ms.
pub fn millis(timeout: Duration) -> u32 {
    timeout.as_nanos().div_ceil(1_000_000).min((u32::MAX - 1) as u128) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_waits_round_up_while_nonblocking_polls_remain_zero() {
        assert_eq!(millis(Duration::ZERO), 0);
        assert_eq!(millis(Duration::from_nanos(1)), 1);
        assert_eq!(millis(Duration::from_secs_f32(1.0 / 144.0)), 7);
        assert_eq!(millis(Duration::from_secs_f32(1.0 / 60.0)), 17);
        assert_eq!(millis(Duration::from_millis(50)), 50);
    }
    #[test]
    fn huge_timeouts_cannot_wrap_or_become_the_infinite_wait_sentinel() {
        assert_eq!(millis(Duration::MAX), u32::MAX - 1);
    }
}
