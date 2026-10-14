use super::*;
use clap::Parser;

async fn request(backend: &Backend, request: Request) -> Result<Response, String> {
    let (result, receiver) = tokio::sync::oneshot::channel();
    backend
        .send(Command::Settings { request, result })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .unwrap()
        .unwrap()
}

#[test]
fn side_question_rows_follow_their_thread_and_confirm_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.path().join("unused.toml");
    options.workspace = Some(directory.path().into());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options, &runtime);
    runtime.block_on(async {
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await
        else {
            unreachable!()
        };
        let session = info.id;
        let ask = |question: &str, follow_up| Request::Ask {
            session: session.clone(),
            question: question.into(),
            follow_up,
        };
        // The normal command establishes the viewer and the existing streaming
        // poll. Subsequent row actions use correlated Settings responses.
        backend
            .send(Command::Request(ask("第一条独立问题", None)))
            .unwrap();
        until(&mut events, |event| {
            matches!(event,
            Event::Response(Response::Questions(questions))
                if !questions.running && questions.records.as_ref().is_some_and(|records|
                    records.len() == 1 && records[0].answer == "旁路回答已完成"))
        })
        .await;
        let Response::Questions(second) = request(&backend, ask("第二条独立问题", None))
            .await
            .unwrap()
        else {
            panic!("question snapshot expected")
        };
        assert_eq!(second.selected, Some(1));
        let records = second.records.unwrap();
        assert_ne!(records[0].thread_id, records[1].thread_id);
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Questions(questions))
                    if !questions.running
                        && questions
                            .records
                            .as_ref()
                            .is_some_and(|records| records.len() == 2)
            )
        })
        .await;
        assert!(
            request(&backend, ask("无效索引不改变历史", Some(99)))
                .await
                .unwrap_err()
                .contains("不存在")
        );
        let Response::Questions(follow_up) =
            request(&backend, ask("hold-side: 继续第一条", Some(0)))
                .await
                .unwrap()
        else {
            panic!("question snapshot expected")
        };
        assert_eq!(follow_up.selected, Some(2));
        let records = follow_up.records.unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].thread_id, records[2].thread_id);
        assert_ne!(records[1].thread_id, records[2].thread_id);
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Questions(questions))
                    if questions.running
                        && questions.selected.is_none()
                        && questions
                            .records
                            .as_ref()
                            .is_some_and(|records| records[2].answer == "正在生成，等待停止")
            )
        })
        .await;
        assert!(
            request(&backend, ask("忙时不会追加", None))
                .await
                .unwrap_err()
                .contains("仍在生成")
        );
        assert!(
            request(
                &backend,
                Request::Admin {
                    action: orbit_protocol::admin::Action::DeleteSession {
                        id: session.clone()
                    },
                }
            )
            .await
            .unwrap_err()
            .contains("正在运行")
        );
        assert!(matches!(
            request(
                &backend,
                Request::CancelQuestion {
                    session: session.clone(),
                    index: 2,
                },
            )
            .await
            .unwrap(),
            Response::Ok
        ));
        let Event::Response(Response::Questions(stopped)) = until(&mut events, |event| {
            matches!(event,
            Event::Response(Response::Questions(questions))
                if !questions.running && questions.records.as_ref().is_some_and(|records|
                    records.len() == 3 && records[2].status == "cancelled"))
        })
        .await
        else {
            unreachable!()
        };
        let records = stopped.records.unwrap();
        assert_eq!(records[0].status, "completed");
        assert_eq!(records[1].status, "completed");
        assert_eq!(records[2].answer, "正在生成，等待停止");
        let Response::Questions(unchanged) = request(
            &backend,
            Request::ReadQuestions {
                session: session.clone(),
                after: Some(stopped.revision),
            },
        )
        .await
        .unwrap() else {
            panic!("question snapshot expected")
        };
        assert!(unchanged.records.is_none());
        backend
            .send(Command::Request(Request::CloseQuestions {
                session: session.clone(),
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(event, Event::Response(Response::Ok))
        })
        .await;
        assert!(
            request(
                &backend,
                Request::OpenQuestions {
                    session: session.clone()
                }
            )
            .await
            .unwrap_err()
            .contains("没有正在查看")
        );
        assert!(
            request(&backend, ask("旧索引不可跨关闭复用", Some(0)))
                .await
                .unwrap_err()
                .contains("已关闭")
        );
    });
}
