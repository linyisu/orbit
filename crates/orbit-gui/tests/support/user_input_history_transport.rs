use super::*;
use clap::Parser;
use orbit_protocol::user_input::{Answer, History, Status};
use std::collections::HashSet;

async fn request(backend: &Backend, request: Request) -> Result<Response, String> {
    let (result, response) = tokio::sync::oneshot::channel();
    backend
        .send(Command::Settings { request, result })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), response)
        .await
        .unwrap()
        .unwrap()
}

async fn history(backend: &Backend, session: &str, before: Option<i64>) -> Result<History, String> {
    match request(
        backend,
        Request::UserInputHistory {
            session: session.into(),
            before,
        },
    )
    .await?
    {
        Response::UserInputs(history) => Ok(history),
        response => panic!("expected input history, received {response:?}"),
    }
}

#[test]
fn agent_input_history_pages_retry_without_changing_pending_answers_or_other_sessions() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory
            .path()
            .join(".fixture-user-input-history"),
        "",
    )
    .unwrap();
    std::fs::write(
        directory
            .path()
            .join(".fixture-user-input-history-fail-once"),
        "",
    )
    .unwrap();
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.path().join("unused.toml");
    options.workspace = Some(directory.path().into());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
    options.resume = Some("demo".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options, &runtime);
    runtime.block_on(async {
        until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await;
        backend
            .send(Command::Send {
                session: "demo".into(),
                id: "history-live-question".into(),
                prompt: "questions".into(),
                paths: vec![],
                steer: false,
            })
            .unwrap();
        let Event::Response(Response::Page(page)) = until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if page
                        .user_inputs
                        .first()
                        .is_some_and(|record| {
                            record.status == Status::Pending && record.form.questions.len() == 2
                        })
            )
        })
        .await
        else {
            unreachable!()
        };
        let pending = page.user_inputs[0].clone();
        let first = history(&backend, "demo", None)
            .await
            .unwrap();
        assert_eq!(first.before, None);
        assert_eq!(first.records.len(), 20);
        assert_eq!(first.records[0], pending);
        let cursor = first.next.unwrap();
        assert!(
            history(&backend, "demo", Some(cursor))
                .await
                .unwrap_err()
                .contains("请重试")
        );
        let second = history(&backend, "demo", Some(cursor))
            .await
            .unwrap();
        assert_eq!(second.before, Some(cursor));
        assert_eq!(second.records.len(), 20);
        assert!(second.next.unwrap() < cursor);
        let last = history(&backend, "demo", second.next)
            .await
            .unwrap();
        assert_eq!(last.records.len(), 6);
        assert!(last.next.is_none());
        let ids: HashSet<_> = first
            .records
            .iter()
            .chain(&second.records)
            .chain(&last.records)
            .map(|record| &record.id)
            .collect();
        assert_eq!(ids.len(), 46);
        assert_eq!(last.records.last().unwrap().id, "history-request-1");
        let other = history(&backend, "navigation", None)
            .await
            .unwrap();
        assert!(other.records.is_empty() && other.next.is_none());
        let Response::Page(page) = request(
            &backend,
            Request::Poll {
                session: "demo".into(),
                after: 0,
            },
        )
        .await
        .unwrap() else {
            panic!("page expected")
        };
        assert_eq!(page.user_inputs, vec![pending.clone()]);

        let answers = vec![
            Answer {
                question_id: "choice".into(),
                selected: vec!["full".into()],
                ..Default::default()
            },
            Answer {
                question_id: "details".into(),
                selected: vec!["docs".into(), "tests".into()],
                text: "保留原有结构".into(),
                note: "历史读取后提交".into(),
            },
        ];
        request(
            &backend,
            Request::AnswerUserInput {
                session: "demo".into(),
                request: pending.id.clone(),
                answers: Some(answers.clone()),
            },
        )
        .await
        .unwrap();
        let refreshed = history(&backend, "demo", None)
            .await
            .unwrap();
        assert_eq!(refreshed.next, first.next);
        assert_eq!(refreshed.records[0].id, pending.id);
        assert_eq!(refreshed.records[0].status, Status::Answered);
        assert_eq!(refreshed.records[0].answers, answers);
        assert!(
            history(&backend, "navigation", None)
                .await
                .unwrap()
                .records
                .is_empty()
        );
    });
}
