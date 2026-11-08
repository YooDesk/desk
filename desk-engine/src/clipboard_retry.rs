//! 系统剪贴板短暂占用时只重试读取；不重放可能已经生效的写入。
pub(crate) fn read_with_retry<T, E>(
    mut read: impl FnMut() -> Result<T, E>,
    is_busy: impl Fn(&E) -> bool,
) -> Result<T, E> {
    for attempt in 0..6 {
        match read() {
            Err(error) if attempt < 5 && is_busy(&error) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transient_busy_read_recovers_without_losing_content() {
        let mut calls = 0;
        let value = read_with_retry(
            || {
                calls += 1;
                if calls < 3 {
                    Err("busy")
                } else {
                    Ok("content")
                }
            },
            |e| *e == "busy",
        );
        assert_eq!(value, Ok("content"));
        assert_eq!(calls, 3);
    }
    #[test]
    fn permanent_busy_is_bounded_and_other_errors_are_not_retried() {
        let mut calls = 0;
        let value: Result<(), _> = read_with_retry(
            || {
                calls += 1;
                Err("busy")
            },
            |e| *e == "busy",
        );
        assert_eq!(value, Err("busy"));
        assert_eq!(calls, 6);
        calls = 0;
        let value: Result<(), _> = read_with_retry(
            || {
                calls += 1;
                Err("denied")
            },
            |e| *e == "busy",
        );
        assert_eq!(value, Err("denied"));
        assert_eq!(calls, 1);
    }
}
