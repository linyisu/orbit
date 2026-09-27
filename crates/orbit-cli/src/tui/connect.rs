//! Browser account connection, isolated from conversation history.
use std::{
    io::Write,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use futures_util::StreamExt;
use orbit_auth::{
    AuthStore, CallbackFlow, CallbackListener, LoginOutcome, OAuthLogin, OAuthProvider,
};
use orbit_model::client::ApiFormat;
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tokio_util::task::AbortOnDropHandle;

use super::{ModelOption, terminal};

#[derive(Clone, Copy)]
enum ConnectionProvider {
    OAuth(OAuthProvider),
    ApiKey(orbit_auth::ApiKeyProvider),
}

impl ConnectionProvider {
    fn name(self) -> &'static str {
        match self {
            Self::OAuth(provider) => provider.name(),
            Self::ApiKey(provider) => provider.name(),
        }
    }
}

const PROVIDERS: [(ConnectionProvider, &str, ApiFormat); 5] = [
    (
        ConnectionProvider::OAuth(OAuthProvider::Antigravity),
        "Google Antigravity",
        ApiFormat::GoogleCodeAssist,
    ),
    (
        ConnectionProvider::OAuth(OAuthProvider::Google),
        "Google Gemini CLI",
        ApiFormat::GoogleCodeAssist,
    ),
    (
        ConnectionProvider::OAuth(OAuthProvider::Chatgpt),
        "ChatGPT",
        ApiFormat::ChatgptCodex,
    ),
    (
        ConnectionProvider::ApiKey(orbit_auth::ApiKeyProvider::Deepseek),
        "DeepSeek API",
        ApiFormat::OpenaiChatCompletions,
    ),
    (
        ConnectionProvider::ApiKey(orbit_auth::ApiKeyProvider::ZhipuCodingPlan),
        "智谱 Coding Plan",
        ApiFormat::OpenaiChatCompletions,
    ),
];

