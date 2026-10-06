use super::{
    Call,
    protocol::{Request, Response},
    transport::Client,
};
use anyhow::Result;
use std::time::Duration;
use tokio::sync::mpsc;

pub async fn pump(
    mut client: Client,
    mut session: String,
    mut commands: mpsc::Receiver<Call>,
    responses: mpsc::Sender<Result<Response>>,
) {
    let mut cursor = 0;
    let mut last_status = None;
    let mut interval = tokio::time::interval(Duration::from_millis(75));
    loop {
        let call = tokio::select! {
            command = commands.recv() => match command { Some(command) => command, None => break },
            _ = interval.tick() => Call { request: Request::Poll { session: session.clone(), after: cursor }, reply: None },
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
                let status = (
                    page.busy,
                    page.model.clone(),
                    page.approvals
                        .iter()
                        .map(|approval| approval.id)
                        .collect::<Vec<_>>(),
                );
                if page.updates.is_empty() && last_status.as_ref() == Some(&status) {
                    continue;
                }
                last_status = Some(status);
            }
            Response::Opened(info) => {
                session = info.id.clone();
                cursor = 0;
                last_status = None;
                interval.reset_immediately();
            }
            _ => {}
        }
        if responses.send(Ok(response)).await.is_err() {
            break;
        }
    }
}
