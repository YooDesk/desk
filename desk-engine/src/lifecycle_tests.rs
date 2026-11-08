//! 使用上游真实service循环验证主动切换与故障退避，不模拟编码线程调度。
use librustdesk::{PublicGenericService, PublicService};
use std::{
    ops::{Deref, DerefMut},
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Driver(PublicGenericService);
impl Deref for Driver {
    type Target = PublicGenericService;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for Driver {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn callbacks(error: &'static str, deadline: Duration) -> Option<Duration> {
    let driver = Driver(PublicGenericService::new(
        format!("lifecycle-{error}"),
        false,
    ));
    driver.on_subscribe(librustdesk::ConnInner::new(9123, None, None));
    let (tx, rx) = std::sync::mpsc::channel();
    PublicGenericService::run(&driver, move |_| {
        let _ = tx.send(Instant::now());
        Err(hbb_common::anyhow::anyhow!(error))
    });
    let end = Instant::now() + deadline;
    let mut stamps = Vec::new();
    for _ in 0..6 {
        match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok(at) => stamps.push(at),
            Err(_) => break,
        }
    }
    driver.join();
    (stamps.len() == 6).then(|| stamps[5].duration_since(stamps[0]))
}

#[test]
fn planned_switch_is_prompt_but_real_failures_keep_backoff() {
    std::env::set_var("YOODESK_EMBEDDED_ENGINE", "1");
    let planned = callbacks("SWITCH", Duration::from_millis(900));
    assert!(
        planned.is_some(),
        "normal switching must not accumulate failure backoff"
    );
    let failed = callbacks("actual capture error", Duration::from_secs(5)).unwrap();
    assert!(
        failed >= Duration::from_secs(1),
        "real capture failures must retain bounded backoff"
    );
}

// Exercise the real upstream recorder: stopping it must allow a new owned
// recorder to start, rather than leave a detached singleton between sessions.
#[test]
#[ignore = "requires an isolated graphical session"]
fn cursor_recorder_can_retire_and_restart() {
    for _ in 0..3 {
        let recorder = librustdesk::input_service::try_start_record_cursor_pos()
            .expect("previous cursor recorder must have retired");
        librustdesk::input_service::try_stop_record_cursor_pos();
        recorder.join().expect("cursor recorder must exit cleanly");
    }
}
