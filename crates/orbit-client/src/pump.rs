mod catalogue;
mod queue_attachments;
mod session;
pub use session::{Command, Event, SessionOptions, observe};

use super::{
    Call,
    protocol::{Request, Response},
    transport::Client,
};
use anyhow::Result;
use std::time::Duration;
use tokio::sync::mpsc;

/// Track all non-transcript state so progress and attention changes are never suppressed.
#[derive(Default)]
struct PollState {
    status: Option<String>,
}

impl PollState {
    fn changed(&mut self, page: &crate::protocol::Page) -> Result<bool> {
        let status = serde_json::to_string(&(
            page.next,
            page.more,
            page.busy,
            &page.model,
            &page.approvals,
            &page.planning,
            &page.user_inputs,
            &page.tool_progress,
            &page.timing,
        ))?;
        let changed =
            page.more || !page.updates.is_empty() || self.status.as_ref() != Some(&status);
        self.status = Some(status);
        Ok(changed)
    }
}

/// Long or approval-bearing operations must not occupy the chat decision channel.
pub fn independent(request: &Request) -> bool {
    matches!(
        request,
        Request::ReadFile { .. }
            | Request::Review {
                action: orbit_protocol::review::Action::Undo { .. }
                    | orbit_protocol::review::Action::Stage { .. }
                    | orbit_protocol::review::Action::Unstage { .. },
                ..
            }
            | Request::Resources {
                action: orbit_protocol::resources::Action::ReloadMcp { .. }
                    | orbit_protocol::resources::Action::ScanMcp { .. }
                    | orbit_protocol::resources::Action::InstallMcp { .. },
                ..
            }
    )
}

pub async fn pump(
    mut client: Client,
    mut session: String,
    mut commands: mpsc::Receiver<Call>,
    responses: mpsc::Sender<Result<Response>>,
) {
    let mut cursor = 0;
    let mut state = PollState::default();
    let mut interval = tokio::time::interval(Duration::from_millis(75));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let Some(call) = next_call(&mut commands, &mut interval, &session, cursor).await else {
            break;
        };
        let response = client.request(call.request).await;
        let response = match response {
            Ok(response) => response,
            Err(error) if client.failed() => {
                if let Some(reply) = call.reply {
                    let _ = reply.send(Err(anyhow::anyhow!(error.to_string())));
                }
                let _ = responses.send(Err(error)).await;
                break;
            }
            Err(error) => Response::Error(format!("{error:#}")),
        };
        if let Some(reply) = call.reply {
            let _ = reply.send(Ok(response));
            continue;
        }
        match &response {
            Response::Page(page) => {
                cursor = page.next;
                if page.more {
                    interval.reset_immediately();
                }
                match state.changed(page) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        let _ = responses.send(Err(error)).await;
                        break;
                    }
                }
            }
            Response::Opened(info) => {
                session = info.id.clone();
                cursor = 0;
                state = PollState::default();
                interval.reset_immediately();
            }
            _ => {}
        }
        if responses
            .send(Ok(response))
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn next_call(
    commands: &mut mpsc::Receiver<Call>,
    interval: &mut tokio::time::Interval,
    session: &str,
    cursor: usize,
) -> Option<Call> {
    tokio::select! {
        biased;
        command = commands.recv() => command,
        _ = interval.tick() => Some(Call {
            request: Request::Poll {
                session: session.into(),
                after: cursor,
            },
            reply: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_delivers_progress_and_attention_changes_without_new_transcript_events() {
        let mut page = crate::protocol::Page {
            planning: Default::default(),
            tool_progress: Vec::new(),
            timing: None,
            next: 0,
            more: false,
            busy: true,
            model: "fixture::model".into(),
            updates: Vec::new(),
            approvals: Vec::new(),
            user_inputs: Vec::new(),
        };
        let mut state = PollState::default();
        assert!(state.changed(&page).unwrap());
        assert!(!state.changed(&page).unwrap());
        page.tool_progress
            .push(orbit_protocol::ToolProgress {
                call_id: "operation".into(),
                stdout: Some("partial".into()),
                elapsed_ms: None,
            });
        assert!(state.changed(&page).unwrap());
        assert!(!state.changed(&page).unwrap());
        page.timing = Some(orbit_protocol::RunTiming {
            elapsed_ms: 800,
            paused: false,
        });
        assert!(state.changed(&page).unwrap());
        page.approvals
            .push(crate::protocol::Approval {
                id: 1,
                preview: orbit_tools::approval::ApprovalPreview::Write(
                    orbit_tools::approval::WritePreview {
                        path: "note.txt".into(),
                        before: None,
                        after: "first".into(),
                    },
                ),
            });
        assert!(state.changed(&page).unwrap());
        if let orbit_tools::approval::ApprovalPreview::Write(preview) =
            &mut page.approvals[0].preview
        {
            preview.after = "changed".into();
        }
        assert!(state.changed(&page).unwrap());
        page.next += 1;
        assert!(state.changed(&page).unwrap());
        page.busy = false;
        assert!(state.changed(&page).unwrap());
        page.model = "fixture::other".into();
        assert!(state.changed(&page).unwrap());
        page.planning.generation += 1;
        assert!(state.changed(&page).unwrap());
        page.more = true;
        assert!(state.changed(&page).unwrap());
        assert!(state.changed(&page).unwrap());
    }

    #[tokio::test]
    async fn queued_user_commands_precede_due_polling_without_reordering_or_repeating() {
        let (tx, mut rx) = mpsc::channel(4);
        let requests = [
            Request::Steer {
                session: "active".into(),
                message: "new instruction".into(),
            },
            Request::Approve {
                session: "active".into(),
                approval: 7,
                allow: true,
                remember: false,
            },
            Request::Cancel {
                session: "active".into(),
            },
        ];
        let expected: Vec<_> = requests
            .iter()
            .map(|request| serde_json::to_value(request).unwrap())
            .collect();
        for request in requests {
            tx.send(Call {
                request,
                reply: None,
            })
            .await
            .unwrap();
        }
        let mut interval = tokio::time::interval(Duration::from_millis(75));
        for expected in expected {
            let actual = next_call(&mut rx, &mut interval, "active", 42)
                .await
                .unwrap()
                .request;
            assert_eq!(serde_json::to_value(actual).unwrap(), expected);
        }
        assert!(matches!(
            next_call(&mut rx, &mut interval, "active", 42)
                .await
                .unwrap()
                .request,
            Request::Poll { session, after: 42 } if session == "active"
        ));
        drop(tx);
        assert!(
            next_call(&mut rx, &mut interval, "active", 42)
                .await
                .is_none()
        );
    }
}
