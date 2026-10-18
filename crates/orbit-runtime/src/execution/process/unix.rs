//! Unix process-group ownership.
/// Own a new process group; dropping the guard kills descendants that stay in the group.
pub struct ProcessGroup(Option<u32>);

impl ProcessGroup {
    /// Spawn a group leader with kill-on-drop enabled and return its ownership guard.
    pub fn spawn(
        command: &mut tokio::process::Command,
    ) -> std::io::Result<(tokio::process::Child, Self)> {
        let child = command
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        let group = Self(child.id());
        Ok((child, group))
    }

    /// Ask the owned group to terminate, allowing server cleanup handlers to run.
    pub fn request_termination(&self) {
        self.signal(libc::SIGTERM);
    }

    /// Kill the owned group once, including when its leader has already exited.
    pub fn kill(&mut self) {
        self.signal(libc::SIGKILL);
        self.0 = None;
    }

    fn signal(&self, signal: i32) {
        if let Some(id) = self.0 {
            // SAFETY: spawn created this positive PID as a private process group leader.
            unsafe {
                libc::kill(-(id as i32), signal);
            }
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}
