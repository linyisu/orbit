use super::*;

impl Tab {
    pub(super) fn schedule_reconnection(&mut self) -> bool {
        if self.id.is_none() || self.connection.is_none() {
            return false;
        }
        let delay = Duration::from_millis(250 * (1u64 << self.reconnect_attempts.min(5)));
        self.reconnect_at = Some(std::time::Instant::now() + delay);
        self.reconnect_attempts = self
            .reconnect_attempts
            .saturating_add(1);
        true
    }

    pub(super) fn poll_reconnection(&mut self) -> bool {
        if self.connecting.is_some()
            || self
                .reconnect_at
                .is_none_or(|at| at > std::time::Instant::now())
        {
            return false;
        }
        self.begin_reconnection(true);
        true
    }
}