pub(super) async fn run(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    options: &crate::TuiOptions,
) -> Result<Option<Vec<ModelOption>>> {
    let mut connected = AuthStore::new(options.auth_directory.clone())?.list()?;
    let mut clipboard = None;
    let mut copied_at = None;
    let mut pointer = None;
    let mut selected = 0;
    let mut pending_project = None;
    let mut project = String::new();
    let mut message = String::new();
    let mut task = None;
    let mut url = String::new();
    let mut verification_url: Option<String> = None;
    let mut timer = tokio::time::interval(Duration::from_millis(100));

    loop {
        let mut rows = Vec::new();
        let mut verification_button = Rect::default();
        let mut copy_button = Rect::default();
        terminal::draw_synchronized(terminal, |frame| {
            let bounds = frame.area();
            let width = bounds.width.saturating_sub(4).min(76);
            let height =
                bounds
                    .height
                    .saturating_sub(2)
                    .min(if message.is_empty() { 9 } else { 16 });
            let area = Rect::new(
                bounds.x + bounds.width.saturating_sub(width) / 2,
                bounds.y + bounds.height.saturating_sub(height) / 2,
                width,
                height,
            );
            super::overlay::clear(frame, area);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" 连接账号 ")
                .border_style(Style::default().fg(Color::Rgb(220, 140, 100)));
            let inner = block.inner(area);
            frame.render_widget(block, area);

            let mut lines = Vec::new();
            for (index, (provider, name, _)) in PROVIDERS.iter().enumerate() {
                rows.push(Rect::new(inner.x, inner.y + index as u16, inner.width, 1));
                let style = if index == selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(220, 140, 100))
                } else {
                    Style::default()
                };
                let status = if connected.iter().any(|entry| entry.name == provider.name()) {
                    "  已连接"
                } else {
                    ""
                };
                let label = format!(" {name}{status} ");
                if let Some(row) = rows.last_mut() {
                    row.width = row
                        .width
                        .min(unicode_width::UnicodeWidthStr::width(label.as_str()) as u16);
                }
                lines.push(Line::styled(label, style));
            }
            lines.push(Line::raw(""));
            if pending_project.is_some() {
                lines.push(Line::raw(
                    "Google 账号授权已完成，未能自动确定 Cloud Code Assist 项目。",
                ));
                lines.push(Line::raw(format!("Google Cloud 项目 ID：{project}")));
            }
            lines.push(Line::raw(&message));
            let mut content = inner;
            if !url.is_empty() || verification_url.is_some() {
                content.height = content.height.saturating_sub(2);
                let y = inner.bottom().saturating_sub(1);
                let copy_label = copy_label(copied_at, Instant::now());
                let open_label = if verification_url.is_some() {
                    "[打开验证页面]"
                } else {
                    "[打开浏览器]"
                };
                copy_button = button_area(inner, inner.x.saturating_add(1), y, copy_label);
                verification_button =
                    button_area(inner, copy_button.right().saturating_add(2), y, open_label);
                for (area, label) in [(copy_button, copy_label), (verification_button, open_label)]
                {
                    let hovered = pointer.is_some_and(|point| area.contains(point));
                    let style = if hovered {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Rgb(220, 140, 100))
                    } else {
                        Style::default().fg(Color::Rgb(220, 140, 100))
                    };
                    frame.render_widget(Paragraph::new(label).style(style), area);
                }
            }
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), content);
        })?;

        if let Some(handle) = task.as_ref()
            && AbortOnDropHandle::is_finished(handle)
        {
            let result: Result<(LoginOutcome, Option<String>)> =
                task.take().expect("login task").await?;
            match result {
                Ok((LoginOutcome::Connected, _)) => match save_profile(options, selected).await {
                    Ok(models) => return Ok(Some(models)),
                    Err(error) => message = format!("账号已连接，但配置未保存：{error:#}"),
                },
                Ok((LoginOutcome::GoogleProjectRequired(pending), error)) => {
                    pending_project = Some(pending);
                    message = error
                        .unwrap_or_else(|| "填写项目后继续初始化，无需再次登录 Google。".into());
                }
                Err(error) => {
                    if let Some(orbit_auth::AuthError::AntigravityVerification { reason, url }) =
                        error.downcast_ref::<orbit_auth::AuthError>()
                    {
                        verification_url = Some(url.clone());
                        message = format!("Google 要求完成账号验证：{reason}");
                    } else {
                        message = format!("连接失败：{error:#}");
                    }
                }
            }
            url.clear();
            connected = AuthStore::new(options.auth_directory.clone())?.list()?;
        }

        let event = tokio::select! {
            _ = timer.tick() => continue,
            event = events.next() => match event {
                Some(event) => event?,
                None => { abort(&mut task); return Ok(None); }
            },
        };
        if let Event::Mouse(mouse) = &event {
            pointer = Some((mouse.column, mouse.row).into());
        }
        let link = verification_url
            .as_deref()
            .or_else(|| (!url.is_empty()).then_some(url.as_str()));
        if let Some(link) = link {
            if copy_requested(&event, copy_button) {
                match copy_link(link, &mut clipboard) {
                    Ok(()) => copied_at = Some(Instant::now()),
                    Err(_) => message = "复制失败，请重试或打开浏览器。".into(),
                }
                continue;
            }
            if open_verification_requested(&event, verification_button) {
                crate::auth_cli::launch_browser(link).await;
                continue;
            }
        }

        let mut activate = false;
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    abort(&mut task);
                    return Ok(None);
                }
                KeyCode::Esc => {
                    if task.is_some() {
                        abort(&mut task);
                        url.clear();
                        message = "已取消连接。".into();
                    } else if pending_project.take().is_some() {
                        project.clear();
                        message = "已取消项目初始化。".into();
                    } else {
                        return Ok(None);
                    }
                }
                KeyCode::Enter if task.is_none() && pending_project.is_some() => activate = true,
                KeyCode::Backspace if task.is_none() && pending_project.is_some() => {
                    project.pop();
                }
                KeyCode::Char(character)
                    if task.is_none()
                        && pending_project.is_some()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && project.len() < 256
                        && !character.is_control() =>
                {
                    project.push(character);
                }
                _ if task.is_none() && pending_project.is_none() => {
                    activate = select_provider(Event::Key(key), &mut selected, &rows)
                }
                _ => {}
            },
            Event::Paste(text) if task.is_none() && pending_project.is_some() => {
                project.extend(
                    text.chars()
                        .filter(|c| !c.is_control())
                        .take(256usize.saturating_sub(project.chars().count())),
                );
            }
            event if task.is_none() && pending_project.is_none() => {
                activate = select_provider(event, &mut selected, &rows)
            }
            _ => {}
        }

        if !activate {
            continue;
        }

        if let Some(mut pending) = pending_project.take() {
            if project.trim().is_empty() {
                pending_project = Some(pending);
                message = "请输入 Google Cloud 项目 ID。".into();
                continue;
            }

            let store = AuthStore::new(options.auth_directory.clone())?;
            let project_id = project.trim().to_owned();
            message = "正在初始化 Google Cloud Code Assist 项目…".into();
            task = Some(AbortOnDropHandle::new(tokio::spawn(async move {
                match pending.finish(&project_id, &store, "google").await {
                    Ok(()) => Ok((LoginOutcome::Connected, None)),
                    Err(error) => Ok((
                        LoginOutcome::GoogleProjectRequired(pending),
                        Some(format!("项目初始化失败：{error}")),
                    )),
                }
            })));
            continue;
        }

        verification_url = None;
        copied_at = None;
        let provider = match PROVIDERS[selected].0 {
            ConnectionProvider::OAuth(provider) => provider,
            ConnectionProvider::ApiKey(provider) => {
                if let Some(models) =
                    super::connect_key::run(terminal, events, options, provider).await?
                {
                    return Ok(Some(models));
                }
                continue;
            }
        };
        let login = OAuthLogin::new(provider, None)?;
        let listener = match CallbackListener::bind_at(login.redirect_uri().clone()).await {
            Ok(listener) => listener,
            Err(error) => {
                message = format!("无法监听登录回调：{error}");
                continue;
            }
        };
        let store = AuthStore::new(options.auth_directory.clone())?;
        url = login.authorization_url().to_string();
        message = "等待浏览器授权…".into();
        let browser_url = url.clone();
        task = Some(AbortOnDropHandle::new(tokio::spawn(async move {
            crate::auth_cli::launch_browser(&browser_url).await;
            let callback = listener.wait(&login, Duration::from_secs(300)).await?;
            let outcome = login
                .finish_interactive(&callback, &store, provider.name())
                .await?;
            Ok((outcome, None))
        })));
    }
}

