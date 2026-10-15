//! Refresh the inspector from stored child runs, independently of parent prose.
use super::Event;
use crate::{
    protocol::{Request, Response},
    transport::{Client, RequestError},
};
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
    /// A restored transcript is a new observer, even for the same session. Always send its first
    /// snapshot rather than reusing a transport cache.
    pub(super) fn set(&mut self, scope: Option<(String, String)>) {
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
        if session != current
            || self
                .due
                .is_some_and(|due| Instant::now() < due)
        {
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
                .await;
            let response = match response {
                Ok(response) => response,
                Err(RequestError::Remote(_)) => return Ok(()),
                Err(error) => return Err(error.into()),
            };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(status: &str) -> Agent {
        Agent {
            id: "sample-child".into(),
            role: "explore".into(),
            model: "sample::model".into(),
            task: "Inspect generated module".into(),
            workspace: "sample-workspace".into(),
            status: status.into(),
            run: 3,
        }
    }

    #[tokio::test]
    async fn rejected_catalogue_pages_preserve_observed_status_and_allow_later_refresh() {
        let running = vec![agent("running")];
        let completed = vec![agent("completed")];
        let mut client = crate::transport::tests::fixture_client(vec![
            Response::Error("synthetic capability denial".into()),
            Response::Subagents(Snapshot::List {
                agents: running.clone(),
                next: None,
            }),
            Response::Subagents(Snapshot::List {
                agents: completed.clone(),
                next: Some(1),
            }),
            Response::Error("synthetic workspace changed during pagination".into()),
            Response::Subagents(Snapshot::List {
                agents: completed.clone(),
                next: None,
            }),
        ])
        .await;
        let mut watch = Watch::default();
        watch.set(Some(("sample-session".into(), "sample-workspace".into())));
        let (events, mut receiver) = mpsc::channel(8);

        watch
            .poll(&mut client, "sample-session", &events)
            .await
            .unwrap();
        assert!(!client.failed());
        assert!(watch.last.is_none());
        assert!(receiver.try_recv().is_err());

        watch.due = None;
        watch
            .poll(&mut client, "sample-session", &events)
            .await
            .unwrap();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Event::AgentStatuses { agents, .. } if agents == running
        ));

        watch.due = None;
        watch
            .poll(&mut client, "sample-session", &events)
            .await
            .unwrap();
        assert!(!client.failed());
        assert_eq!(watch.last.as_ref(), Some(&running));
        assert!(receiver.try_recv().is_err());

        watch.due = None;
        watch
            .poll(&mut client, "sample-session", &events)
            .await
            .unwrap();
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Event::AgentStatuses { agents, .. } if agents == completed
        ));

        watch.due = None;
        assert!(
            watch
                .poll(&mut client, "sample-session", &events)
                .await
                .is_err()
        );
        assert!(client.failed());
        assert_eq!(watch.last.as_ref(), Some(&completed));
    }
}
