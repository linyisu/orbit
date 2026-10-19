//! Native connection editing. Existing secrets are retained by reference and
//! never copied into an input; a blank replacement key keeps the stored key.
mod model_parameters;

use crate::backend::Options;
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, IndexPath, Sizable, WindowExt,
    button::{Button, ButtonVariant, ButtonVariants},
    dialog::DialogButtonProps,
    h_flex,
    input::{Input, InputState},
    searchable_list::SearchableListItem,
    select::{Select, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use model_parameters::ModelParameters;
use orbit_client::accounts::{
    AccountIdentity, ApiKeyProvider, Connection, Login, LoginOutcome, OAuthProvider,
    PendingAccountSetup, connections,
};
use orbit_config::ApiFormat;
use orbit_config::config::connections::edit::{self, ConnectionDraft, KeyChange};
use std::{path::PathBuf, sync::Arc};

pub(crate) enum AccountsEvent {
    Changed,
    RefreshRequested,
}

pub(crate) struct Accounts {
    config: PathBuf,
    auth: PathBuf,
    runtime: Arc<tokio::runtime::Runtime>,
    fields: Vec<Entity<InputState>>,
    entries: Vec<Connection>,
    draft: Option<ConnectionDraft>,
    oauth: Option<OAuthProvider>,
    api_provider: Option<ApiKeyProvider>,
    error: Option<String>,
    busy: bool,
    loading: bool,
    login_stage: Option<LoginStage>,
    task: Task<()>,
    operation: Option<tokio::task::JoinHandle<()>>,
    protocol: Entity<SelectState<Vec<ProtocolChoice>>>,
    project: Entity<InputState>,
    pending_project: Option<PendingAccountSetup>,
    adding: bool,
    setup: Setup,
}

impl EventEmitter<AccountsEvent> for Accounts {}

pub(crate) enum AccountCheck {
    Loading,
    Providers,
    Chatgpt,
    Google,
    Official,
    Editing,
    Discovering,
    Saving,
}

impl AccountCheck {
    pub(crate) fn frame_name(&self) -> &'static str {
        match self {
            Self::Loading => "accounts",
            Self::Providers => "providers",
            Self::Chatgpt => "chatgpt",
            Self::Google => "google",
            Self::Official => "official-api",
            Self::Editing => "custom-api",
            Self::Discovering => "discovered-models",
            Self::Saving => "saved-account",
        }
    }
}

