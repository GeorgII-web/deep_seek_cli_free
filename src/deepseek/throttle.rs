//! Keeps the request rate low so the account is not flagged. The state is a file,
//! because every `dsc` call is a new process.

use crate::config::{Paths, Throttle as Settings};
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HOUR_MS: u64 = 3_600_000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// How long to wait before the next request, given the send times (ms) of earlier ones.
pub fn delay_ms(settings: &Settings, sent: &[u64], now: u64, jitter: u64) -> u64 {
    let mut wait = 0;
    if let Some(last) = sent.iter().max() {
        let gap = settings.min_interval_ms + jitter;
        wait = (last + gap).saturating_sub(now);
    }
    let in_hour: Vec<u64> = sent.iter().copied().filter(|t| t + HOUR_MS > now).collect();
    if settings.max_per_hour > 0 && in_hour.len() as u64 >= settings.max_per_hour as u64 {
        // Wait until the oldest request that still counts leaves the hour window.
        let mut sorted = in_hour;
        sorted.sort_unstable();
        let blocking = sorted[sorted.len() - settings.max_per_hour as usize];
        wait = wait.max((blocking + HOUR_MS).saturating_sub(now));
    }
    wait
}

/// Waits as the settings ask, then records the request. `on_wait` is told the
/// time before a wait starts.
pub async fn acquire(paths: &Paths, settings: &Settings, on_wait: &mut dyn FnMut(Duration)) {
    let file = paths.cache().join("throttle.json");
    let now = now_ms();
    let mut sent: Vec<u64> = fs::read_to_string(&file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    sent.retain(|t| t + HOUR_MS > now && *t <= now);

    let jitter = match settings.jitter_ms {
        0 => 0,
        // Not security relevant: the clock's sub-second part is random enough.
        max => {
            (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0) as u64)
                % (max + 1)
        }
    };
    let wait = Duration::from_millis(delay_ms(settings, &sent, now, jitter));
    if !wait.is_zero() {
        on_wait(wait);
        tokio::time::sleep(wait).await;
    }
    sent.push(now_ms());
    let _ = fs::create_dir_all(paths.cache());
    let _ = fs::write(&file, serde_json::to_string(&sent).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(min_interval_ms: u64, max_per_hour: u32) -> Settings {
        Settings {
            min_interval_ms,
            jitter_ms: 0,
            max_per_hour,
            ..Settings::default()
        }
    }

    #[test]
    fn first_request_does_not_wait() {
        assert_eq!(delay_ms(&settings(3000, 10), &[], 1_000_000, 0), 0);
    }

    #[test]
    fn gap_and_jitter_after_the_last_request() {
        let s = settings(3000, 0);
        assert_eq!(delay_ms(&s, &[10_000], 11_000, 0), 2000);
        assert_eq!(delay_ms(&s, &[10_000], 11_000, 500), 2500);
        assert_eq!(delay_ms(&s, &[10_000], 20_000, 500), 0);
    }

    #[test]
    fn hour_cap_waits_for_the_oldest_to_expire() {
        let s = settings(0, 2);
        let now = 10 * HOUR_MS;
        let sent = [now - 50 * 60_000, now - 10 * 60_000];
        assert_eq!(delay_ms(&s, &sent, now, 0), 10 * 60_000);
        // A request older than an hour does not count.
        assert_eq!(
            delay_ms(&s, &[now - 70 * 60_000, now - 10 * 60_000], now, 0),
            0
        );
    }
}
