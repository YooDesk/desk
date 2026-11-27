//! Wait for udev/logind to apply access to this newly created virtual device.
use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};

pub(super) fn open_created_event(path: &Path, nonblocking: i32) -> io::Result<File> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match OpenOptions::new()
            .read(true)
            .custom_flags(nonblocking)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waits_for_device_creation_and_returns_other_errors_immediately() {
        let dir = std::env::temp_dir().join(format!(
            "yoodesk-event-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("event");
        let created = path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            std::fs::write(created, b"ready").unwrap();
        });
        let file = open_created_event(&path, 0).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 5);
        writer.join().unwrap();
        let invalid = path.join("child");
        let start = Instant::now();
        assert!(open_created_event(&invalid, 0).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
