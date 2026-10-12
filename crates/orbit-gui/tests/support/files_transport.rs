use super::*;
use base64::Engine as _;
use clap::Parser;
use orbit_protocol::files::{Action, EntryKind, Snapshot};

async fn request(backend: &Backend, request: Request) -> Result<Response, String> {
    let (result, receiver) = tokio::sync::oneshot::channel();
    backend.send(Command::Settings { request, result }).unwrap();
    tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .unwrap()
        .unwrap()
}

#[test]
fn file_transport_preserves_raw_text_and_refuses_stale_saves() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("src")).unwrap();
    let path = directory.path().join("src/notes.md");
    let original = "```rust\r\nlet old = 1;\r\n```\r\n";
    let edited = "```rust\r\nlet 新 = 2;\r\n```\r\n";
    std::fs::write(&path, original).unwrap();
    std::fs::write(directory.path().join(".hidden"), "hidden").unwrap();
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
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| matches!(event, Event::Response(Response::Opened(_)))).await else { unreachable!() };
        let session = info.id;
        let workspace = directory.path().to_string_lossy().into_owned();
        let files = |action| Request::Files { session: session.clone(), expected_workspace: Some(workspace.clone()), action };
        let Response::Files(Snapshot::Directory { entries, .. }) = request(&backend, files(Action::List { path: ".".into(), offset: 0, include_hidden: false })).await.unwrap() else { panic!("directory expected") };
        assert!(entries.iter().any(|entry| entry.path == "src" && entry.kind == EntryKind::Directory));
        assert!(!entries.iter().any(|entry| entry.path == ".hidden"));
        let Response::Files(Snapshot::Directory { entries, .. }) = request(&backend, files(Action::List { path: "src".into(), offset: 0, include_hidden: false })).await.unwrap() else { panic!("directory expected") };
        assert!(entries.iter().any(|entry| entry.path == "src/notes.md" && entry.kind == EntryKind::File));
        let Response::File(encoded) = request(&backend, Request::ReadFile { session: session.clone(), path: "src/notes.md".into() }).await.unwrap() else { panic!("raw file expected") };
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(encoded).unwrap(), original.as_bytes());
        backend.send(Command::Request(Request::ReadFile { session: session.clone(), path: "src/notes.md".into() })).unwrap();
        let Event::File { path: returned, bytes } = until(&mut events, |event| matches!(event, Event::File { .. })).await else { unreachable!() };
        assert_eq!(returned, "src/notes.md");
        assert_eq!(bytes, original.as_bytes());
        assert!(matches!(request(&backend, files(Action::Save { path: "src/notes.md".into(), original: original.into(), content: edited.into() })).await.unwrap(), Response::Files(Snapshot::Saved { content, .. }) if content == edited));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
        std::fs::write(&path, "external change").unwrap();
        let error = request(&backend, files(Action::Save { path: "src/notes.md".into(), original: edited.into(), content: "draft must stay in editor".into() })).await.unwrap_err();
        assert!(error.contains("其他程序修改"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external change");
        assert!(request(&backend, Request::Files { session: session.clone(), expected_workspace: Some("previous-workspace".into()), action: Action::Save { path: "src/notes.md".into(), original: "external change".into(), content: "wrong workspace".into() } }).await.unwrap_err().contains("工作区已切换"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external change");
    });
}
