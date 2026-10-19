//! Component composition for the workspace and message composer.
use super::*;
use crate::preferences::Preferences;
use crate::theme::{CONTENT_GUTTER_REM, CONTENT_WIDTH_REM};
use orbit_client::protocol::Mode;

impl Workspace {
    fn composer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(form) = &self.question_form
            && !form.read(cx).hidden()
        {
            return form.clone().into_any_element();
        }
        let paste_owner = cx.weak_entity();
        v_flex()
            .id("composer")
            .key_context("OrbitComposer")
            .capture_action(cx.listener(
                |view, _: &gpui_kit::component::input::Paste, window, cx| {
                    view.paste_file_list(window, cx)
                },
            ))
            .capture_action(cx.listener(
                |view, _: &gpui_kit::component::input::MoveUp, window, cx| {
                    view.history_arrow(true, window, cx)
                },
            ))
            .capture_action(cx.listener(
                |view, _: &gpui_kit::component::input::MoveDown, window, cx| {
                    view.history_arrow(false, window, cx)
                },
            ))
            .on_action(cx.listener(|view, _: &InputHistoryPrevious, window, cx| {
                view.input_history(true, window, cx)
            }))
            .on_action(cx.listener(|view, _: &InputHistoryNext, window, cx| {
                view.input_history(false, window, cx)
            }))
            .relative()
            .when(self.command_open, |view| {
                view.key_context("OrbitComposer ComposerCompletions")
                    .on_action(cx.listener(|view, _: &CompletionNext, window, cx| {
                        view.move_completion(1, window, cx)
                    }))
                    .on_action(cx.listener(|view, _: &CompletionPrevious, window, cx| {
                        view.move_completion(-1, window, cx)
                    }))
                    .on_action(cx.listener(|view, _: &CompletionSubmit, window, cx| {
                        view.submit_completion(window, cx)
                    }))
                    .on_action(cx.listener(|view, _: &CompletionConfirm, window, cx| {
                        view.confirm_completion(window, cx)
                    }))
                    .on_action(cx.listener(|view, _: &CompletionDismiss, _, cx| {
                        view.dismiss_completions(cx)
                    }))
                    .child(deferred(self.completion_menu(window, cx)).with_priority(1))
            })
            .on_drop(cx.listener(|view, paths: &ExternalPaths, window, cx| {
                view.add_paths(&paths.0, window, cx)
            }))
            .w_full()
            .gap_2()
            .when(self.busy, |view| {
                view.child(
                    div().px_2().py_1().w_full().child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .child(Icon::new(IconName::Clock).small())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_ellipsis()
                                    .child(
                                        self.conversation
                                            .read(cx)
                                            .activity_label(),
                                    ),
                            ),
                    ),
                )
            })
            .when(self.is_new_conversation(cx), |view| {
                view.child(self.composer_target(cx))
            })
            .child(
                v_flex()
                    .w_full()
                    .when_some(self.input_queue.clone(), |view, queue| view.child(queue))
                    .child(
                        v_flex()
                            .w_full()
                            .p_3()
                            .gap_2()
                            .bg(cx.theme().group_box)
                            .border_1()
                            .border_color(cx.theme().border)
                            .when(
                                self.input
                                    .read(cx)
                                    .focus_handle(cx)
                                    .is_focused(window),
                                |view| view.border_color(cx.theme().ring),
                            )
                            .rounded(cx.theme().radius_lg)
                            .when(!self.paths.is_empty(), |view| {
                                view.child(
                                    h_flex().gap_2().flex_wrap().children(
                                        self.paths
                                            .iter()
                                            .map(|path| self.composer_attachment(path, cx)),
                                    ),
                                )
                            })
                            .child(
                                Textarea::new(&self.input)
                                    .content_padding(Edges::default())
                                    .on_paste(move |item, window, cx| {
                                        paste_owner
                                            .update(cx, |view, cx| {
                                                view.paste_attachments(item, window, cx)
                                            })
                                            .unwrap_or(false)
                                    })
                                    .appearance(false)
                                    .bordered(false)
                                    .aria_label("消息输入框"),
                            )
                            .child(self.composer_toolbar(cx)),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .flex_wrap()
                    .when(self.recovery.is_active(), |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(self.recovery.label()),
                        )
                    })
                    .when(
                        !self.connected
                            && !self.opening
                            && !self.recovery.is_active()
                            && self.info.is_some(),
                        |row| {
                            row.child(
                                Button::new("reconnect-task")
                                    .ghost()
                                    .xsmall()
                                    .label("重新连接")
                                    .on_click(
                                        cx.listener(|view, _, window, cx| view.connect(window, cx)),
                                    ),
                            )
                        },
                    )
                    .when(
                        self.options.ssh.is_some() || self.options.wsl.is_some(),
                        |row| {
                            row.child(
                                Button::new("retained-connection-logs")
                                    .ghost()
                                    .xsmall()
                                    .label("连接日志")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.open_connection_logs(window, cx)
                                    })),
                            )
                        },
                    )
                    .child(
                        Button::new("composer-context")
                            .ghost()
                            .xsmall()
                            .label(format!(
                                "上下文 {}%",
                                self.info.as_ref().map_or(0, |info| {
                                    orbit_client::usage::context_percent(
                                        self.context_used,
                                        info.input_context_tokens,
                                    )
                                })
                            ))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.execute(super::actions::WorkspaceCommand::Context, window, cx)
                            })),
                    )
                    .when(self.error.is_some(), |row| {
                        row.child(
                            Button::new("composer-error")
                                .ghost()
                                .xsmall()
                                .icon(IconName::CircleAlert)
                                .label("查看错误")
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.show_error(window, cx)),
                                ),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("composer-usage")
                            .ghost()
                            .xsmall()
                            .child(self.usage.read(cx).summary(cx))
                            .accessibility_label("查看模型用量")
                            .on_click(
                                cx.listener(|view, _, window, cx| view.show_usage(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn composer_attachment(&self, path: &std::path::Path, cx: &mut Context<Self>) -> Attachment {
        let remove = path.to_owned();
        let image = composer::is_image(path);
        let name = self
            .attachment_names
            .get(path)
            .cloned()
            .unwrap_or_else(|| {
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        let mut card = Attachment::new()
            .id(SharedString::from(path.to_string_lossy().into_owned()))
            .tooltip(name.clone());
        if image {
            let preview = path.to_owned();
            let owner = cx.weak_entity();
            card = card
                .axis(Axis::Vertical)
                .w(rems(7.))
                .h(rems(5.))
                .media(
                    AttachmentMedia::new()
                        .aspect_ratio(7. / 5.)
                        .child(
                            img(path.to_owned())
                                .size_full()
                                .object_fit(ObjectFit::Contain),
                        ),
                )
                .on_click(move |_, window, cx| {
                    if owner
                        .read_with(cx, |view, _| view.paths.contains(&preview))
                        .unwrap_or(false)
                    {
                        crate::image_preview::open_path(preview.clone(), window, cx);
                    }
                });
        } else {
            let preview = path.to_owned();
            let owner = cx.weak_entity();
            card = card
                .small()
                .media(AttachmentMedia::new().child(Icon::new(IconName::File)))
                .content(AttachmentContent::new().title(AttachmentTitle::new(name.clone())))
                .on_click(move |_, window, cx| {
                    if owner
                        .read_with(cx, |view, _| view.paths.contains(&preview))
                        .unwrap_or(false)
                    {
                        crate::file_preview::open_path(name.clone(), preview.clone(), window, cx);
                    }
                });
        }
        if self.pending.is_none() {
            card = card.on_remove(cx.listener(move |view, _, _, cx| {
                cx.stop_propagation();
                view.paths
                    .retain(|path| path != &remove);
                view.prune_draft_attachments();
                cx.emit(WorkspaceEvent::DraftChanged);
                cx.notify();
            }));
        }
        card
    }

    fn composer_target(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let directory = std::path::Path::new(self.location.as_ref())
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("选择目录")
            .to_owned();
        let local = self.options.ssh.is_none() && self.options.wsl.is_none();
        let wsl = self.options.wsl.is_some();
        let owner = cx.weak_entity();
        let directory_owner = cx.weak_entity();
        let current_directory = self.location.to_string();
        const MAX_RECENT_DIRECTORIES: usize = 12;
        let mut seen = std::collections::HashSet::new();
        let directories = std::iter::once(self.location.as_ref())
            .chain(
                self.sessions
                    .iter()
                    .map(|entry| entry.workspace.as_ref()),
            )
            .filter(|path| !path.is_empty() && seen.insert(*path))
            .take(MAX_RECENT_DIRECTORIES)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        h_flex()
            .w_full()
            .gap_1()
            .child(
                Button::new("draft-directory")
                    .ghost()
                    .small()
                    .icon(IconName::Folder)
                    .label(directory)
                    .max_w(rems(18.))
                    .tooltip(format!("选择工作目录：{}", self.location))
                    .disabled(
                        self.opening
                            || self.choosing_project
                            || self.picker_pending
                            || self.attachment_imports != 0
                            || self.pending.is_some(),
                    )
                    .dropdown_caret(true)
                    .dropdown_menu_with_anchor(Anchor::TopLeft, move |mut menu, _, _| {
                        for path in &directories {
                            let owner = directory_owner.clone();
                            let path = path.clone();
                            menu = menu.item(
                                PopupMenuItem::new(path.clone())
                                    .checked(path == current_directory)
                                    .on_click(move |_, window, cx| {
                                        let _ = owner.update(cx, |view, cx| {
                                            view.switch_project(
                                                PathBuf::from(&path),
                                                None,
                                                false,
                                                false,
                                                window,
                                                cx,
                                            );
                                        });
                                    }),
                            );
                        }
                        let owner = directory_owner.clone();
                        menu.separator()
                            .item(PopupMenuItem::new("选择其他目录…").on_click(
                                move |_, window, cx| {
                                    let _ =
                                        owner.update(cx, |view, cx| view.open_project(window, cx));
                                },
                            ))
                    }),
            )
            .child(
                Button::new("draft-platform")
                    .ghost()
                    .small()
                    .icon(if local {
                        IconName::Laptop
                    } else {
                        IconName::Monitor
                    })
                    .label(self.environment())
                    .max_w(rems(18.))
                    .tooltip(self.environment())
                    .dropdown_caret(true)
                    .disabled(
                        self.opening
                            || self.choosing_project
                            || self.picker_pending
                            || self.attachment_imports != 0
                            || self.pending.is_some(),
                    )
                    .dropdown_menu_with_anchor(Anchor::TopLeft, move |mut menu, _, _| {
                        for (kind, label) in [(0, "本地"), (1, "SSH…"), (2, "WSL…")] {
                            let owner = owner.clone();
                            menu = menu.item(
                                PopupMenuItem::new(label)
                                    .checked(match kind {
                                        0 => local,
                                        1 => !local && !wsl,
                                        _ => wsl,
                                    })
                                    .on_click(move |_, window, cx| {
                                        let _ = owner.update(cx, |view, cx| {
                                            if kind == 0 {
                                                if !local {
                                                    view.choose_project_directory(None, cx);
                                                }
                                            } else {
                                                view.choose_platform(kind == 2, window, cx);
                                            }
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
    }

    fn composer_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let controls_disabled =
            !self.connected || self.opening || self.busy || self.pending.is_some();
        let dictating = self.dictation.read(cx).is_active(cx);
        let show_stop = !dictating
            && self.busy
            && !draft_ready(&self.input.read(cx).value(), &self.paths, self.busy);
        let mode = self
            .info
            .as_ref()
            .map(|info| info.mode)
            .unwrap_or_default();
        let mode_owner = cx.weak_entity();
        let mode_session = self
            .info
            .as_ref()
            .map(|info| info.id.clone())
            .unwrap_or_default();
        let work_owner = cx.weak_entity();
        let planning = self.planning.mode == orbit_protocol::plan::WorkMode::Plan;
        h_flex()
            .gap_1()
            .flex_wrap()
            .when(!dictating, |row| {
                row.child(
                    Button::new("attach")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .accessibility_label("添加附件")
                        .tooltip("添加附件")
                        .disabled(
                            self.pending.is_some()
                                || self.opening
                                || self.picker_pending
                                || self.attachment_imports != 0,
                        )
                        .on_click(cx.listener(|view, _, _, cx| view.attach(cx))),
                )
                .child(
                    Button::new("work-mode")
                        .ghost()
                        .small()
                        .compact()
                        .icon(IconName::SquareTerminal)
                        .label(
                            if self.planning.mode == orbit_protocol::plan::WorkMode::Plan {
                                "计划"
                            } else {
                                "执行"
                            },
                        )
                        .disabled(controls_disabled)
                        .dropdown_caret(true)
                        .tooltip("工作模式：执行任务或先制定计划")
                        .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                            menu = menu.check_side(gpui_kit::component::Side::Right);
                            for (is_plan, label) in [(false, "执行"), (true, "计划")] {
                                let owner = work_owner.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(label)
                                        .checked(planning == is_plan)
                                        .on_click(move |_, _, cx| {
                                            let _ = owner.update(cx, |view, cx| {
                                                view.plan_action(
                                                    if is_plan {
                                                        orbit_protocol::plan::Action::Enter
                                                    } else {
                                                        orbit_protocol::plan::Action::Leave
                                                    },
                                                    cx,
                                                )
                                            });
                                        }),
                                );
                            }
                            menu
                        }),
                )
                .child(
                    Button::new("mode")
                        .ghost()
                        .small()
                        .compact()
                        .dropdown_caret(true)
                        .tooltip("工具操作的审批方式")
                        .label(match mode {
                            Mode::Ask => "每次询问",
                            Mode::Auto => "自动审批",
                            Mode::Yolo => "全部允许",
                        })
                        .disabled(!self.connected || self.opening || self.pending.is_some())
                        .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                            menu = menu.check_side(gpui_kit::component::Side::Right);
                            for (choice, label) in [
                                (Mode::Ask, "每次询问"),
                                (Mode::Auto, "自动审批"),
                                (Mode::Yolo, "全部允许"),
                            ] {
                                let owner = mode_owner.clone();
                                let session = mode_session.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(label)
                                        .checked(mode == choice)
                                        .on_click(move |_, _, cx| {
                                            let _ = owner.update(cx, |view, cx| {
                                                view.command(
                                                    Command::Request(Request::SetMode {
                                                        session: session.clone(),
                                                        mode: choice,
                                                    }),
                                                    cx,
                                                )
                                            });
                                        }),
                                );
                            }
                            menu
                        }),
                )
                .child(div().flex_1())
                .when_some(
                    self.info
                        .as_ref()
                        .filter(|info| !info.reasoning_efforts.is_empty()),
                    |row, info| {
                        let owner = cx.weak_entity();
                        let session = info.id.clone();
                        let model = info.model.clone();
                        let efforts = info.reasoning_efforts.clone();
                        row.child(
                            Button::new("reasoning-effort")
                                .ghost()
                                .small()
                                .dropdown_caret(true)
                                .label(
                                    info.reasoning_effort
                                        .clone()
                                        .unwrap_or_else(|| "思考强度".into()),
                                )
                                .disabled(controls_disabled)
                                .dropdown_menu_with_anchor(
                                    Anchor::BottomLeft,
                                    move |mut menu, _, _| {
                                        for effort in &efforts {
                                            let owner = owner.clone();
                                            let session = session.clone();
                                            let model = model.clone();
                                            let effort = effort.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(effort.clone()).on_click(
                                                    move |_, _, cx| {
                                                        let _ = owner.update(cx, |view, cx| {
                                                            view.command(
                                                                Command::Request(
                                                                    Request::SetModel {
                                                                        session: session.clone(),
                                                                        model: model.clone(),
                                                                        effort: Some(
                                                                            effort.clone(),
                                                                        ),
                                                                    },
                                                                ),
                                                                cx,
                                                            )
                                                        });
                                                    },
                                                ),
                                            );
                                        }
                                        menu
                                    },
                                ),
                        )
                    },
                )
                .child(self.composer_model_button(controls_disabled, cx))
            })
            .child(self.dictation.clone())
            .child(
                Button::new("composer-action")
                    .small()
                    .when(!show_stop, |button| button.primary())
                    .icon(Icon::new(if show_stop {
                        IconName::Square
                    } else {
                        IconName::ArrowUp
                    }))
                    .accessibility_label(if show_stop { "停止" } else { "发送" })
                    .tooltip(if show_stop {
                        crate::keymap::tooltip("停止", "stop_task", cx)
                    } else {
                        crate::keymap::tooltip(
                            if self.busy {
                                "本次结束后发送"
                            } else {
                                "发送"
                            },
                            "composer_send",
                            cx,
                        )
                    })
                    .disabled(!show_stop && !self.can_send(cx))
                    .on_click(cx.listener(move |view, _, window, cx| {
                        if show_stop {
                            view.stop_task(cx);
                        } else {
                            view.send(&SendMessage, window, cx);
                        }
                    })),
            )
    }

    fn composer_model_button(&self, disabled: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let model_names = self
            .info
            .as_ref()
            .map(|info| info.models.clone())
            .unwrap_or_default();
        let model_groups = super::models::groups(&model_names);
        let current_model = self.model.clone();
        let weak = cx.weak_entity();
        let session = self
            .info
            .as_ref()
            .map(|info| info.id.clone())
            .unwrap_or_default();
        Button::new("model")
            .ghost()
            .small()
            .dropdown_caret(true)
            .label(
                self.model
                    .rsplit("::")
                    .next()
                    .unwrap_or(&self.model)
                    .to_owned(),
            )
            .disabled(disabled)
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, window, cx| {
                for (provider, models) in &model_groups {
                    let models = models.clone();
                    let weak = weak.clone();
                    let session = session.clone();
                    let current = current_model.clone();
                    let active = models
                        .iter()
                        .any(|model| model == current.as_ref());
                    menu = menu.submenu_with_icon(
                        active.then(|| Icon::new(IconName::Check)),
                        format!("{provider} · {}", models.len()),
                        window,
                        cx,
                        move |mut menu, _, _| {
                            for model in &models {
                                let model = model.clone();
                                let weak = weak.clone();
                                let session = session.clone();
                                let label = model
                                    .split_once("::")
                                    .map_or(model.as_str(), |(_, name)| name)
                                    .to_owned();
                                menu = menu.item(
                                    PopupMenuItem::new(label)
                                        .checked(model == current.as_ref())
                                        .on_click(move |_, _, cx| {
                                            let _ = weak.update(cx, |view, cx| {
                                                view.command(
                                                    Command::Request(Request::SetModel {
                                                        session: session.clone(),
                                                        model: model.clone(),
                                                        effort: None,
                                                    }),
                                                    cx,
                                                )
                                            });
                                        }),
                                );
                            }
                            menu
                        },
                    );
                }
                menu
            })
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use super::actions::WorkspaceCommand;
        let empty = !self.opening && self.conversation.read(cx).is_empty();
        let body =
            v_flex()
                .size_full()
                .min_w_0()
                .min_h_0()
                .child(div().flex_1().min_h_0().child(
                    if self.opening && !self.recovery.is_active() {
                        v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .child(gpui_kit::component::spinner::Spinner::new())
                            .child(
                                div()
                                    .text_sm()
                                    .child(self.connection_stage.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(self.environment()),
                            )
                            .child(
                                Button::new("show-connection-logs")
                                    .small()
                                    .ghost()
                                    .label("连接日志")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.open_connection_logs(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("cancel-connection")
                                    .small()
                                    .label("取消连接")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.cancel_connection(window, cx)
                                    })),
                            )
                            .into_any_element()
                    } else if self.info.is_none() && self.error.is_some() {
                        self.connection_failure(cx)
                            .into_any_element()
                    } else if self.configuration_required() {
                        v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_xl()
                                    .child("添加模型连接，开始使用 Orbit"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .child("先添加账号或 API 连接，再选择模型开始任务。"),
                            )
                            .child(
                                Button::new("setup-model-connection")
                                    .label("添加模型连接…")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.execute(WorkspaceCommand::Accounts, window, cx)
                                    })),
                            )
                            .into_any_element()
                    } else if empty {
                        v_flex()
                            .size_full()
                            .items_center()
                            .justify_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(if self.info.is_some() {
                                        "开始新任务"
                                    } else {
                                        "打开项目，开始工作"
                                    }),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if self.info.is_some() {
                                        "描述任务，或继续左侧的会话"
                                    } else {
                                        "选择工作目录，或从左侧继续已有会话"
                                    }),
                            )
                            .when(self.info.is_none(), |view| {
                                view.child(
                                    Button::new("home-open-project")
                                        .label("打开项目…")
                                        .disabled(self.choosing_project)
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.open_project(window, cx)
                                        })),
                                )
                            })
                            .into_any_element()
                    } else {
                        self.conversation
                            .clone()
                            .into_any_element()
                    },
                ))
                .when(
                    !self.approvals.is_empty()
                        || self
                            .question_form
                            .as_ref()
                            .is_some_and(|form| form.read(cx).hidden())
                        || self
                            .planning
                            .plan
                            .as_ref()
                            .is_some_and(|p| p.status == orbit_protocol::plan::Status::Review),
                    |view| {
                        view.child(
                            h_flex()
                                .px(rems(CONTENT_GUTTER_REM))
                                .py_2()
                                .gap_2()
                                .flex_wrap()
                                .children(self.approvals.iter().map(|approval| {
                                    let approval = approval.clone();
                                    Button::new(("approval", approval.id))
                                        .small()
                                        .label("审批操作…")
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            view.approval(&approval, window, cx)
                                        }))
                                }))
                                .when(
                                    self.question_form
                                        .as_ref()
                                        .is_some_and(|form| form.read(cx).hidden()),
                                    |view| {
                                        view.child(
                                            Button::new("answer-agent")
                                                .small()
                                                .label("回答 Agent…")
                                                .on_click(cx.listener(|view, _, window, cx| {
                                                    view.questions(window, cx)
                                                })),
                                        )
                                    },
                                )
                                .when(
                                    self.planning
                                        .plan
                                        .as_ref()
                                        .is_some_and(|p| {
                                            p.status == orbit_protocol::plan::Status::Review
                                        }),
                                    |view| {
                                        view.child(
                                            Button::new("review-plan")
                                                .small()
                                                .label("审阅计划…")
                                                .on_click(cx.listener(|view, _, window, cx| {
                                                    view.show_plan(window, cx)
                                                })),
                                        )
                                    },
                                ),
                        )
                    },
                )
                .when(
                    self.info.is_some() && (!self.opening || self.recovery.is_active()),
                    |view| {
                        view.child(
                            v_flex()
                                .relative()
                                .w_full()
                                .flex_shrink_0()
                                .items_center()
                                .px(rems(CONTENT_GUTTER_REM))
                                .pt_2()
                                .pb_3()
                                .child(
                                    div()
                                        .w_full()
                                        .when(Preferences::comfortable_width(cx), |view| {
                                            view.max_w(rems(CONTENT_WIDTH_REM))
                                        })
                                        .child(self.composer(window, cx)),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .right_0()
                                        .bottom(rems(3.))
                                        .child(
                                            self.conversation
                                                .read(cx)
                                                .navigation_controls(),
                                        ),
                                ),
                        )
                    },
                );
        let accessory = self
            .panel
            .as_ref()
            .map(|panel| panel.clone().into_any_element())
            .or_else(|| {
                self.inspector.map(|_| {
                    self.inspector_view(cx)
                        .into_any_element()
                })
            });
        let body = if let Some(accessory) = accessory {
            if window.viewport_size().width < window.rem_size() * 72. {
                div()
                    .size_full()
                    .child(accessory)
                    .into_any_element()
            } else {
                h_resizable("document-panes")
                    .child(resizable_panel().child(body))
                    .child(
                        resizable_panel()
                            .size(window.rem_size() * 22.)
                            .size_range(window.rem_size() * 18. ..window.rem_size() * 48.)
                            .child(accessory),
                    )
                    .into_any_element()
            }
        } else {
            body.into_any_element()
        };
        let body = if self.page == MainPage::Tasks {
            v_flex()
                .size_full()
                .min_h_0()
                .child(
                    h_flex()
                        .h_10()
                        .px_3()
                        .gap_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            Button::new("tasks-back")
                                .ghost()
                                .small()
                                .icon(IconName::ArrowLeft)
                                .accessibility_label("返回会话")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.focus_composer(window, cx);
                                })),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child("项目任务"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .text_ellipsis()
                                .text_color(cx.theme().muted_foreground)
                                .child(self.location.clone()),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .children(self.tasks.clone()),
                )
                .into_any_element()
        } else {
            body
        };
        let body = match self.page {
            MainPage::Files => div()
                .size_full()
                .min_h_0()
                .children(self.files.clone())
                .into_any_element(),
            MainPage::Review => div()
                .size_full()
                .min_h_0()
                .children(self.review.clone())
                .into_any_element(),
            _ => body,
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .key_context("Orbit")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::send))
            .on_action(cx.listener(|view, _: &NextTask, _, cx| view.adjacent_task(true, cx)))
            .on_action(cx.listener(|view, _: &PreviousTask, _, cx| view.adjacent_task(false, cx)))
            .on_action(cx.listener(|view, _: &ToggleInspector, window, cx| {
                view.toggle_inspector(window, cx)
            }))
            .on_action(cx.listener(|view, _: &NewConversation, _, cx| view.open(None, cx)))
            .on_action(cx.listener(Self::appearance))
            .on_action(
                cx.listener(|view, _: &OpenProject, window, cx| view.open_project(window, cx)),
            )
            .on_action(
                cx.listener(|view, _: &WorkspaceCommands, window, cx| view.commands(window, cx)),
            )
            .on_action(cx.listener(|view, _: &ShowChanges, window, cx| {
                view.execute(WorkspaceCommand::Changes, window, cx)
            }))
            .on_action(
                cx.listener(|view, _: &SearchSessions, window, cx| {
                    view.search_sessions(window, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &ToggleSidebar, _, cx| {
                view.sidebar
                    .update(cx, |sidebar, cx| sidebar.toggle(cx));
            }))
            .on_action(
                cx.listener(|view, _: &FocusComposer, window, cx| view.focus_composer(window, cx)),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}