enum AccountEvent {
    Browser(String),
    ProjectRequired(PendingAccountSetup, Option<String>),
    Connected(Result<Vec<Connection>, String>, Vec<String>),
    Finished(Result<Vec<Connection>, String>),
    Removed(Result<Vec<Connection>, String>, Option<String>),
    Models(Result<Vec<orbit_config::config::ModelProfile>, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoginStage {
    Browser,
    Project,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Setup {
    Providers,
    OAuth(OAuthProvider),
    Api,
}

#[derive(Clone)]
struct ProtocolChoice(ApiFormat);

impl SearchableListItem for ProtocolChoice {
    type Value = ApiFormat;

    fn title(&self) -> SharedString {
        format_label(self.0).into()
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

/// Keep the configured protocol visible even for a managed account connection.
fn protocol_choices(current: ApiFormat) -> Vec<ProtocolChoice> {
    let mut formats = vec![
        ApiFormat::OpenaiChatCompletions,
        ApiFormat::OpenaiResponses,
        ApiFormat::AnthropicMessages,
    ];
    if !formats.contains(&current) {
        formats.push(current);
    }
    formats
        .into_iter()
        .map(ProtocolChoice)
        .collect()
}

impl Accounts {
    /// Drive the same handlers as the native controls in an explicitly requested CI probe.
    pub(crate) fn check_account_edit(
        &mut self,
        step: &mut AccountCheck,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(self.error.is_none(), "account probe: {:?}", self.error);
        if self.busy || self.loading {
            return Ok(false);
        }
        match step {
            AccountCheck::Loading => {
                self.adding = true;
                self.setup = Setup::Providers;
                *step = AccountCheck::Providers;
                cx.notify();
            }
            AccountCheck::Providers => {
                self.setup = Setup::OAuth(OAuthProvider::Chatgpt);
                *step = AccountCheck::Chatgpt;
                cx.notify();
            }
            AccountCheck::Chatgpt => {
                self.setup = Setup::OAuth(OAuthProvider::Google);
                *step = AccountCheck::Google;
                cx.notify();
            }
            AccountCheck::Google => {
                self.edit(None, Some(ApiKeyProvider::Deepseek), window, cx);
                *step = AccountCheck::Official;
            }
            AccountCheck::Official => {
                let name = self
                    .entries
                    .first()
                    .ok_or_else(|| {
                        anyhow::anyhow!("account probe requires an isolated saved connection")
                    })?
                    .draft
                    .name
                    .clone();
                self.edit(Some(name), None, window, cx);
                *step = AccountCheck::Editing;
            }
            AccountCheck::Editing => {
                anyhow::ensure!(self.draft.is_some(), "account edit form did not open");
                self.fields[2].update(cx, |input, cx| {
                    input.set_value("acceptance-key", window, cx)
                });
                self.discover_models(cx);
                *step = AccountCheck::Discovering;
            }
            AccountCheck::Discovering => {
                anyhow::ensure!(
                    self.draft
                        .as_ref()
                        .is_some_and(|draft| !draft.models.is_empty()),
                    "model discovery returned no models"
                );
                self.save_key(cx);
                *step = AccountCheck::Saving;
            }
            AccountCheck::Saving => {
                anyhow::ensure!(
                    !self.adding && self.draft.is_none(),
                    "account save did not finish"
                );
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn has_pending_work(&self) -> bool {
        self.busy
            || self.loading
            || self.login_stage.is_some()
            || self.adding
            || self.draft.is_some()
    }

    pub(crate) fn new(
        options: &Options,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let auth = orbit_client::configuration::auth_directory(options.auth_dir.clone());
        let (auth, error) = match auth {
            Ok(path) => (path, None),
            Err(error) => (PathBuf::new(), Some(error.to_string())),
        };
        let mut view = Self {
            config: options.config.clone(),
            auth,
            runtime,
            fields: ["连接名称", "API 地址", "API Key", "模型 ID，逗号分隔"]
                .into_iter()
                .enumerate()
                .map(|(ix, label)| {
                    cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(label)
                            .masked(ix == 2)
                    })
                })
                .collect(),
            entries: Vec::new(),
            draft: None,
            oauth: None,
            api_provider: None,
            adding: false,
            setup: Setup::Providers,
            error,
            busy: false,
            loading: false,
            login_stage: None,
            task: Task::ready(()),
            operation: None,
            protocol: cx.new(|cx| {
                SelectState::new(
                    protocol_choices(ApiFormat::OpenaiChatCompletions),
                    Some(IndexPath::default()),
                    window,
                    cx,
                )
            }),
            project: cx
                .new(|cx| InputState::new(window, cx).placeholder("Google Cloud 项目 ID（可选）")),
            pending_project: None,
        };
        view.load(cx);
        view
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.auth.as_os_str().is_empty() || self.busy || self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        let auth = self.auth.clone();
        let config = self.config.clone();
        let job = self
            .runtime
            .spawn_blocking(move || connections(&config, &auth));
        self.task = cx.spawn(async move |weak, cx| {
            let result = job.await;
            let _ = weak.update(cx, |view, cx| {
                view.loading = false;
                match result {
                    Ok(Ok(entries)) => view.entries = entries,
                    Ok(Err(error)) => view.error = Some(format!("{error:#}")),
                    Err(error) => view.error = Some(error.to_string()),
                };
                cx.notify();
            });
        });
        cx.notify();
    }

    fn receive(
        &mut self,
        mut receiver: tokio::sync::mpsc::Receiver<AccountEvent>,
        cx: &mut Context<Self>,
    ) {
        self.busy = true;
        self.error = None;
        self.task = cx.spawn(async move |weak, cx| {
            while let Some(event) = receiver.recv().await {
                if weak
                    .update_in(cx, |view, window, cx| {
                        match event {
                            AccountEvent::Models(result) => {
                                view.busy = false;
                                view.operation = None;
                                match result {
                                    Ok(models) => {
                                        let ids = models
                                            .iter()
                                            .map(|model| model.id.as_str())
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                        view.fields[3].update(cx, |input, cx| {
                                            input.set_value(ids, window, cx)
                                        });
                                        if let Some(draft) = &mut view.draft {
                                            draft.models = models;
                                        }
                                    }
                                    Err(error) => view.error = Some(error),
                                }
                            }
                            AccountEvent::Browser(url) => cx.open_url(&url),
                            AccountEvent::ProjectRequired(pending, error) => {
                                view.pending_project = Some(pending);
                                view.adding = true;
                                view.setup = Setup::OAuth(OAuthProvider::Google);
                                view.busy = false;
                                view.login_stage = None;
                                view.operation = None;
                                view.error = error;
                                view.project
                                    .update(cx, |input, cx| input.focus(window, cx));
                            }
                            AccountEvent::Connected(result, diagnostics) => {
                                view.busy = false;
                                view.login_stage = None;
                                view.operation = None;
                                view.pending_project = None;
                                if !diagnostics.is_empty() {
                                    window.push_notification(
                                        format!(
                                            "连接已保存，模型刷新未完成：{}",
                                            diagnostics.join("；")
                                        ),
                                        cx,
                                    );
                                }
                                match result {
                                    Ok(entries) => {
                                        view.entries = entries;
                                        view.adding = false;
                                        view.setup = Setup::Providers;
                                        view.draft = None;
                                        view.fields[2].update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                    }
                                    Err(error) => {
                                        view.error =
                                            Some(format!("连接已保存，列表加载失败：{error}"));
                                    }
                                }
                                cx.emit(AccountsEvent::Changed);
                            }
                            AccountEvent::Finished(result)
                            | AccountEvent::Removed(result, None) => {
                                view.busy = false;
                                view.login_stage = None;
                                view.operation = None;
                                match result {
                                    Ok(entries) => {
                                        view.entries = entries;
                                        view.adding = false;
                                        view.setup = Setup::Providers;
                                        view.draft = None;
                                        view.fields[2].update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                        cx.emit(AccountsEvent::Changed);
                                    }
                                    Err(error) => view.error = Some(error),
                                }
                            }
                            AccountEvent::Removed(result, Some(error)) => {
                                view.busy = false;
                                view.operation = None;
                                if let Ok(entries) = result {
                                    view.entries = entries;
                                }
                                view.error = Some(error);
                                cx.emit(AccountsEvent::Changed);
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        cx.notify();
    }

    fn edit(
        &mut self,
        name: Option<String>,
        provider: Option<ApiKeyProvider>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.loading {
            return;
        }
        self.oauth = name.as_ref().and_then(|name| {
            self.entries
                .iter()
                .find(|entry| &entry.draft.name == name)
                .and_then(|entry| entry.oauth)
        });
        self.fields[2].update(cx, |input, cx| input.set_value("", window, cx));
        self.loading = true;
        self.api_provider = provider;
        self.error = None;
        let config = self.config.clone();
        let job = self
            .runtime
            .spawn_blocking(move || ConnectionDraft::load(&config, name.as_deref()));
        self.task = cx.spawn(async move |owner, cx| {
            let result = job.await;
            let _ = owner.update_in(cx, |view, window, cx| {
                view.loading = false;
                match result {
                    Ok(Ok(mut draft)) => {
                        if let Some(provider) = provider {
                            draft.name = provider.name().into();
                            draft.endpoint = provider.base_url().into();
                        }
                        view.protocol.update(cx, |picker, cx| {
                            picker.set_items(protocol_choices(draft.format), window, cx);
                            picker.set_selected_value(&draft.format, window, cx);
                        });
                        let values = [
                            draft.name.clone(),
                            draft.endpoint.clone(),
                            String::new(),
                            draft
                                .models
                                .iter()
                                .map(|model| model.id.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                        ];
                        for (input, value) in view.fields.iter().zip(values) {
                            input.update(cx, |input, cx| input.set_value(value, window, cx));
                        }
                        view.fields[2].update(cx, |input, cx| {
                            input.set_placeholder(
                                if draft.original_name().is_some() {
                                    "留空以保留现有密钥"
                                } else if provider.is_some() {
                                    "输入 API Key"
                                } else {
                                    "没有密钥时可留空"
                                },
                                window,
                                cx,
                            )
                        });
                        view.draft = Some(draft);
                        view.adding = true;
                        view.setup = Setup::Api;
                        let ix = if provider.is_some() {
                            2
                        } else {
                            usize::from(
                                view.draft
                                    .as_ref()
                                    .is_some_and(|draft| draft.original_name().is_some()),
                            )
                        };
                        view.fields[ix].update(cx, |input, cx| input.focus(window, cx));
                    }
                    Ok(Err(error)) => view.error = Some(format!("{error:#}")),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn login(&mut self, provider: OAuthProvider, cx: &mut Context<Self>) {
        self.start_login(provider, None, cx);
    }

    fn start_login(
        &mut self,
        provider: OAuthProvider,
        credential: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.loading {
            return;
        }
        let auth = self.auth.clone();
        let config = self.config.clone();
        let project = self
            .project
            .read(cx)
            .value()
            .trim()
            .to_string();
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        self.pending_project = None;
        self.login_stage = Some(LoginStage::Browser);
        self.operation = Some(self.runtime.spawn(async move {
            let result = async {
                let login = Login::start(
                    provider,
                    (provider == OAuthProvider::Google && !project.is_empty()).then_some(project),
                    &auth,
                    credential,
                )
                .await?;
                sender
                    .send(AccountEvent::Browser(login.url().to_owned()))
                    .await?;
                match login.finish().await? {
                    LoginOutcome::Connected(account) => {
                        saved_account_event(account, &config, &auth).await
                    }
                    LoginOutcome::GoogleProjectRequired(pending) => {
                        Ok(AccountEvent::ProjectRequired(pending, None))
                    }
                }
            }
            .await
            .map_err(|error| format!("{error:#}"));
            let event = result.unwrap_or_else(|error| AccountEvent::Finished(Err(error)));
            let _ = sender.send(event).await;
        }));
        self.receive(receiver, cx);
    }

    fn cancel_login(&mut self, cx: &mut Context<Self>) {
        if let Some(operation) = self.operation.take() {
            operation.abort();
        }
        self.task = Task::ready(());
        self.busy = false;
        self.login_stage = None;
        self.pending_project = None;
        self.load(cx);
    }

    fn finish_project(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let project = self
            .project
            .read(cx)
            .value()
            .trim()
            .to_owned();
        let Some(mut pending) = self.pending_project.take() else {
            return;
        };
        let config = self.config.clone();
        let auth = self.auth.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        self.login_stage = Some(LoginStage::Project);
        self.operation = Some(self.runtime.spawn(async move {
            let event = match pending.finish(&project).await {
                Ok(account) => saved_account_event(account, &config, &auth)
                    .await
                    .unwrap_or_else(|error| AccountEvent::Finished(Err(format!("{error:#}")))),
                Err(error) => AccountEvent::ProjectRequired(pending, Some(format!("{error:#}"))),
            };
            let _ = sender.send(event).await;
        }));
        self.receive(receiver, cx);
    }

    fn parameters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let Some(draft) = self.draft.as_ref() else {
            return;
        };
        let profiles = match orbit_config::config::connections::models::profiles(
            &self.fields[3].read(cx).value(),
            &draft.models,
        ) {
            Ok(profiles) if !profiles.is_empty() => profiles,
            Ok(_) => {
                self.error = Some("先填写至少一个模型 ID，再编辑参数。".into());
                cx.notify();
                return;
            }
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let snapshot = draft.clone();
        let defaults = match draft.model_defaults() {
            Ok(defaults) => defaults,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let editor = cx.new(|cx| ModelParameters::new(profiles, defaults, window, cx));
        let owner = cx.weak_entity();
        let apply: std::rc::Rc<dyn Fn(&mut App) -> bool> = {
            let editor = editor.clone();
            std::rc::Rc::new(move |cx| {
                let Some(profiles) = editor.update(cx, |editor, cx| editor.values(cx)) else {
                    return false;
                };
                let updated = owner
                    .update(cx, |view, cx| {
                        if view.busy || view.loading {
                            return false;
                        }
                        let Some(draft) = view.draft.as_mut() else {
                            return false;
                        };
                        if !draft.same_snapshot(&snapshot) {
                            return false;
                        }
                        draft.models = profiles;
                        view.error = None;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !updated {
                    editor.update(cx, |editor, cx| {
                        editor.set_error("连接已切换，请关闭参数窗口后重新打开。", cx)
                    });
                }
                updated
            })
        };
        let first = std::cell::Cell::new(true);
        crate::dialog::open(window, cx, move |dialog, window, cx| {
            if first.replace(false) {
                editor.update(cx, |editor, cx| editor.focus(window, cx));
            }
            let keyboard_apply = apply.clone();
            let button_apply = apply.clone();
            dialog
                .title("模型参数")
                .child(editor.clone())
                .on_ok(move |_, _, cx| keyboard_apply(cx))
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Button::new("cancel-model-parameters")
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("apply-model-parameters")
                                .primary()
                                .label("应用到表单")
                                .on_click(move |_, window, cx| {
                                    if button_apply(cx) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
        });
    }

    fn save_key(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        if let Some(provider) = self.api_provider {
            let secret = self.fields[2]
                .read(cx)
                .value()
                .trim()
                .to_owned();
            let config = self.config.clone();
            let auth = self.auth.clone();
            let (sender, receiver) = tokio::sync::mpsc::channel(1);
            self.operation = Some(self.runtime.spawn(async move {
                let event =
                    match orbit_client::accounts::connect_api_key(&config, &auth, provider, secret)
                        .await
                    {
                        Ok(outcome) => AccountEvent::Connected(
                            connections(&config, &auth).map_err(|error| format!("{error:#}")),
                            outcome.diagnostics,
                        ),
                        Err(error) => AccountEvent::Finished(Err(format!("{error:#}"))),
                    };
                let _ = sender.send(event).await;
            }));
            self.receive(receiver, cx);
            return;
        }
        let Some(mut draft) = self.draft.clone() else {
            return;
        };
        let values = self
            .fields
            .iter()
            .map(|input| {
                input
                    .read(cx)
                    .value()
                    .trim()
                    .to_string()
            })
            .collect::<Vec<_>>();
        if values[0].is_empty() || values[1].is_empty() {
            self.error = Some("填写连接名称和 API 地址。".into());
            cx.notify();
            return;
        }
        draft.name = values[0].clone();
        draft.endpoint = values[1].clone();
        if let Some(format) = self.protocol.read(cx).selected_value() {
            draft.format = *format;
        }
        draft.models =
            match orbit_config::config::connections::models::profiles(&values[3], &draft.models) {
                Ok(profiles) => profiles,
                Err(error) => {
                    self.error = Some(error.to_string());
                    cx.notify();
                    return;
                }
            };
        let auth = self.auth.clone();
        let config = self.config.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        self.operation = Some(self.runtime.spawn(async move {
            let result = async {
                let key = KeyChange::from_input(&values[2], draft.original_name().is_some());
                edit::save(&config, &auth, &draft, key).await?;
                connections(&config, &auth)
            }
            .await
            .map_err(|error| format!("{error:#}"));
            let _ = sender
                .send(AccountEvent::Finished(result))
                .await;
        }));
        self.receive(receiver, cx);
    }

    fn discover_models(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let Some(mut draft) = self.draft.clone() else {
            return;
        };
        draft.name = self.fields[0]
            .read(cx)
            .value()
            .trim()
            .into();
        draft.endpoint = self.fields[1]
            .read(cx)
            .value()
            .trim()
            .into();
        if let Some(format) = self.protocol.read(cx).selected_value() {
            draft.format = *format;
        }
        let secret = self.fields[2]
            .read(cx)
            .value()
            .trim()
            .to_owned();
        let auth = self.auth.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        self.operation = Some(self.runtime.spawn(async move {
            let key = KeyChange::from_input(&secret, draft.original_name().is_some());
            let result = orbit_config::config::connections::models::discover(&draft, &key, &auth)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = sender
                .send(AccountEvent::Models(result))
                .await;
        }));
        self.receive(receiver, cx);
    }

    fn confirm_remove(
        &mut self,
        draft: ConnectionDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        crate::dialog::alert(window, cx, move |dialog, _, _| {
            let owner = owner.clone();
            let draft = draft.clone();
            dialog.confirm().title(format!("移除“{}”？", draft.name))
                .description("此连接将从可用模型中移除。仅由它使用的凭据会一并删除，使用它的压缩模型选择将恢复默认。")
                .button_props(DialogButtonProps::default()
                    .ok_text("移除")
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text("取消"))
                .on_ok(move |_,
                    _,
                    cx| {
                    let _ = owner
                    .update(cx,
                    |view,
                    cx| view
                    .remove(draft
                    .clone(),
                    cx));
                    true
                })
        });
    }

    fn remove(&mut self, draft: ConnectionDraft, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let config = self.config.clone();
        let auth = self.auth.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        self.operation = Some(self.runtime.spawn(async move {
            let error = edit::remove(&config, &auth, &draft)
                .await
                .err()
                .map(|error| format!("{error:#}"));
            let entries = connections(&config, &auth).map_err(|error| format!("{error:#}"));
            let _ = sender
                .send(AccountEvent::Removed(entries, error))
                .await;
        }));
        self.receive(receiver, cx);
    }
}

impl Drop for Accounts {
    fn drop(&mut self) {
        if let Some(task) = &self.operation {
            task.abort();
        }
    }
}

async fn saved_account_event(
    account: AccountIdentity,
    config: &std::path::Path,
    auth: &std::path::Path,
) -> anyhow::Result<AccountEvent> {
    let outcome = account.save(config).await?;
    let entries = connections(config, auth).map_err(|error| format!("{error:#}"));
    Ok(AccountEvent::Connected(entries, outcome.diagnostics))
}

fn format_label(format: ApiFormat) -> &'static str {
    match format {
        ApiFormat::OpenaiChatCompletions => "OpenAI Chat Completions",
        ApiFormat::OpenaiResponses => "OpenAI Responses",
        ApiFormat::AnthropicMessages => "Anthropic Messages",
        ApiFormat::GoogleCodeAssist => "Google Code Assist",
        ApiFormat::ChatgptCodex => "ChatGPT Codex",
    }
}

impl Render for Accounts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let awaiting_project =
            self.pending_project.is_some() || self.login_stage == Some(LoginStage::Project);
        let editing = self
            .draft
            .as_ref()
            .is_some_and(|draft| draft.original_name().is_some());
        let managed = self.oauth.is_some()
            || self
                .draft
                .as_ref()
                .is_some_and(ConnectionDraft::account);
        let mut content = v_flex()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::MEDIUM)
                            .child(if self.setup == Setup::Api {
                                if editing {
                                    "编辑连接"
                                } else {
                                    "添加 API 连接"
                                }
                            } else if self.adding {
                                "添加连接"
                            } else {
                                "已保存的连接"
                            }),
                    )
                    .when(!self.adding, |view| {
                        view.child(
                            Button::new("refresh-connections")
                                .ghost()
                                .small()
                                .icon(IconName::RefreshCw)
                                .label(if self.loading {
                                    "刷新中…"
                                } else {
                                    "刷新模型"
                                })
                                .disabled(self.busy || self.loading)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.load(cx);
                                    cx.emit(AccountsEvent::RefreshRequested);
                                })),
                        )
                    })
                    .child(
                        Button::new("add-connection")
                            .small()
                            .label(if self.adding {
                                "返回列表"
                            } else {
                                "添加连接…"
                            })
                            .disabled(self.busy || self.loading)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.adding = !view.adding;
                                view.setup = Setup::Providers;
                                view.draft = None;
                                view.oauth = None;
                                view.api_provider = None;
                                view.pending_project = None;
                                view.error = None;
                                cx.notify();
                            })),
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            });
        if !self.adding {
            if self.entries.is_empty() {
                content = content.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if self.loading {
                            "正在加载连接…"
                        } else {
                            "添加账号或 API 连接后，即可选择模型。"
                        }),
                );
            }
            for entry in &self.entries {
                let name = entry.draft.name.clone();
                let remove = entry.draft.clone();
                let oauth = entry.oauth;
                let credential = entry
                    .draft
                    .provider()
                    .and_then(|provider| provider.credential.clone());
                content = content.child(
                    h_flex()
                        .gap_3()
                        .py_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(name.clone())
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "{} · {}",
                                            format_label(entry.draft.format),
                                            entry.draft.endpoint
                                        )),
                                )
                                .when_some(entry.account.clone(), |view, account| {
                                    view.child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(account),
                                    )
                                })
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            if entry
                                                .draft
                                                .provider()
                                                .is_some_and(|provider| {
                                                    provider.spec().models.is_auto()
                                                })
                                            {
                                                "自动发现模型".to_owned()
                                            } else {
                                                format!("{} 个模型", entry.draft.models.len())
                                            },
                                        ),
                                ),
                        )
                        .child(
                            Button::new(SharedString::from(format!("edit-connection:{name}")))
                                .small()
                                .label("编辑…")
                                .disabled(self.busy || self.loading)
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.edit(Some(name.clone()), None, window, cx)
                                })),
                        )
                        .when_some(oauth, |view, provider| {
                            view.child(
                                Button::new(SharedString::from(format!(
                                    "login-connection:{}",
                                    entry.draft.name
                                )))
                                .ghost()
                                .small()
                                .label("重新登录")
                                .disabled(self.busy || self.loading)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.start_login(provider, credential.clone(), cx)
                                })),
                            )
                        })
                        .child(
                            Button::new(SharedString::from(format!(
                                "remove-connection:{}",
                                entry.draft.name
                            )))
                            .ghost()
                            .small()
                            .label("移除…")
                            .disabled(self.busy || self.loading)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.confirm_remove(remove.clone(), window, cx)
                            })),
                        ),
                );
            }
        } else if self.setup == Setup::Providers && !awaiting_project {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("选择服务商。订阅账号通过浏览器登录，API 连接只需填写密钥。"),
            );
            content = content.child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("订阅账号"),
            );
            for (provider, label, description) in [
                (OAuthProvider::Chatgpt, "ChatGPT", "使用 ChatGPT 账号授权"),
                (OAuthProvider::Anthropic, "Claude", "使用 Claude 账号授权"),
                (
                    OAuthProvider::Google,
                    "Gemini",
                    "使用 Google 账号授权 Gemini CLI",
                ),
                (
                    OAuthProvider::Antigravity,
                    "Antigravity",
                    "使用 Google 账号授权 Antigravity",
                ),
            ] {
                content = content.child(
                    h_flex()
                        .gap_4()
                        .py_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(label)
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(description),
                                ),
                        )
                        .child(
                            Button::new(provider.name())
                                .label("登录…")
                                .disabled(self.busy || self.loading)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.setup = Setup::OAuth(provider);
                                    view.error = None;
                                    cx.notify();
                                })),
                        ),
                );
            }
            content = content.child(
                div()
                    .mt_2()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("API 密钥"),
            );
            for provider in [ApiKeyProvider::Deepseek, ApiKeyProvider::ZhipuCodingPlan] {
                content = content.child(
                    h_flex()
                        .gap_4()
                        .py_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(provider.label())
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("使用官方地址，自动获取模型与能力"),
                                ),
                        )
                        .child(
                            Button::new(provider.name())
                                .label("添加…")
                                .disabled(self.busy || self.loading)
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.edit(None, Some(provider), window, cx)
                                })),
                        ),
                );
            }
            content =
                content.child(
                    h_flex()
                        .gap_4()
                        .py_2()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child("自定义 API")
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("连接其他服务或本机模型"),
                                ),
                        )
                        .child(
                            Button::new("add-api-key")
                                .label("配置…")
                                .disabled(self.busy || self.loading)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.edit(None, None, window, cx)
                                })),
                        ),
                );
        } else if let Setup::OAuth(provider) = self.setup {
            content = content
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(match provider {
                            OAuthProvider::Chatgpt => "ChatGPT",
                            OAuthProvider::Anthropic => "Claude",
                            OAuthProvider::Google => "Gemini",
                            OAuthProvider::Antigravity => "Antigravity",
                        }),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if awaiting_project {
                            "账号已授权，填写项目后继续连接"
                        } else {
                            "在浏览器中完成账号授权，Orbit 不会读取账号密码。"
                        }),
                )
                .when(
                    awaiting_project || provider == OAuthProvider::Google,
                    |view| {
                        view.child(
                            v_flex()
                                .gap_1()
                                .child(if awaiting_project {
                                    "Google Cloud 项目 ID"
                                } else {
                                    "Google Cloud 项目 ID（可选）"
                                })
                                .child(
                                    Input::new(&self.project)
                                        .aria_label("Google Cloud 项目 ID")
                                        .disabled(self.busy),
                                ),
                        )
                    },
                )
                .when(!awaiting_project, |view| {
                    view.child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new("choose-other-provider")
                                    .label("选择其他服务商")
                                    .disabled(self.busy || self.loading)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.setup = Setup::Providers;
                                        view.error = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("login-selected-account")
                                    .primary()
                                    .label("在浏览器中登录")
                                    .disabled(self.busy || self.loading)
                                    .on_click(
                                        cx.listener(move |view, _, _, cx| view.login(provider, cx)),
                                    ),
                            ),
                    )
                })
                .when(awaiting_project && !self.busy, |view| {
                    view.child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("finish-account-project")
                                    .primary()
                                    .label("继续连接")
                                    .disabled(self.busy || self.loading)
                                    .on_click(
                                        cx.listener(|view, _, _, cx| view.finish_project(cx)),
                                    ),
                            )
                            .child(
                                Button::new("cancel-account-project")
                                    .label("取消")
                                    .disabled(self.busy || self.loading)
                                    .on_click(cx.listener(|view, _, _, cx| view.cancel_login(cx))),
                            ),
                    )
                });
        } else if let Some(provider) = self.api_provider {
            content = content
                .child(div().child(provider.label()))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "使用官方地址 {}，验证密钥后自动获取可用模型。",
                            provider.base_url()
                        )),
                )
                .child(
                    v_flex().gap_1().child("API Key").child(
                        Input::new(&self.fields[2])
                            .aria_label("API Key")
                            .disabled(self.busy),
                    ),
                )
                .child(
                    h_flex().justify_end().child(
                        Button::new("save-official-api")
                            .primary()
                            .label("验证并保存")
                            .disabled(self.busy || self.loading)
                            .on_click(cx.listener(|view, _, _, cx| view.save_key(cx))),
                    ),
                );
        } else {
            let protocol = div().w_full().h_8().child(
                Select::new(&self.protocol)
                    .accessibility_label("API 协议")
                    .disabled(self.busy || managed),
            );
            let fields = ["连接名称", "API 地址", "API Key", "模型 ID，逗号分隔"]
                .into_iter()
                .enumerate()
                .zip(&self.fields)
                .map(|((ix, label), input)| {
                    v_flex().gap_1().child(label).child(
                        Input::new(input)
                            .aria_label(label)
                            .disabled(
                                self.busy
                                    || (editing && ix == 0)
                                    || (managed && matches!(ix, 1 | 2)),
                            ),
                    )
                });
            content = content
                .child(v_flex().gap_1().child("API 协议").child(protocol))
                .children(fields)
                .when(editing, |view| view.child(
                    div().text_sm().text_color(cx.theme().muted_foreground)
                        .child(if managed {
                            "认证协议与服务地址由登录流程管理。已有模型的高级选项会保留。"
                        } else {
                            "密钥留空会保留现有认证。修改 API 地址时需填写该地址的新密钥。已有模型的高级选项会保留。"
                        }),
                ))
                .child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Button::new("discover-models")
                                .label("检测模型")
                                .disabled(self.busy || self.loading)
                                .on_click(cx.listener(|view, _, _, cx| view.discover_models(cx))),
                        )
                        .child(
                            Button::new("model-parameters")
                                .label("模型参数…")
                                .disabled(self.busy || self.loading)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.parameters(window, cx)
                                })),
                        )
                        .child(
                            Button::new("save-api")
                                .primary()
                                .label("保存连接")
                                .disabled(self.busy || self.loading)
                                .on_click(cx.listener(|view, _, _, cx| view.save_key(cx))),
                        ),
                );
        }
        content.when(self.busy, |view| {
            view.child(
                h_flex()
                    .gap_2()
                    .child(gpui_kit::component::spinner::Spinner::new())
                    .child(match self.login_stage {
                        Some(LoginStage::Browser) => "等待浏览器登录…",
                        Some(LoginStage::Project) => "正在初始化项目…",
                        None => "正在验证连接与模型…",
                    })
                    .when(self.login_stage.is_some(), |view| {
                        view.child(
                            Button::new("cancel-account-login")
                                .small()
                                .label("取消登录")
                                .on_click(cx.listener(|view, _, _, cx| view.cancel_login(cx))),
                        )
                    }),
            )
        })
    }
}
