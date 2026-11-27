#[derive(Debug, PartialEq, Eq)]
pub enum Output { Ready, Buffered, Stalled }

// Accept bounded encoder warm-up without publishing a nonexistent video frame.
pub fn observe(has_packet: bool, consecutive_failures: &mut usize) -> Output {
    if has_packet {
        *consecutive_failures = 0;
        Output::Ready
    } else {
        *consecutive_failures += 1;
        if *consecutive_failures >= 3 { Output::Stalled } else { Output::Buffered }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_output_warms_up_then_resets_the_failure_budget() {
        let mut failures = 0;
        assert_eq!(observe(false, &mut failures), Output::Buffered);
        assert_eq!(observe(true, &mut failures), Output::Ready);
        assert_eq!(failures, 0);
        assert_eq!(observe(false, &mut failures), Output::Buffered);
    }
    #[test]
    fn an_encoder_without_output_cannot_warm_up_forever() {
        let mut failures = 0;
        assert_eq!(observe(false, &mut failures), Output::Buffered);
        assert_eq!(observe(false, &mut failures), Output::Buffered);
        assert_eq!(observe(false, &mut failures), Output::Stalled);
    }
}
