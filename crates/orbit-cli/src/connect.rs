use std::{io::Write, path::PathBuf};

use anyhow::{Context, Result, bail};
use orbit_agent::events::AgentEvent;
use orbit_app::workspace::{
    self,
    protocol::{Request, Response},
    transport,
};
use orbit_model::types::StreamEvent;

use crate::connect_args::ConnectArgs;

pub async fn connect(
    args: ConnectArgs,
    config: PathBuf,
    database: Option<PathBuf>,
    auth_dir: Option<PathBuf>,
) -> Result<()> {
    eprintln!("正在连接工作区…");
    let connection =
        workspace::prepare_connection(&args.options(), config, database, auth_dir, None).await?;
    let mut client = connection.connect().await?;
    if args.stop {
        client.request(Request::Shutdown).await?;
        println!("工作区服务已停止。");
        return Ok(());
    }
    if args.list {
        let Response::Sessions(sessions) = client.request(Request::Sessions).await? else {
            bail!("unexpected session list response");
        };
        for session in sessions {
            println!(
                "{}  {}  {}",
                session.id,
                session.model,
                session.title.replace(['\n', '\r'], " ")
            );
        }
        return Ok(());
    }
    let Response::Opened(info) = client
        .request(Request::Open {
            resume: args.resume,
            configuration: connection.configuration().await?,
        })
        .await?
    else {
        bail!("unexpected workspace response");
    };
    let id = info.id.clone();
    let result = if let Some(prompt) = args.prompt {
        run_prompt(&mut client, &id, prompt).await
    } else {
        orbit_tui::run_workspace(client, connection, info).await
    };
    result.with_context(|| {
        format!(
            "工作区会话 {id}；可使用相同连接参数加 --resume {id} 重新接入，断开连接不会取消任务"
        )
    })
}

async fn run_prompt(client: &mut transport::Client, session: &str, prompt: String) -> Result<()> {
    use workspace::prompt::PromptUpdate;

    eprintln!("会话：{session}");
    let cancellation = tokio_util::sync::CancellationToken::new();
    let token = cancellation.clone();
    let signals = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            token.cancel();
        }
    }));
    let result = workspace::prompt::run(client, session, prompt, cancellation, |update| {
        match update {
            PromptUpdate::Agent(AgentEvent::Model(StreamEvent::TextDelta(text))) => {
                print!("{text}");
                std::io::stdout().flush()?;
            }
            PromptUpdate::Agent(AgentEvent::Failed { error }) => eprintln!("{error}"),
            PromptUpdate::ApprovalDenied => {
                eprintln!("审批已拒绝：无交互模式；使用 connect --resume {session} 交互执行。")
            }
            _ => {}
        }
        Ok(())
    })
    .await;
    drop(signals);
    println!();
    result
}
