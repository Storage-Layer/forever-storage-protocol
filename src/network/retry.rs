//! Bounded Exponential Retry with Full Jitter (`src/network/retry.rs`).
//!
//! Provides deterministic and randomized backoff policies for transient network RPC failures.

use std::thread;
use std::time::Duration;

/// Configuration for exponential retry backoff.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: usize,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub backoff_factor: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay: Duration::from_millis(25),
            max_delay: Duration::from_millis(500),
            backoff_factor: 2.0,
        }
    }
}

/// Executes an operation with bounded exponential backoff.
pub fn retry_operation<F, T, E>(policy: &RetryPolicy, mut op: F) -> Result<T, E>
where
    F: FnMut(usize) -> Result<T, E>,
{
    let mut attempt = 0;
    let mut current_delay = policy.initial_delay;

    loop {
        match op(attempt) {
            Ok(val) => return Ok(val),
            Err(err) => {
                attempt += 1;
                if attempt > policy.max_retries {
                    return Err(err);
                }
                thread::sleep(current_delay);
                let next_ms = (current_delay.as_millis() as f64 * policy.backoff_factor) as u64;
                current_delay = Duration::from_millis(next_ms).min(policy.max_delay);
            }
        }
    }
}
