//! Component composition for the workspace and message composer.
use super::*;
use crate::theme::CONTENT_WIDTH_REM;
use orbit_client::protocol::Mode;

impl Workspace {
    fn composer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(form) = &self.question_form
            && !form.read(cx).hidden()
        {
            return form.clone().into_any_element();
        }
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
        let controls_disabled =
            !self.connected || self.opening || self.busy || self.pending.is_some();
        let mode = self.info.as_ref().map(|info| info.mode).unwrap_or_default();
        let mode_owner = cx.weak_entity();
        let mode_session = session.clone();
        let paste_owner = cx.weak_entity();
        let work_owner = cx.weak_entity();
        let planning = self.planning.mode == orbit_protocol::plan::WorkMode::Plan;
        v_flex()
            .id("composer")
            .relative()
            .when(self.command_open, |view| view.key_context("ComposerCompletions")
                .on_action(cx.listener(|view, _: &CompletionNext, window, cx| view.move_completion(1, window, cx)))
                .on_action(cx.listener(|view, _: &CompletionPrevious, window, cx| view.move_completion(-1, window, cx)))
                .on_action(cx.listener(|view, _: &CompletionSubmit, window, cx| view.submit_completion(window, cx)))
                .on_action(cx.listener(|view, _: &CompletionConfirm, window, cx| view.confirm_completion(window, cx)))
                .on_action(cx.listener(|view, _: &CompletionDismiss, _, cx| view.dismiss_completions(cx)))
                .child(deferred(self.completion_menu(window, cx)).with_priority(1))
            )
            .on_drop(
                cx.listener(|view, paths: &ExternalPaths, _, cx| view.add_images(&paths.0, cx)),
            )
            .w_full()
            .when(
                crate::preferences::Preferences::comfortable_width(cx),
                |view| view.max_w(rems(CONTENT_WIDTH_REM)),
            )
            .gap_2()
            .child(
                v_flex()
                    .w_full()
                    .p_3()
                    .gap_2()
                    .bg(cx.theme().group_box)
                    .border_1()
                    .border_color(cx.theme().border)
                    .when(
                        self.input.read(cx).focus_handle(cx).is_focused(window),
                        |view| view.border_color(cx.theme().ring),
                    )
                    .rounded(cx.theme().radius_lg)
                    .when(!self.paths.is_empty(), |view| {
                        view.child(h_flex().gap_2().flex_wrap().children(self.paths.iter().map(
                            |path| {
                                let remove = path.clone();
                                let preview = path.clone();
                                let label: SharedString = path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned()
                                    .into();
                                Attachment::new()
                                    .id(SharedString::from(path.to_string_lossy().into_owned()))
                                    .axis(Axis::Vertical)
                                    .w(rems(7.))
                                    .h(rems(5.))
                                    .tooltip(label.clone())
                                    .media(
                                        AttachmentMedia::new().aspect_ratio(7. / 5.).child(
                                            img(path.clone())
                                                .size_full()
                                                .object_fit(ObjectFit::Contain),
                                        ),
                                    )
                                    .when(self.pending.is_none(), |attachment| {
                                        attachment.on_remove(cx.listener(move |view, _, _, cx| {
                                            view.paths.retain(|path| path != &remove);
                                            cx.emit(WorkspaceEvent::DraftChanged);
                                            cx.notify();
                                        }))
                                    })
                                    .on_click(move |_, window, cx| {
                                        let preview = preview.clone();
                                        let label = label.clone();
                                        window.open_dialog(cx, move |dialog, _, _| {
                                            dialog.title(label.clone()).child(
                                                div().h_64().w_full().child(
                                                    img(preview.clone())
                                                        .size_full()
                                                        .object_fit(ObjectFit::Contain),
                                                ),
                                            )
                                        });
                                    })
                            },
                        )))
                    })
                    .child(
                        Textarea::new(&self.input)
                            .on_paste(move |item, _, cx| {
                                paste_owner
                                    .update(cx, |view, cx| view.paste_images(item, cx))
                                    .unwrap_or(false)
                            })
                            .appearance(false)
                            .bordered(false)
                            .aria_label("消息输入框"),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .flex_wrap()
                            .child(
                                Button::new("work-mode")
                                    .ghost()
                                    .small()
                                    .icon(IconName::SquareTerminal)
                                    .label(
                                        if self.planning.mode
                                            == orbit_protocol::plan::WorkMode::Plan
                                        {
                                            "计划"
                                        } else {
                                            "Agent"
                                        },
                                    )
                                    .disabled(controls_disabled)
                                    .child(Icon::new(IconName::ChevronDown).size_3())
                                    .dropdown_menu(move |mut menu, _, _| {
                                        for (is_plan, label) in [(false, "Agent · 直接执行"), (true, "计划 · 先审阅再执行")] {
                                            let owner = work_owner.clone();
                                            menu = menu.item(PopupMenuItem::new(label).checked(planning == is_plan)
                                                .on_click(move |_, _, cx| {
                                                    let _ = owner.update(cx, |view, cx| view.plan_action(
                                                        if is_plan { orbit_protocol::plan::Action::Enter } else { orbit_protocol::plan::Action::Leave }, cx));
                                                }));
                                        }
                                        menu
                                    }),
                            )
                            .child(
                                Button::new("attach")
                                    .ghost()
                                    .small()
                                    .icon(Icon::new(IconName::Plus))
                                    .accessibility_label("添加图片")
                                    .tooltip("添加图片")
                                    .disabled(
                                        self.pending.is_some()
                                            || self.opening
                                            || !self
                                                .info
                                                .as_ref()
                                                .is_some_and(|info| info.supports_vision),
                                    )
                                    .on_click(cx.listener(|view, _, _, cx| view.attach(cx))),
                            )
                            .child(
                                Button::new("mode")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Shield)
                                    .child(Icon::new(IconName::ChevronDown).size_3())
                                    .label(match mode {
                                        Mode::Ask => "每次询问",
                                        Mode::Auto => "自动审批",
                                        Mode::Yolo => "全部允许",
                                    })
                                    .disabled(controls_disabled)
                                    .dropdown_menu(move |mut menu, _, _| {
                                        for (mode, label) in [
                                            (Mode::Ask, "每次询问"),
                                            (Mode::Auto, "自动审批"),
                                            (Mode::Yolo, "全部允许"),
                                        ] {
                                            let owner = mode_owner.clone();
                                            let session = mode_session.clone();
                                            menu = menu.item(PopupMenuItem::new(label).on_click(
                                                move |_, _, cx| {
                                                    let _ = owner.update(cx, |view, cx| {
                                                        view.command(
                                                            Command::Request(Request::SetMode {
                                                                session: session.clone(),
                                                                mode,
                                                            }),
                                                            cx,
                                                        )
                                                    });
                                                },
                                            ));
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
                                            .child(Icon::new(IconName::ChevronDown).size_3())
                                            .label(
                                                info.reasoning_effort
                                                    .clone()
                                                    .unwrap_or_else(|| "思考强度".into()),
                                            )
                                            .disabled(controls_disabled)
                                            .dropdown_menu(move |mut menu, _, _| {
                                                for effort in &efforts {
                                                    let owner = owner.clone();
                                                    let session = session.clone();
                                                    let model = model.clone();
                                                    let effort = effort.clone();
                                                    menu = menu.item(
                                                        PopupMenuItem::new(effort.clone())
                                                            .on_click(move |_, _, cx| {
                                                                let _ =
                                                                    owner.update(cx, |view, cx| {
                                                                        view.command(
                                                                    Command::Request(
                                                                        Request::SetModel {
                                                                            session: session
                                                                                .clone(),
                                                                            model: model.clone(),
                                                                            effort: Some(
                                                                                effort.clone(),
                                                                            ),
                                                                        },
                                                                    ),
                                                                    cx,
                                                                )
                                                                    });
                                                            }),
                                                    );
                                                }
                                                menu
                                            }),
                                    )
                                },
                            )
                            .child(
                                Button::new("model")
                                    .ghost()
                                    .small()
                                    .child(Icon::new(IconName::ChevronDown).size_3())
                                    .label(
                                        self.model
                                            .rsplit("::")
                                            .next()
                                            .unwrap_or(&self.model)
                                            .to_owned(),
                                    )
                                    .disabled(controls_disabled)
                                    .dropdown_menu(move |mut menu, window, cx| {
                                        for (provider, models) in &model_groups {
                                            let models = models.clone();
                                            let weak = weak.clone();
                                            let session = session.clone();
                                            let current = current_model.clone();
                                            let active = models.iter().any(|model| model == current.as_ref());
                                            menu = menu.submenu_with_icon(
                                                active.then(|| Icon::new(IconName::Check)),
                                                format!("{provider} · {}", models.len()), window, cx,
                                                move |mut menu, _, _| {
                                                    for model in &models {
                                                        let model = model.clone();
                                                        let weak = weak.clone();
                                                        let session = session.clone();
                                                        let label = model.split_once("::").map_or(model.as_str(), |(_, name)| name).to_owned();
                                                        menu = menu.item(PopupMenuItem::new(label).checked(model == current.as_ref())
                                                            .on_click(move |_, _, cx| {
                                                                let _ = weak.update(cx, |view, cx| view.command(Command::Request(Request::SetModel {
                                                                    session: session.clone(), model: model.clone(), effort: None,
                                                                }), cx));
                                                            }));
                                                    }
                                                    menu
                                                });
                                        }
                                        menu
                                    }),
                            )
                            .when(self.busy, |view| {
                                view.child(
                                    Button::new("stop")
                                        .small()
                                        .icon(Icon::new(IconName::Square))
                                        .accessibility_label("停止")
                                        .tooltip("停止")
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            if let Some(info) = &view.info {
                                                view.command(
                                                    Command::Request(Request::Cancel {
                                                        session: info.id.clone(),
                                                    }),
                                                    cx,
                                                );
                                            }
                                        })),
                                )
                            })
                            .when(
                                !self.busy || !self.input.read(cx).value().trim().is_empty(),
                                |view| {
                                    view.child(
                                        Button::new("send")
                                            .primary()
                                            .small()
                                            .icon(Icon::new(IconName::ArrowUp))
                                            .accessibility_label("发送")
                                            .tooltip(if self.busy {
                                                "补充指令 · Enter"
                                            } else {
                                                "发送 · Enter"
                                            })
                                            .disabled(!self.can_send(cx))
                                            .on_click(cx.listener(|view, _, window, cx| {
                                                view.send(&SendMessage, window, cx)
                                            })),
                                    )
                                },
                            ),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("composer-context")
                            .ghost()
                            .xsmall()
                            .label(format!(
                                "上下文 {}%",
                                self.info
                                    .as_ref()
                                    .map_or(0, |info| self.context_used.saturating_mul(100)
                                        / info.context_tokens.max(1))
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
            ).into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(settings) = &self.settings_page {
            return settings.clone().into_any_element();
        }
        use super::actions::WorkspaceCommand;
        use gpui_kit::component::TitleBar;
        let title_bar = TitleBar::new().child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(
                    Button::new("workspace-menu")
                        .ghost()
                        .small()
                        .icon(IconName::Menu)
                        .accessibility_label("工作区命令")
                        .tooltip("工作区命令 · Ctrl+Shift+P")
                        .on_click(cx.listener(|view, _, window, cx| view.commands(window, cx))),
                )
                .child(div().text_sm().child(self.project.clone()))
                .when_some(
                    self.info.as_ref().and_then(|info| info.branch.clone()),
                    |view, branch| {
                        view.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("/ {branch}")),
                        )
                    },
                ),
        );
        let sidebar = v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                v_flex()
                    .p_3()
                    .gap_1()
                    .child(
                        Button::new("new-session")
                            .ghost()
                            .small()
                            .w_full()
                            .accessibility_label("新任务")
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::Plus).size_4())
                                    .child("新任务"),
                            )
                            .tooltip("Ctrl+N")
                            .disabled(self.opening || !self.connected)
                            .on_click(cx.listener(|view, _, _, cx| view.open(None, cx))),
                    )
                    .child(
                        Button::new("open-project")
                            .ghost()
                            .small()
                            .w_full()
                            .accessibility_label("打开项目")
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::Folder).size_4())
                                    .child("打开项目…"),
                            )
                            .tooltip("Ctrl+O")
                            .disabled(self.choosing_project || self.pending.is_some())
                            .on_click(cx.listener(|view, _, _, cx| view.open_project(cx))),
                    )
                    .child(
                        Button::new("find-task")
                            .ghost()
                            .small()
                            .w_full()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::Search).size_4())
                                    .child("搜索任务"),
                            )
                            .tooltip("搜索任务 · Ctrl+P")
                            .on_click(
                                cx.listener(|view, _, window, cx| view.search_sessions(window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .px_4()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("项目"),
            )
            .child(div().flex_1().min_h_0().child(self.project_tree(cx)))
            .child(
                h_flex()
                    .p_3()
                    .gap_1()
                    .child(
                        Button::new("accounts")
                            .ghost()
                            .small()
                            .label("账号…")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.execute(WorkspaceCommand::Accounts, window, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("appearance")
                            .ghost()
                            .small()
                            .icon(IconName::Settings)
                            .label("设置")
                            .accessibility_label("设置")
                            .tooltip("设置 · Ctrl+,")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.appearance(&AppearanceSettings, window, cx)
                            })),
                    ),
            );
        let empty = !self.opening && self.conversation.read(cx).is_empty();
        let body = v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
                h_flex()
                    .h_9()
                    .px_2()
                    .gap_2()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("toggle-sidebar")
                            .ghost()
                            .small()
                            .icon(IconName::PanelLeft)
                            .accessibility_label("切换侧栏")
                            .tooltip("切换侧栏 · Ctrl+B")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.sidebar_visible = !view.sidebar_visible;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1().min_w_0().child(self.task_tabs(cx)))
                    .when(self.error.is_some() && self.info.is_none(), |bar| {
                        bar.child(
                            Button::new("project-error")
                                .ghost()
                                .small()
                                .icon(IconName::CircleAlert)
                                .label("查看错误")
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.show_error(window, cx)),
                                ),
                        )
                    })
                    .when(self.open_tasks.len() > 1, |bar| {
                        bar.child(
                            Button::new("close-task")
                                .ghost()
                                .small()
                                .icon(IconName::Close)
                                .accessibility_label("关闭当前任务视图")
                                .tooltip("关闭当前任务视图")
                                .disabled(self.busy || self.pending.is_some())
                                .on_click(cx.listener(|view, _, _, cx| {
                                    cx.emit(WorkspaceEvent::Close(view.key.clone()))
                                })),
                        )
                    })
                    .child(
                        Button::new("toggle-inspector")
                            .ghost()
                            .small()
                            .icon(IconName::PanelRight)
                            .label("工作面板")
                            .accessibility_label("工作面板")
                            .tooltip("执行、计划、变更与 Agent")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.inspector = if view.inspector.is_some() {
                                    None
                                } else {
                                    Some(super::inspector::Section::Execution)
                                };
                                cx.notify();
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().child(if self.opening {
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(gpui_kit::component::spinner::Spinner::new())
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("正在恢复会话…"),
                    )
                    .into_any_element()
            } else if empty {
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(div().text_xl().font_weight(FontWeight::MEDIUM).child(
                        if self.info.is_some() {
                            "开始新任务"
                        } else {
                            "打开项目，开始工作"
                        },
                    ))
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
                                .on_click(cx.listener(|view, _, _, cx| view.open_project(cx))),
                        )
                    })
                    .when(self.info.is_some(), |view| {
                        view.child(
                            div()
                                .w_full()
                                .when(
                                    crate::preferences::Preferences::comfortable_width(cx),
                                    |view| view.max_w(rems(CONTENT_WIDTH_REM)),
                                )
                                .px_6()
                                .pt_3()
                                .child(self.composer(window, cx)),
                        )
                    })
                    .into_any_element()
            } else {
                self.conversation.clone().into_any_element()
            }))
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
                            .px_4()
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
                                self.planning.plan.as_ref().is_some_and(|p| {
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
            .when(self.info.is_some() && !empty && !self.opening, |view| {
                view.child(
                    v_flex()
                        .w_full()
                        .items_center()
                        .px_4()
                        .pt_2()
                        .pb_3()
                        .when(self.busy, |view| {
                            view.child(
                                h_flex()
                                    .w_full()
                                    .when(
                                        crate::preferences::Preferences::comfortable_width(cx),
                                        |view| view.max_w(rems(CONTENT_WIDTH_REM)),
                                    )
                                    .pb_2()
                                    .child(
                                        Button::new("run-progress")
                                            .ghost()
                                            .xsmall()
                                            .label(format!(
                                                "{}  ·  {} 次工具调用  ·  {} 个文件",
                                                self.status(),
                                                self.execution.tools().len(),
                                                self.execution.files().len()
                                            ))
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.inspector =
                                                    Some(super::inspector::Section::Execution);
                                                cx.notify();
                                            })),
                                    ),
                            )
                        })
                        .child(self.composer(window, cx)),
                )
            });
        let accessory = self
            .panel
            .as_ref()
            .map(|panel| panel.clone().into_any_element())
            .or_else(|| {
                self.inspector
                    .map(|_| self.inspector_view(cx).into_any_element())
            });
        let body = if let Some(accessory) = accessory {
            if window.viewport_size().width < window.rem_size() * 72. {
                div().size_full().child(accessory).into_any_element()
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
            .on_action(cx.listener(|view, _: &ToggleInspector, _, cx| {
                view.inspector = if view.inspector.is_some() {
                    None
                } else {
                    Some(super::inspector::Section::Execution)
                };
                cx.notify();
            }))
            .on_action(cx.listener(|view, _: &NewConversation, _, cx| view.open(None, cx)))
            .on_action(cx.listener(Self::appearance))
            .on_action(cx.listener(|view, _: &OpenProject, _, cx| view.open_project(cx)))
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
                view.sidebar_visible = !view.sidebar_visible;
                cx.notify();
            }))
            .on_action(cx.listener(|view, _: &FocusComposer, window, cx| {
                view.input.update(cx, |input, cx| input.focus(window, cx))
            }))
            .child(title_bar)
            .child(div().flex_1().min_h_0().child(if self.sidebar_visible {
                h_resizable("workspace-panes")
                    .on_resize(|state, window, cx| {
                        if let Some(width) = state.read(cx).sizes().first().copied() {
                            crate::preferences::Preferences::set_sidebar(
                                width / window.rem_size(),
                                cx,
                            );
                        }
                    })
                    .child(
                        resizable_panel()
                            .size(window.rem_size() * crate::preferences::Preferences::sidebar(cx))
                            .size_range(window.rem_size() * 12. ..window.rem_size() * 24.)
                            .child(sidebar),
                    )
                    .child(resizable_panel().child(body))
                    .into_any_element()
            } else {
                body
            }))
            .into_any_element()
    }
}