fn copy_label(copied_at: Option<Instant>, now: Instant) -> &'static str {
    if copied_at.is_some_and(|at| now.saturating_duration_since(at) < Duration::from_millis(1500)) {
        "[已复制]"
    } else {
        "[复制链接]"
    }
}

fn button_area(bounds: Rect, x: u16, y: u16, label: &str) -> Rect {
    let width = unicode_width::UnicodeWidthStr::width(label).min(u16::MAX as usize) as u16;
    Rect::new(x, y, width, 1).intersection(bounds)
}

fn copy_requested(event: &Event, button: Rect) -> bool {
    match event {
        Event::Mouse(mouse) => {
            mouse.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left)
                && button.contains((mouse.column, mouse.row).into())
        }
        Event::Key(key) => {
            key.kind != KeyEventKind::Release
                && key.code == KeyCode::Char('y')
                && key.modifiers.is_empty()
        }
        _ => false,
    }
}

fn copy_link(link: &str, clipboard: &mut Option<arboard::Clipboard>) -> Result<()> {
    if clipboard.is_none() {
        *clipboard = arboard::Clipboard::new().ok();
    }
    if let Some(clipboard) = clipboard
        && clipboard.set_text(link.to_owned()).is_ok()
    {
        return Ok(());
    }

    // OSC 52 also supports terminals without a local desktop clipboard (SSH).
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(link);
    let mut stdout = std::io::stdout().lock();
    write!(stdout, "\x1b]52;c;{encoded}\x07")?;
    stdout.flush()?;
    Ok(())
}

fn open_verification_requested(event: &Event, button: Rect) -> bool {
    match event {
        Event::Key(key) => {
            key.kind != KeyEventKind::Release
                && matches!(key.code, KeyCode::Char('v' | 'V'))
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        }
        Event::Mouse(mouse) => {
            mouse.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left)
                && button.contains((mouse.column, mouse.row).into())
        }
        _ => false,
    }
}

/// Confirmation starts OAuth immediately; selection has no text-entry stage.
fn select_provider(event: Event, selected: &mut usize, rows: &[Rect]) -> bool {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
            KeyCode::Enter => return true,
            KeyCode::Down => *selected = (*selected + 1) % PROVIDERS.len(),
            KeyCode::Up => *selected = (*selected + PROVIDERS.len() - 1) % PROVIDERS.len(),
            _ => {}
        },
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::Moved | MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                if let Some(index) = rows
                    .iter()
                    .position(|row| row.contains((mouse.column, mouse.row).into()))
                {
                    *selected = index;
                    return mouse.kind != MouseEventKind::Moved;
                }
            }
            _ => {}
        },
        _ => {}
    }

    false
}

type LoginTask = AbortOnDropHandle<Result<(LoginOutcome, Option<String>)>>;

