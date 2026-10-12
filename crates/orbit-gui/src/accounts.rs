//! Credential setup runs independently of session transport. Secrets live only
//! in masked inputs and the authentication store, never in Shell or reports.
use crate::backend::Options;
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use orbit_auth::{AuthStore, CallbackFlow, CallbackListener, OAuthLogin, OAuthProvider};
use std::{path::PathBuf, sync::Arc, time::Duration};

pub(crate) struct Accounts {
    config: PathBuf,
    auth: PathBuf,
    runtime: Arc<tokio::runtime::Runtime>,
    fields: Vec<Entity<InputState>>,
    entries: Vec<orbit_auth::CredentialInfo>,
    error: Option<String>,
    busy: bool,
    loading: bool,
    task: Task<()>,
    operation: Option<tokio::task::JoinHandle<()>>,
    format: orbit_model::client::ApiFormat,
    project: Entity<InputState>,
    adding: bool,
    api_form: bool,
}
enum AccountEvent {
    Browser(String),
    Finished(Result<Vec<orbit_auth::CredentialInfo>, String>),
}
impl Accounts {
    pub(crate) fn new(
        options: &Options,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let auth = orbit_auth::directory(
            options.auth_dir.clone(),
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        );
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
            adding: false,
            api_form: false,
            error,
            busy: false,
            loading: false,
            task: Task::ready(()),
            operation: None,
            format: orbit_model::client::ApiFormat::OpenaiChatCompletions,
            project: cx
                .new(|cx| InputState::new(window, cx).placeholder("Google Cloud 项目 ID（可选）")),
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
        let job = self
            .runtime
            .spawn_blocking(move || AuthStore::new(auth)?.list());
        self.task = cx.spawn(async move |weak, cx| {
            let result = job.await;
            let _ = weak.update(cx, |view, cx| {
                view.loading = false;
                match result {
                    Ok(Ok(entries)) => view.entries = entries,
                    Ok(Err(error)) => view.error = Some(error.to_string()),
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
                    .update(cx, |view, cx| {
                        match event {
                            AccountEvent::Browser(url) => cx.open_url(&url),
                            AccountEvent::Finished(result) => {
                                view.busy = false;
                                match result {
                                    Ok(entries) => {
                                        view.entries = entries;
                                        view.adding = false;
                                        view.api_form = false;
                                    }
                                    Err(error) => view.error = Some(error),
                                }
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
    fn login(&mut self, provider: OAuthProvider, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let auth = self.auth.clone();
        let config = self.config.clone();
        let project = self.project.read(cx).value().trim().to_string();
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        self.operation = Some(self.runtime.spawn(async move {
            let result = async {
                let store = AuthStore::new(&auth)?;
                let login = OAuthLogin::new(provider, (!project.is_empty()).then_some(project))?;
                let callback = CallbackListener::bind_at(login.redirect_uri().clone()).await?;
                sender
                    .send(AccountEvent::Browser(login.authorization_url().to_string()))
                    .await?;
                let grant = callback.wait(&login, Duration::from_secs(300)).await?;
                login.finish(&grant, &store, provider.name()).await?;
                if provider == OAuthProvider::Anthropic {
                    orbit_config::config::connections::append_profile(
                        &config,
                        &auth,
                        provider.name(),
                        orbit_model::client::ApiFormat::AnthropicMessages,
                        "https://api.anthropic.com/",
                    )?;
                } else {
                    orbit_config::config::connections::save_account(&config, &auth, provider)
                        .await?;
                }
                Ok::<_, anyhow::Error>(store.list()?)
            }
            .await
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(AccountEvent::Finished(result)).await;
        }));
        self.receive(receiver, cx);
    }
    fn save_key(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let values = self
            .fields
            .iter()
            .map(|input| input.read(cx).value().trim().to_string())
            .collect::<Vec<_>>();
        if values.iter().any(String::is_empty) {
            self.error = Some("填写连接名称、API 地址、密钥和模型 ID".into());
            cx.notify();
            return;
        }
        let auth = self.auth.clone();
        let config = self.config.clone();
        let format = self.format;
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        self.operation = Some(self.runtime.spawn(async move {
            let result = async {
                let models = values[3]
                    .split(',')
                    .map(|name| orbit_config::config::ModelProfile::new(name.trim()))
                    .collect::<Vec<_>>();
                let mut provider = orbit_config::config::ProviderSpec::new(
                    format,
                    &values[1],
                    orbit_config::config::ModelSelection::Explicit(Vec::new()),
                );
                provider.credential = Some(values[0].clone());
                provider.set_models(&models);
                orbit_config::config::connections::prepare_provider(
                    &config, &auth, &values[0], &provider,
                )?;
                let store = AuthStore::new(&auth)?;
                store
                    .set_api_key(&values[0], values[2].clone(), &values[1])
                    .await?;
                orbit_config::config::connections::save_provider(
                    &config, &auth, &values[0], &provider,
                )?;
                Ok::<_, anyhow::Error>(store.list()?)
            }
            .await
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(AccountEvent::Finished(result)).await;
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
impl Render for Accounts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use orbit_model::client::ApiFormat;
        let formats = [
            (ApiFormat::OpenaiChatCompletions, "OpenAI Chat Completions"),
            (ApiFormat::OpenaiResponses, "OpenAI Responses"),
            (ApiFormat::AnthropicMessages, "Anthropic Messages"),
        ];
        let owner = cx.weak_entity();
        v_flex()
            .w_full()
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::MEDIUM)
                            .child(if self.adding {
                                "添加连接"
                            } else {
                                "已连接的账号"
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
                                    "刷新"
                                })
                                .disabled(self.busy || self.loading)
                                .on_click(cx.listener(|view, _, _, cx| view.load(cx))),
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
                                view.api_form = false;
                                cx.notify();
                            })),
                    ),
            )
            .when(!self.adding, |view| {
                view.when(self.entries.is_empty(), |view| {
                    view.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("添加账号或 API 连接后，即可选择模型。"),
                    )
                })
                .children(self.entries.iter().map(|entry| {
                    v_flex()
                        .gap_1()
                        .py_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(entry.name.clone())
                        .when_some(entry.account.clone(), |view, account| {
                            view.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(account),
                            )
                        })
                }))
            })
            .when(self.adding && !self.api_form, |view| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("选择登录方式"),
                )
                .child(
                    h_flex().gap_2().flex_wrap().children(
                        [
                            (OAuthProvider::Antigravity, "Antigravity"),
                            (OAuthProvider::Chatgpt, "ChatGPT"),
                            (OAuthProvider::Google, "Gemini"),
                            (OAuthProvider::Anthropic, "Claude"),
                        ]
                        .into_iter()
                        .map(|(provider, label)| {
                            Button::new(provider.name())
                                .label(format!("登录 {label}"))
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(move |view, _, _, cx| view.login(provider, cx)),
                                )
                        }),
                    ),
                )
                .child(Input::new(&self.project).disabled(self.busy))
                .child(
                    Button::new("add-api-key")
                        .label("使用 API Key…")
                        .disabled(self.busy)
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.api_form = true;
                            cx.notify();
                        })),
                )
            })
            .when(self.adding && self.api_form, |view| {
                view.child(
                    Button::new("api-format")
                        .label(formats.iter().find(|(f, _)| *f == self.format).unwrap().1)
                        .disabled(self.busy)
                        .dropdown_menu(move |mut menu, _, _| {
                            for (format, label) in formats {
                                let owner = owner.clone();
                                menu = menu.item(PopupMenuItem::new(label).on_click(
                                    move |_, _, cx| {
                                        let _ = owner.update(cx, |view, cx| {
                                            view.format = format;
                                            cx.notify();
                                        });
                                    },
                                ));
                            }
                            menu
                        }),
                )
                .children(
                    ["连接名称", "API 地址", "API Key", "模型 ID，逗号分隔"]
                        .iter()
                        .zip(&self.fields)
                        .map(|(label, input)| {
                            v_flex()
                                .gap_1()
                                .child(*label)
                                .child(Input::new(input).disabled(self.busy))
                        }),
                )
                .child(
                    h_flex().justify_end().child(
                        Button::new("save-api")
                            .primary()
                            .label("保存连接")
                            .disabled(self.busy)
                            .on_click(cx.listener(|view, _, _, cx| view.save_key(cx))),
                    ),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .when(self.busy, |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .child(gpui_kit::component::spinner::Spinner::new())
                        .child("正在连接…"),
                )
            })
    }
}
