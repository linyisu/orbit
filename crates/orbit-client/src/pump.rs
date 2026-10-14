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
                let status = (
                    page.busy,
                    page.model.clone(),
                    page.planning.generation,
                    page.user_inputs
                        .iter()
                        .map(|q| q.id.clone())
                        .collect::<Vec<_>>(),
                    page.approvals
                        .iter()
                        .map(|approval| approval.id)
                        .collect::<Vec<_>>(),
                );
                if page.updates.is_empty()
                    && page.timing.is_none()
                    && last_status.as_ref() == Some(&status)
                {
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