fn abort(task: &mut Option<LoginTask>) {
    if let Some(task) = task.take() {
        task.abort();
    }
}

async fn save_profile(options: &crate::TuiOptions, selected: usize) -> Result<Vec<ModelOption>> {
    let (provider, _, format) = PROVIDERS[selected];
    let ConnectionProvider::OAuth(provider) = provider else {
        anyhow::bail!("API key connection must be verified before saving");
    };
    let endpoint = match provider {
        OAuthProvider::Antigravity => "https://daily-cloudcode-pa.googleapis.com/",
        OAuthProvider::Google => "https://cloudcode-pa.googleapis.com/",
        OAuthProvider::Chatgpt => "https://chatgpt.com/backend-api/codex/",
        OAuthProvider::Anthropic => unreachable!(),
    };
    append_profile(
        &options.config,
        &options.auth_directory,
        provider.name(),
        format,
        endpoint,
    )?;
    let mut config = crate::FileConfig::load(&options.config, &options.auth_directory)?;
    let diagnostics = crate::account_models::refresh(&mut config, true).await;
    if !diagnostics.is_empty() {
        anyhow::bail!("{}", diagnostics.join("\n"));
    }
    config.model_options()
}

fn append_profile(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    credential: &str,
    format: ApiFormat,
    endpoint: &str,
) -> Result<Vec<ModelOption>> {
    let catalog: serde_json::Value = serde_json::from_str(include_str!("account_models.json"))?;
    let antigravity;
    let models = if credential == "antigravity" {
        antigravity = serde_json::to_value(crate::antigravity_profiles()?)?;
        &antigravity
    } else {
        catalog.get(credential).context("账号缺少内置模型目录")?
    };

    append_profile_models(path, auth_directory, credential, format, endpoint, models)
}

