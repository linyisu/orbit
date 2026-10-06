use std::time::Duration;

use anyhow::{Result, bail};
use orbit_agent::events::AgentEvent;
use tokio_util::sync::CancellationToken;

use super::{
    protocol::{Mode, Page, Request, Response, Update},
    transport::Client,
};

const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub enum PromptUpdate<'a> {
    Agent(&'a AgentEvent),
    ApprovalDenied,
}

pub struct PromptOptions {
    pub prompt: String,
    pub images: Vec<orbit_protocol::ImageUpload>,
    pub mode: Mode,
    pub model: Option<String>,
}

pub struct PromptResult {
    pub answer: String,
    pub cancelled: bool,
}

pub async fn run(
    client: &mut Client,
    session: &str,
    prompt: String,
    cancellation: CancellationToken,
    report: impl FnMut(PromptUpdate<'_>) -> Result<()>,
) -> Result<()> {
    execute(
        client,
        session,
        PromptOptions {
            prompt,
            images: Vec::new(),
            mode: Mode::Auto,
            model: None,
        },
        cancellation,
        report,
    )
    .await
    .map(|_| ())
}

pub async fn execute(
    client: &mut Client,
    session: &str,
    options: PromptOptions,
    cancellation: CancellationToken,
    mut report: impl FnMut(PromptUpdate<'_>) -> Result<()>,
) -> Result<PromptResult> {
    let mut cursor = 0;
    loop {
        let page = poll(client, session, cursor).await?;
        cursor = page.next;
        if !page.more {
            break;
        }
    }
    client
        .request(Request::Start {
            images: options.images,
            compact: false,
            session: session.into(),
            request_id: super::request_id(),
            prompt: options.prompt,
            mode: options.mode,
            model: options.model,
        })
        .await?;

    let mut failed = false;
    let mut answer = String::new();
    let mut cancelled = false;
    let mut cancel_sent = false;
    loop {
        let page = poll(client, session, cursor).await?;
        cursor = page.next;
        for update in page.updates {
            match update {
                Update::Agent(event) => {
                    failed |= matches!(*event, AgentEvent::Failed { .. });
                    match &*event {
                        AgentEvent::TurnStarted { .. } => answer.clear(),
                        AgentEvent::Model(orbit_model::types::StreamEvent::TextDelta(text)) => {
                            answer.push_str(text)
                        }
                        _ => {}
                    }
                    if let Err(error) = report(PromptUpdate::Agent(&event)) {
                        let _ = client
                            .request(Request::Cancel {
                                session: session.into(),
                            })
                            .await;
                        return Err(error);
                    }
                }
                Update::Finished { status } => {
                    cancelled = status == "cancelled";
                    failed |= status != "completed" && !cancelled;
                }
                _ => {}
            }
        }
        for approval in page.approvals {
            report(PromptUpdate::ApprovalDenied)?;
            client
                .request(Request::Approve {
                    session: session.into(),
                    approval: approval.id,
                    allow: false,
                    remember: false,
                })
                .await?;
        }
        if !page.busy && !page.more {
            break;
        }
        if !page.more {
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {},
                _ = cancellation.cancelled(), if !cancel_sent => {
                    client.request(Request::Cancel { session: session.into() }).await?;
                    cancel_sent = true;
                }
            }
        }
    }

    if failed {
        bail!("workspace run did not complete successfully");
    }
    Ok(PromptResult { answer, cancelled })
}

async fn poll(client: &mut Client, session: &str, after: usize) -> Result<Page> {
    match client
        .request(Request::Poll {
            session: session.into(),
            after,
        })
        .await?
    {
        Response::Page(page) => Ok(page),
        _ => bail!("unexpected event response"),
    }
}
