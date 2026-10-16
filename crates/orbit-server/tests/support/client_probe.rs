//! Isolated shared-client calls keep transport environment changes out of the test process.

use std::path::Path;

use orbit_client::{
    ConnectionOptions,
    protocol::{Request, Response},
};
use serde_json::Value;
use tokio::process::Command;

pub fn command(root: &Path, bin: &Path, request: Value) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "client_probe::connection",
            "--nocapture",
        ])
        .env("ORBIT_TEST_CONNECTION", request.to_string())
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config-home"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .current_dir(root)
        .kill_on_drop(true);
    command
}

#[tokio::test]
#[ignore = "invoked by transport tests in an isolated subprocess"]
async fn connection() {
    let request: Value =
        serde_json::from_str(&std::env::var("ORBIT_TEST_CONNECTION").unwrap()).unwrap();
    let mut options = ConnectionOptions::local(
        request["workspace"]
            .as_str()
            .unwrap()
            .into(),
    );
    options.server = Some(env!("CARGO_BIN_EXE_orbit-workspace-server").into());
    options
        .set_target(serde_json::from_value(request["target"].clone()).unwrap())
        .unwrap();
    let connection = orbit_client::prepare_connection(
        &options,
        request["config"]
            .as_str()
            .unwrap_or("missing.toml")
            .into(),
        request["database"]
            .as_str()
            .map(Into::into),
        request["auth"].as_str().map(Into::into),
        None,
    )
    .await
    .unwrap();
    let mut client = connection.connect().await.unwrap();
    match request["operation"].as_str().unwrap() {
        "list" => assert!(matches!(
            client
                .request(Request::Sessions)
                .await
                .unwrap(),
            Response::Sessions(_)
        )),
        "shutdown" => assert!(matches!(
            client
                .request(Request::Shutdown)
                .await
                .unwrap(),
            Response::Ok
        )),
        "prompt" => {
            let Response::Opened(info) = client
                .request(Request::Open {
                    resume: None,
                    model: Some(
                        request["model"]
                            .as_str()
                            .unwrap()
                            .into(),
                    ),
                    policy: Default::default(),
                    configuration: connection
                        .configuration()
                        .await
                        .unwrap(),
                })
                .await
                .unwrap()
            else {
                panic!("session did not open");
            };
            orbit_client::prompt::run(
                &mut client,
                &info.id,
                request["prompt"]
                    .as_str()
                    .unwrap()
                    .into(),
                tokio_util::sync::CancellationToken::new(),
                |_| Ok(()),
            )
            .await
            .unwrap();
        }
        _ => panic!("unexpected test operation"),
    }
}