pub(super) fn append_profile_models(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    credential: &str,
    format: ApiFormat,
    endpoint: &str,
    models: &serde_json::Value,
) -> Result<Vec<ModelOption>> {
    let original = std::fs::read_to_string(path)?;
    let mut config: crate::FileConfig = toml::from_str(&original)?;
    config.auth_directory = Some(auth_directory.to_path_buf());
    config.merge_account_models()?;

    if config.providers.values().any(|provider| {
        provider.credential.as_deref() == Some(credential)
            && provider.base_url == endpoint
            && provider.api_format == format
    }) {
        return config.model_options();
    }

    let mut name = format!("{credential}-account");
    let mut suffix = 2;
    while config.providers.contains_key(&name) {
        name = format!("{credential}-account-{suffix}");
        suffix += 1;
    }

    // Serialize only the added table, preserving the user's comments and settings.
    let addition = serde_json::json!({
        "providers": {
            name: {
                "api_format": format,
                "base_url": endpoint,
                "credential": credential,
                "models": models
            }
        }
    });
    let updated = format!(
        "{}\n\n{}",
        original.trim_end(),
        toml::to_string_pretty(&addition)?
    );
    let mut parsed: crate::FileConfig = toml::from_str(&updated)?;
    parsed.auth_directory = Some(auth_directory.to_path_buf());
    parsed.merge_account_models()?;
    let models = parsed.model_options()?;

    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(std::fs::metadata(path)?.permissions())?;
    temporary.write_all(updated.as_bytes())?;
    temporary.as_file().sync_all()?;
    ensure!(
        std::fs::read_to_string(path)? == original,
        "配置已被其他程序修改，请重试"
    );
    temporary.persist(path).context("无法保存账号配置")?;

    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_feedback_expires_and_buttons_fit_display_width() {
        let now = Instant::now();
        assert_eq!(copy_label(Some(now), now), "[已复制]");
        assert_eq!(
            copy_label(Some(now), now + Duration::from_millis(1500)),
            "[复制链接]"
        );
        assert_eq!(copy_label(None, now), "[复制链接]");

        let bounds = Rect::new(0, 0, 60, 10);
        assert_eq!(button_area(bounds, 1, 8, "[已复制]").width, 8);
        assert_eq!(button_area(bounds, 1, 8, "[复制链接]").width, 10);
        assert_eq!(button_area(bounds, 1, 8, "[打开浏览器]").width, 12);
    }

    #[test]
    fn copy_button_only_activates_on_click_or_copy_shortcut() {
        use crossterm::event::{KeyEvent, MouseButton, MouseEvent};

        let button = Rect::new(4, 8, 14, 1);
        let mouse = |kind, row| {
            Event::Mouse(MouseEvent {
                kind,
                column: 5,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };

        assert!(copy_requested(
            &mouse(MouseEventKind::Down(MouseButton::Left), 8),
            button
        ));
        assert!(!copy_requested(&mouse(MouseEventKind::Moved, 8), button));
        assert!(!copy_requested(
            &mouse(MouseEventKind::Down(MouseButton::Left), 9),
            button
        ));
        assert!(copy_requested(
            &Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            button
        ));
        assert!(!copy_requested(
            &Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            button
        ));
    }

    #[test]
    fn verification_opens_only_on_explicit_keyboard_or_button_selection() {
        use crossterm::event::{KeyEvent, MouseButton, MouseEvent};

        let button = Rect::new(2, 10, 40, 1);
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        let click = |row| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };

        assert!(open_verification_requested(
            &key(KeyCode::Char('v')),
            button
        ));
        assert!(open_verification_requested(&click(10), button));
        assert!(!open_verification_requested(&click(9), button));
        assert!(!open_verification_requested(&key(KeyCode::Enter), button));
        assert!(!open_verification_requested(
            &Event::Resize(100, 40),
            button
        ));
    }

    #[test]
    fn selecting_an_account_immediately_requests_browser_login() {
        use crossterm::event::{KeyEvent, MouseButton, MouseEvent};

        let mut selected = 0;
        let rows = [Rect::new(2, 3, 20, 1), Rect::new(2, 4, 20, 1)];
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(select_provider(enter, &mut selected, &rows));
        assert_eq!(selected, 0);

        let click = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });

        assert!(select_provider(click, &mut selected, &rows));
        assert_eq!(selected, 1);
        assert!(!select_provider(
            Event::Paste("model-id".into()),
            &mut selected,
            &rows
        ));
    }

    #[test]
    fn api_key_connection_persists_discovered_models_without_a_secret() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let auth = directory.path().join("auth");
        std::fs::write(
            &path,
            "# preserve me\ndefault_provider = 'deepseek-account'\n[providers]\n",
        )
        .unwrap();
        let catalog = serde_json::json!([{ "id": "server-model-a" }, { "id": "server-model-b" }]);

        let models = append_profile_models(
            &path,
            &auth,
            "deepseek",
            ApiFormat::OpenaiChatCompletions,
            orbit_auth::DEEPSEEK_BASE,
            &catalog,
        )
        .unwrap();

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].model, "server-model-a");
        assert_eq!(models[1].model, "server-model-b");
        assert_eq!(
            models[0].config.credential.as_ref().unwrap().name,
            "deepseek"
        );
        let saved = std::fs::read_to_string(path).unwrap();
        assert!(saved.starts_with("# preserve me"));
        assert!(!saved.contains("api_key"));
    }

    #[test]
    fn zhipu_profile_uses_domestic_coding_endpoint_and_credential() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let auth = directory.path().join("auth");
        std::fs::write(
            &path,
            "default_provider = 'zhipu-coding-plan-account'\n[providers]\n",
        )
        .unwrap();
        let provider = orbit_auth::ApiKeyProvider::ZhipuCodingPlan;
        let catalog = serde_json::json!([{ "id": "glm-server-model" }]);

        let models = append_profile_models(
            &path,
            &auth,
            provider.name(),
            ApiFormat::OpenaiChatCompletions,
            provider.base_url(),
            &catalog,
        )
        .unwrap();

        assert_eq!(models[0].model, "glm-server-model");
        assert_eq!(
            models[0].config.credential.as_ref().unwrap().name,
            "zhipu-coding-plan"
        );
        let saved = std::fs::read_to_string(path).unwrap();
        assert!(saved.contains(orbit_auth::ZHIPU_CODING_BASE));
        assert!(!saved.contains("api_key"));
    }

    #[test]
    fn connection_preserves_config_and_reloads_both_account_protocols() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let auth = directory.path().join("auth");
        let original = "# user settings\ndefault_provider = 'local'\n[providers.local]\nbase_url = 'http://localhost:8000/v1'\napi_key = 'test-key'\nmodels = [{ id = 'local-model' }]\n";
        std::fs::write(&path, original).unwrap();

        for (credential, format, endpoint) in [
            (
                "antigravity",
                ApiFormat::GoogleCodeAssist,
                "https://daily-cloudcode-pa.googleapis.com/",
            ),
            (
                "google",
                ApiFormat::GoogleCodeAssist,
                "https://cloudcode-pa.googleapis.com/",
            ),
            (
                "chatgpt",
                ApiFormat::ChatgptCodex,
                "https://chatgpt.com/backend-api/codex/",
            ),
        ] {
            let models = append_profile(&path, &auth, credential, format, endpoint).unwrap();
            let connected = models
                .iter()
                .find(|entry| entry.provider == format!("{credential}-account"))
                .unwrap();

            assert_eq!(
                connected.config.credential.as_ref().unwrap().directory,
                auth
            );
            assert_eq!(
                connected.config.credential.as_ref().unwrap().name,
                credential
            );
            assert!(
                std::fs::read_to_string(&path)
                    .unwrap()
                    .starts_with(original)
            );

            let before = std::fs::read_to_string(&path).unwrap();
            let reconnected = append_profile(&path, &auth, credential, format, endpoint).unwrap();

            assert_eq!(
                crate::FileConfig::load(&path, &auth)
                    .unwrap()
                    .model_options()
                    .unwrap()
                    .len(),
                models.len()
            );
            assert_eq!(reconnected.len(), models.len());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        }

        assert!(!auth.exists());
    }

    #[test]
    fn existing_antigravity_catalog_expands_without_rewriting_user_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let auth = directory.path().join("auth");
        let original = "default_provider = 'account'\n[providers.account]\napi_format = 'google_code_assist'\nbase_url = 'https://daily-cloudcode-pa.googleapis.com/'\ncredential = 'antigravity'\nmodels = [{ id = 'gemini-3-pro', context_tokens = 90000 }, { id = 'custom-model' }]\n";
        std::fs::write(&path, original).unwrap();

        let config = crate::FileConfig::load(&path, &auth).unwrap();
        let options = config.model_options().unwrap();
        let reconnected = append_profile(
            &path,
            &auth,
            "antigravity",
            ApiFormat::GoogleCodeAssist,
            "https://daily-cloudcode-pa.googleapis.com/",
        )
        .unwrap();

        assert_eq!(options.len(), 18);
        assert_eq!(reconnected.len(), options.len());
        assert_eq!(options[0].context_tokens, 90000);
        for id in [
            "gemini-3.8-flash",
            "claude-opus-4-6",
            "gpt-oss-120b",
            "custom-model",
        ] {
            assert!(
                options.iter().any(|model| model.model == id),
                "missing {id}"
            );
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        let connected = append_profile(
            &path,
            &auth,
            "chatgpt",
            ApiFormat::ChatgptCodex,
            "https://chatgpt.com/backend-api/codex/",
        )
        .unwrap();

        assert_eq!(connected.len(), 19);
        assert!(
            connected
                .iter()
                .any(|model| model.model == "gemini-3.8-flash")
        );
        assert!(connected.iter().any(|model| model.model == "gpt-5.5"));
        assert_eq!(
            crate::FileConfig::load(&path, &auth)
                .unwrap()
                .model_options()
                .unwrap()
                .len(),
            connected.len()
        );
        assert!(!auth.exists());
    }

    #[test]
    fn connection_keeps_existing_provider_and_rejects_invalid_config_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let auth = directory.path().join("auth");
        let original = "default_provider = 'google-account'\n[providers.google-account]\nbase_url = 'http://localhost:8000/v1'\napi_key = 'test-key'\nmodels = [{ id = 'existing' }]\n";
        std::fs::write(&path, original).unwrap();

        let models = append_profile(
            &path,
            &auth,
            "google",
            ApiFormat::GoogleCodeAssist,
            "https://cloudcode-pa.googleapis.com/",
        )
        .unwrap();

        assert!(
            models
                .iter()
                .any(|model| model.qualified_name() == "google-account::existing")
        );
        assert!(
            models
                .iter()
                .any(|model| model.provider == "google-account-2")
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with(original)
        );

        std::fs::write(&path, "invalid = [").unwrap();
        let result = append_profile(
            &path,
            &auth,
            "google",
            ApiFormat::GoogleCodeAssist,
            "https://cloudcode-pa.googleapis.com/",
        );

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid = [");
    }

    #[tokio::test]
    async fn dropping_login_cancels_background_work() {
        let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
        let task = AbortOnDropHandle::new(tokio::spawn(async move {
            let _sender = sender;
            std::future::pending::<()>().await;
        }));

        drop(task);

        assert!(
            tokio::time::timeout(Duration::from_secs(1), receiver)
                .await
                .unwrap()
                .is_err()
        );
    }
}
