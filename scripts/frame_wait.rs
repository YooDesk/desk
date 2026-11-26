use std::collections::HashSet;
use std::time::Instant;

pub fn budget(embedded: bool) -> u64 {
    if embedded { 250 } else { 3_000 }
}

pub fn wait_for_frames<E>(
    expected: &HashSet<i32>,
    fetched: &mut HashSet<i32>,
    timeout_millis: u64,
    mut before_wait: impl FnMut() -> Result<(), E>,
    mut poll: impl FnMut(&mut HashSet<i32>, u64),
) -> Result<(), E> {
    let begin = Instant::now();
    while begin.elapsed().as_millis() < timeout_millis as u128 {
        before_wait()?;
        let remaining = timeout_millis.saturating_sub(begin.elapsed().as_millis() as u64);
        if remaining == 0 { break; }
        poll(fetched, remaining.min(300));
        fetched.retain(|id| expected.contains(id));
        if fetched.len() >= expected.len() { break; }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_missing_viewer_ack_cannot_stall_healthy_viewers_or_be_fabricated() {
        let (tx, rx) = mpsc::channel();
        tx.send(99).unwrap(); // not a recipient of this frame
        tx.send(1).unwrap();
        let mut fetched = HashSet::new();
        let begin = Instant::now();
        wait_for_frames(&HashSet::from([1, 2]), &mut fetched, budget(true), || Ok::<_, ()>(()),
            |fetched, remaining| {
                if let Ok(id) = rx.recv_timeout(Duration::from_millis(remaining)) { fetched.insert(id); }
            }).unwrap();
        assert!(begin.elapsed() >= Duration::from_millis(200));
        assert!(begin.elapsed() < Duration::from_millis(750));
        assert_eq!(fetched, HashSet::from([1]));
    }

    #[test]
    fn all_real_acks_finish_immediately_and_privacy_errors_remain_fatal() {
        let (tx, rx) = mpsc::channel();
        tx.send(1).unwrap(); tx.send(2).unwrap();
        let mut fetched = HashSet::new();
        let begin = Instant::now();
        wait_for_frames(&HashSet::from([1, 2]), &mut fetched, budget(true), || Ok::<_, ()>(()),
            |fetched, remaining| {
                if let Ok(id) = rx.recv_timeout(Duration::from_millis(remaining)) { fetched.insert(id); }
            }).unwrap();
        assert!(begin.elapsed() < Duration::from_millis(200));
        assert_eq!(fetched, HashSet::from([1, 2]));
        let mut fetched = HashSet::new();
        assert!(wait_for_frames(&HashSet::from([1]), &mut fetched, 250,
            || Err("privacy changed"), |_, _| panic!("wait continued after privacy failure")).is_err());
        assert!(fetched.is_empty());
        assert_eq!(budget(false), 3_000);
    }
}
