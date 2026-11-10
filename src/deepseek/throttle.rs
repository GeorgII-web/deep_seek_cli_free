//! Keeps the request rate low so the account is not flagged. The state is a file,
//! because every `dsc` call is a new process.

use crate::config::{Paths, Throttle as Settings};
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HOUR_MS: u64 = 3_600_000;
const MINUTE_MS: u64 = 60_000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// What is remembered between runs (`<home>/cache/throttle.json`).
#[derive(Default, Serialize, Deserialize)]
struct State {
    /// Send times (ms) of the last hour
    sent: Vec<u64>,
    /// After a "too many requests" answer: no request before this time (ms)
    #[serde(default)]
    cooldown_until: u64,
    /// When the token last proved to work (ms): a check or an answered request
    #[serde(default)]
    token_ok_at: u64,
}

fn file(paths: &Paths) -> std::path::PathBuf {
    paths.cache().join("throttle.json")
}

fn load(paths: &Paths) -> State {
    let text = fs::read_to_string(file(paths)).unwrap_or_default();
    // The first versions stored only the list of send times.
    serde_json::from_str::<State>(&text)
        .or_else(|_| {
            serde_json::from_str::<Vec<u64>>(&text).map(|sent| State {
                sent,
                ..State::default()
            })
        })
        .unwrap_or_default()
}

fn save(paths: &Paths, state: &State) {
    let _ = fs::create_dir_all(paths.cache());
    let _ = fs::write(
        file(paths),
        serde_json::to_string(state).unwrap_or_default(),
    );
}

/// Wait until fewer than `max` of `sent` lie inside the last `window` ms; 0 = no cap.
fn window_wait(sent: &[u64], now: u64, window: u64, max: u32) -> u64 {
    let mut inside: Vec<u64> = sent.iter().copied().filter(|t| t + window > now).collect();
    if max == 0 || inside.len() < max as usize {
        return 0;
    }
    // The oldest request that still counts must leave the window.
    inside.sort_unstable();
    let blocking = inside[inside.len() - max as usize];
    (blocking + window).saturating_sub(now)
}

/// How long to wait before the next request, given the send times (ms) of earlier ones.
pub fn delay_ms(settings: &Settings, sent: &[u64], now: u64, jitter: u64) -> u64 {
    let gap = match sent.iter().max() {
        Some(last) => (last + settings.min_interval_ms + jitter).saturating_sub(now),
        None => 0,
    };
    gap.max(window_wait(sent, now, MINUTE_MS, settings.max_per_minute))
        .max(window_wait(sent, now, HOUR_MS, settings.max_per_hour))
}

/// Waits as the settings ask (gap, per-minute and per-hour caps, a cooldown left
/// by an earlier "too many requests"), then records the request. `on_wait` is
/// told the time before a wait starts.
pub async fn acquire(paths: &Paths, settings: &Settings, on_wait: &mut dyn FnMut(Duration)) {
    let now = now_ms();
    let mut state = load(paths);
    state.sent.retain(|t| t + HOUR_MS > now && *t <= now);

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
    let cooldown = state.cooldown_until.saturating_sub(now);
    let wait = Duration::from_millis(delay_ms(settings, &state.sent, now, jitter).max(cooldown));
    if !wait.is_zero() {
        on_wait(wait);
        tokio::time::sleep(wait).await;
    }
    state.sent.push(now_ms());
    save(paths, &state);
}

/// Notes that the token just worked, so the next start need not ask DeepSeek again.
pub fn token_ok(paths: &Paths) {
    let mut state = load(paths);
    state.token_ok_at = now_ms();
    save(paths, &state);
}

/// Whether the token worked within the last `secs` seconds.
pub fn token_ok_within(paths: &Paths, secs: u64) -> bool {
    let at = load(paths).token_ok_at;
    at > 0 && now_ms().saturating_sub(at) < secs * 1000
}

/// Remembers that DeepSeek asked to slow down: no request for `secs`, also in a later run.
pub fn cooldown(paths: &Paths, secs: u64) {
    let mut state = load(paths);
    state.cooldown_until = now_ms() + secs * 1000;
    save(paths, &state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(min_interval_ms: u64, max_per_hour: u32) -> Settings {
        Settings {
            min_interval_ms,
            jitter_ms: 0,
            max_per_minute: 0,
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

    #[test]
    fn minute_cap_spreads_a_burst() {
        let s = Settings {
            min_interval_ms: 0,
            jitter_ms: 0,
            max_per_minute: 3,
            max_per_hour: 0,
            ..Settings::default()
        };
        let now = 10 * HOUR_MS;
        // Three requests in the last 30 s: the fourth waits until the first is a minute old.
        let sent = [now - 30_000, now - 20_000, now - 10_000];
        assert_eq!(delay_ms(&s, &sent, now, 0), 30_000);
        assert_eq!(delay_ms(&s, &sent[1..], now, 0), 0);
    }

    #[test]
    fn old_state_file_is_read_and_cooldown_is_kept() {
        let paths = Paths {
            home: std::env::temp_dir().join(format!("dsc-throttle-{}", std::process::id())),
        };
        fs::create_dir_all(paths.cache()).unwrap();
        fs::write(file(&paths), "[1,2,3]").unwrap();
        assert_eq!(load(&paths).sent, [1, 2, 3]);
        cooldown(&paths, 90);
        let state = load(&paths);
        assert_eq!(state.sent, [1, 2, 3]);
        let left = state.cooldown_until - now_ms();
        assert!((80_000..=90_000).contains(&left), "{left}");
        assert!(!token_ok_within(&paths, 60));
        token_ok(&paths);
        assert!(token_ok_within(&paths, 60));
        assert_eq!(load(&paths).sent, [1, 2, 3]);
        fs::remove_dir_all(paths.home).unwrap();
    }
}
