//! A bounded reconnect budget. A handshake alone does not establish recovery;
//! history must finish loading, and a stable connection replenishes the budget.
use std::time::{Duration, Instant};

const MAX_RECONNECT_ATTEMPTS: u32 = 5;
const HEALTHY_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct Recovery {
    attempts: u32,
    active: bool,
    healthy_since: Option<Instant>,
}

impl Recovery {
    pub fn disconnected(&mut self, now: Instant) -> Option<Duration> {
        if self
            .healthy_since
            .take()
            .is_some_and(|since| now.duration_since(since) >= HEALTHY_INTERVAL)
        {
            self.attempts = 0;
        }
        if self.attempts >= MAX_RECONNECT_ATTEMPTS {
            self.active = false;
            return None;
        }
        let delay = Duration::from_secs(1 << self.attempts);
        self.attempts += 1;
        self.active = true;
        Some(delay)
    }

    pub fn restored(&mut self, now: Instant) {
        self.active = false;
        self.healthy_since.get_or_insert(now);
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn label(&self) -> String {
        format!("正在重新连接（{}/{MAX_RECONNECT_ATTEMPTS}）", self.attempts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_handshakes_cannot_restart_the_retry_budget() {
        let now = Instant::now();
        let mut recovery = Recovery::default();
        for seconds in [1, 2, 4, 8, 16] {
            assert_eq!(
                recovery.disconnected(now),
                Some(Duration::from_secs(seconds))
            );
            recovery.restored(now);
        }
        assert_eq!(recovery.disconnected(now), None);
        assert!(!recovery.is_active());
        recovery.restored(now);
        assert_eq!(
            recovery.disconnected(now + Duration::from_secs(30)),
            Some(Duration::from_secs(1))
        );
    }
}
