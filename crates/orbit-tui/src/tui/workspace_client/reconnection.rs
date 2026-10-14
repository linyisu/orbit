use super::*;

impl Tab {
    pub(super) fn schedule_reconnection(&mut self) -> bool {
        if self.id.is_none() || self.connection.is_none() {
            return false;
        }
        self.reconnect_at = self
            .recovery
            .disconnected(std::time::Instant::now())
            .map(|delay| std::time::Instant::now() + delay);
        self.reconnect_at.is_some()
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
