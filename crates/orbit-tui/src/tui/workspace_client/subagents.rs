use tokio::sync::oneshot;

use super::*;

const POLL_INTERVAL: Duration = Duration::from_millis(150);

pub(super) struct Remote {
    agent: String,
    after: usize,
    next_poll: std::time::Instant,
    pending: Option<oneshot::Receiver<Result<Response>>>,
    submission: Option<oneshot::Receiver<Result<Response>>>,
    failed: bool,
}

fn request(
    commands: &mpsc::Sender<Call>,
    request: Request,
) -> Result<oneshot::Receiver<Result<Response>>> {
    let (reply, response) = oneshot::channel();
    commands.try_send(Call {
        request,
        reply: Some(reply),
    })?;
    Ok(response)
}

impl Tab {
    pub(super) fn poll_subagent(&mut self) -> bool {
        let Some(viewer) = &mut self.state.subagent_dialog else {
            self.subagent = None;
            return false;
        };
        if viewer.selecting() {
            return viewer.tick();
        }
        let (Some(commands), Some(session)) = (&self.commands, &self.id) else {
            return viewer.tick();
        };
        let remote = self
            .subagent
            .get_or_insert_with(|| Remote {
                agent: viewer.id().into(),
                after: 0,
                next_poll: std::time::Instant::now(),
                pending: None,
                submission: None,
                failed: false,
            });
        if remote.agent != viewer.id() {
            self.subagent = None;
            return true;
        }
        let mut changed = viewer.tick();
        if let Some(pending) = &mut remote.pending {
            match pending.try_recv() {
                Ok(result) => {
                    remote.pending = None;
                    let result = result.and_then(|response| match response {
                        Response::Subagent(page) => Ok(page),
                        Response::Error(error) => anyhow::bail!("{error}"),
                        _ => anyhow::bail!("unexpected subagent response"),
                    });
                    if let Ok(page) = &result {
                        changed |= remote.after == 0;
                        remote.after = page.next;
                        if page.more {
                            remote.next_poll = std::time::Instant::now();
                        }
                        changed |= !page.updates.is_empty();
                    } else {
                        remote.failed = true;
                        changed = true;
                    }
                    viewer.apply_remote(result);
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    remote.pending = None;
                    remote.failed = true;
                    viewer.apply_remote(Err(anyhow::anyhow!("workspace connection closed")));
                    changed = true;
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if let Some(submission) = &mut remote.submission {
            match submission.try_recv() {
                Ok(result) => {
                    remote.submission = None;
                    viewer.submission_result(result.and_then(|response| match response {
                        Response::Ok => Ok(()),
                        Response::Error(error) => anyhow::bail!("{error}"),
                        _ => anyhow::bail!("unexpected subagent message response"),
                    }));
                    changed = true;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    remote.submission = None;
                    viewer.submission_result(Err(anyhow::anyhow!("workspace connection closed")));
                    changed = true;
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if !remote.failed
            && remote.pending.is_none()
            && std::time::Instant::now() >= remote.next_poll
            && let Ok(response) = request(
                commands,
                Request::ReadSubagent {
                    session: session.clone(),
                    agent: remote.agent.clone(),
                    after: remote.after,
                },
            )
        {
            remote.pending = Some(response);
            remote.next_poll = std::time::Instant::now() + POLL_INTERVAL;
        }
        changed
    }

    pub(super) fn send_subagent(&mut self, message: String) {
        let message = message.trim().to_owned();
        self.poll_subagent();
        let (Some(remote), Some(commands), Some(session), Some(viewer)) = (
            &mut self.subagent,
            &self.commands,
            &self.id,
            &mut self.state.subagent_dialog,
        ) else {
            return;
        };
        if remote.submission.is_some() {
            return;
        }
        match request(
            commands,
            Request::SendSubagent {
                session: session.clone(),
                agent: remote.agent.clone(),
                message,
            },
        ) {
            Ok(response) => remote.submission = Some(response),
            Err(error) => viewer.submission_result(Err(error)),
        }
    }
}
