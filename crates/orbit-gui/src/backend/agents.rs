//! Refresh the inspector from stored child runs, independently of parent prose.
use super::{Client, Event, Request, Response};
use anyhow::{Result, ensure};
use orbit_protocol::subagents::{Action, Agent, Snapshot};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Default)]
pub(super) struct Watch {
    scope: Option<(String, String)>,
    due: Option<Instant>,
    last: Option<Vec<Agent>>,
}

impl Watch {
    pub(super) fn set(&mut self, scope: Option<(String, String)>) {
        // A restored transcript is a new observer, even for the same session.
        // Always send its first snapshot rather than reusing a transport cache.
        self.scope = scope;
        self.due = None;
        self.last = None;
    }

    pub(super) async fn poll(
        &mut self,
        client: &mut Client,
        current: &str,
        events: &mpsc::Sender<Event>,
    ) -> Result<()> {
        let Some((session, workspace)) = &self.scope else {
            return Ok(());
        };
        if session != current || self.due.is_some_and(|due| Instant::now() < due) {
            return Ok(());
        }
        self.due = Some(Instant::now() + Duration::from_secs(2));
        let mut agents = Vec::new();
        let mut offset = 0;
        loop {
            let response = client
                .request(Request::Subagents {
                    session: session.clone(),
                    expected_workspace: workspace.clone(),
                    action: Action::List { offset },
                })
                .await?;
            let Response::Subagents(Snapshot::List { agents: page, next }) = response else {
                // A policy denial or workspace change must not manufacture a
                // terminal result, nor erase previously observed evidence.
                return Ok(());
            };
            agents.extend(page);
            let Some(next) = next else { break };
            ensure!(
                next > offset && next <= 20_000,
                "invalid child catalogue cursor"
            );
            offset = next;
        }
        if self.last.as_ref() != Some(&agents) {
            events
                .send(Event::AgentStatuses {
                    session: session.clone(),
                    workspace: workspace.clone(),
                    agents: agents.clone(),
                })
                .await?;
            self.last = Some(agents);
        }
        Ok(())
    }
}
