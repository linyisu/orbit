//! A retained project backlog. IDs stay in row actions and never need typing.
use super::*;
use gpui_kit::component::{
    IndexPath,
    checkbox::Checkbox,
    input::{Input, InputState, NumberInput, NumberInputEvent, StepAction},
    list::{List, ListDelegate, ListItem, ListState},
    menu::{DropdownMenu, PopupMenuItem},
    spinner::Spinner,
};
use orbit_protocol::project::{ProjectTask, TaskAction, TaskSnapshot, TaskStatus};
use std::collections::BTreeSet;

pub(super) struct Tasks {
    owner: WeakEntity<Workspace>,
    session: String,
    workspace: String,
    tasks: Vec<ProjectTask>,
    selected: Option<i64>,
    query: Entity<InputState>,
    title: Entity<InputState>,
    details: Entity<TextareaState>,
    priority: Entity<InputState>,
    dependencies: BTreeSet<i64>,
    show_completed: bool,
    editing: bool,
    creating: bool,
    busy: bool,
    loaded: bool,
    saving: bool,
    error: Option<String>,
    saved: bool,
    width: Option<Pixels>,
    list: Entity<ListState<TaskList>>,
    _subscriptions: Vec<Subscription>,
}

impl Tasks {
    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        session: String,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("搜索项目任务…"));
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("任务标题"));
        let priority = cx.new(|cx| InputState::new(window, cx).default_value("0"));
        let query_events = cx.subscribe_in(&query, window, |view, _, event, window, cx| {
            if matches!(event, InputEvent::Change) {
                view.sync_list(window, cx);
                cx.notify();
            }
        });
        let priority_events = cx.subscribe_in(
            &priority,
            window,
            |_, input, event: &NumberInputEvent, window, cx| {
                let NumberInputEvent::Step(step) = event;
                let current = input.read(cx).value().parse::<i64>().unwrap_or(0);
                let next = match step {
                    StepAction::Increment => current.saturating_add(1),
                    StepAction::Decrement => current.saturating_sub(1),
                }
                .clamp(-100, 100);
                input.update(cx, |input, cx| {
                    input.set_value(next.to_string(), window, cx)
                });
            },
        );
        let entity = cx.weak_entity();
        let list = cx.new(|cx| ListState::new(TaskList::new(entity.clone()), window, cx));
        cx.defer(move |cx| {
            let _ = entity.update(cx, |view, cx| view.refresh(cx));
        });
        Self {
            owner,
            session,
            workspace,
            tasks: vec![],
            selected: None,
            query,
            title,
            details: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(4, 10)
                    .placeholder("任务目标、实现说明与验收要求")
            }),
            priority,
            dependencies: BTreeSet::new(),
            show_completed: false,
            editing: false,
            creating: false,
            busy: false,
            loaded: false,
            saving: false,
            error: None,
            saved: false,
            width: None,
            list,
            _subscriptions: vec![query_events, priority_events],
        }
    }

    fn request(&mut self, action: TaskAction, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let session = self.session.clone();
        let workspace = self.workspace.clone();
        let sent = self.owner.update(cx, |view, _| {
            view.backend
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("请先连接工作区"))?
                .send(Command::Tasks {
                    session,
                    workspace,
                    action,
                })
        });
        match sent
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| e.to_string()))
        {
            Ok(()) => {
                self.busy = true;
                self.error = None;
                self.saved = false;
            }
            Err(error) => {
                self.error = Some(error);
                self.saving = false;
            }
        }
        self.list.update(cx, |list, cx| {
            list.set_selectable(!self.busy && !self.editing, cx)
        });
        cx.notify();
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.request(
            TaskAction::List {
                include_completed: true,
            },
            cx,
        );
    }

    pub(super) fn activate(&mut self, cx: &mut Context<Self>) {
        if !self.editing {
            self.refresh(cx);
        }
    }

    pub(super) fn bind_session(
        &mut self,
        session: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session == session {
            return;
        }
        self.disconnected(window, cx);
        // A reconnect can replace an unpersisted conversation while retaining
        // the same project. Keep its form, but reject the previous session's reply.
        self.session = session;
        cx.notify();
    }

    pub(super) fn disconnected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.busy {
            return;
        }
        self.error = Some(if self.saving {
            "连接已中断，任务修改的结果尚未确认。编辑内容已保留；重新连接后请先刷新，检查是否需要重新保存。".into()
        } else {
            "连接已中断，请重新连接后刷新项目任务。".into()
        });
        self.busy = false;
        self.saving = false;
        self.saved = false;
        self.sync_list(window, cx);
        cx.notify();
    }

    pub(super) fn receive(
        &mut self,
        session: &str,
        result: Result<TaskSnapshot, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if session != self.session || !self.busy {
            return;
        }
        self.busy = false;
        let created = self.saving && self.creating && result.is_ok();
        match result {
            Ok(snapshot) => {
                if self.saving && self.creating {
                    self.selected = snapshot.changed;
                }
                self.tasks = snapshot.tasks;
                self.loaded = true;
                self.error = None;
                if self.saving {
                    self.saved = true;
                    self.editing = false;
                    self.creating = false;
                }
                if !self.editing && !self.tasks.iter().any(|task| Some(task.id) == self.selected) {
                    self.selected = self
                        .tasks
                        .iter()
                        .find(|task| self.show_completed || !closed(task.status))
                        .map(|task| task.id);
                }
            }
            Err(error) => self.error = Some(error),
        }
        self.saving = false;
        self.sync_list(window, cx);
        if created {
            self.list
                .update(cx, |list, cx| list.scroll_to_selected_item(window, cx));
        }
        cx.notify();
    }

    pub(super) fn start_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing {
            self.title.update(cx, |input, cx| input.focus(window, cx));
            return;
        }
        self.selected = None;
        self.editing = true;
        self.creating = true;
        self.dependencies.clear();
        self.error = None;
        self.saved = false;
        self.title.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.details
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.priority
            .update(cx, |input, cx| input.set_value("0", window, cx));
        self.sync_list(window, cx);
        cx.notify();
    }

    fn edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self
            .tasks
            .iter()
            .find(|task| Some(task.id) == self.selected)
        else {
            return;
        };
        self.title.update(cx, |input, cx| {
            input.set_value(task.title.clone(), window, cx)
        });
        self.details.update(cx, |input, cx| {
            input.set_value(task.details.clone(), window, cx)
        });
        self.priority.update(cx, |input, cx| {
            input.set_value(task.priority.to_string(), window, cx)
        });
        self.dependencies = task.depends_on.iter().copied().collect();
        self.editing = true;
        self.creating = false;
        self.error = None;
        self.saved = false;
        self.title.update(cx, |input, cx| input.focus(window, cx));
        self.sync_list(window, cx);
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let title = self.title.read(cx).value().trim().to_string();
        let details = self.details.read(cx).value().to_string();
        let priority = self.priority.read(cx).value().parse::<i64>();
        let error = if title.is_empty() {
            Some("请填写任务标题")
        } else if title.len() > 512 {
            Some("任务标题不能超过 512 字节")
        } else if details.len() > 8192 {
            Some("任务说明不能超过 8192 字节")
        } else if !priority
            .as_ref()
            .is_ok_and(|value| (-100..=100).contains(value))
        {
            Some("优先级应为 -100～100 的整数")
        } else if self.dependencies.len() > 64 {
            Some("一个任务最多选择 64 个依赖")
        } else {
            None
        };
        if let Some(error) = error {
            self.error = Some(error.into());
            cx.notify();
            return;
        }
        let priority = priority.unwrap();
        let depends_on = self.dependencies.iter().copied().collect();
        let action = if self.creating {
            TaskAction::Create {
                title,
                details,
                priority,
                parent_id: None,
                depends_on,
            }
        } else if let Some(id) = self.selected {
            TaskAction::Update {
                id,
                title: Some(title),
                details: Some(details),
                status: None,
                priority: Some(priority),
                depends_on: Some(depends_on),
            }
        } else {
            return;
        };
        self.saving = true;
        self.request(action, cx);
    }

    fn visible_tasks(&self, cx: &App) -> Vec<usize> {
        let query = self.query.read(cx).value().trim().to_lowercase();
        self.tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| {
                (self.show_completed || !closed(task.status))
                    && (query.is_empty()
                        || task.title.to_lowercase().contains(&query)
                        || task.details.to_lowercase().contains(&query)
                        || task.id.to_string().contains(&query))
            })
            .map(|(ix, _)| ix)
            .collect()
    }

    fn sync_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = self
            .visible_tasks(cx)
            .into_iter()
            .map(|index| self.tasks[index].clone())
            .collect::<Vec<_>>();
        let selected = items
            .iter()
            .position(|task| Some(task.id) == self.selected)
            .map(IndexPath::new);
        self.list.update(cx, |list, cx| {
            list.delegate_mut().items = items;
            list.set_selectable(!self.busy && !self.editing, cx);
            list.set_selected_index(selected, window, cx);
            cx.notify();
        });
    }

    fn editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = v_flex()
            .w_full()
            .gap_4()
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(if self.creating {
                        "新建项目任务"
                    } else {
                        "编辑任务"
                    }),
            )
            .child(
                v_flex().gap_1().child("标题").child(
                    Input::new(&self.title)
                        .aria_label("任务标题")
                        .disabled(self.busy),
                ),
            )
            .child(
                v_flex().gap_1().child("说明").child(
                    Textarea::new(&self.details)
                        .aria_label("任务说明")
                        .disabled(self.busy),
                ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(div().child("优先级"))
                    .child(
                        div()
                            .w(rems(7.))
                            .child(NumberInput::new(&self.priority).disabled(self.busy)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("数值越大越优先，范围 -100～100"),
                    ),
            )
            .child(
                v_flex().gap_1().child("依赖任务").child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("所有依赖完成后，才能开始或完成此任务。"),
                ),
            );
        let dependencies = self
            .tasks
            .iter()
            .filter(|task| self.creating || Some(task.id) != self.selected);
        body = body.child(
            div()
                .id("task-dependencies")
                .max_h(rems(14.))
                .overflow_y_scroll()
                .child(v_flex().gap_2().children(dependencies.map(|task| {
                    let id = task.id;
                    Checkbox::new(SharedString::from(format!("task-dependency-{id}")))
                        .label(format!("{} · {}", task.title, status_label(task.status)))
                        .checked(self.dependencies.contains(&id))
                        .disabled(self.busy)
                        .on_change(cx.listener(move |view, checked: &bool, _, cx| {
                            if *checked {
                                view.dependencies.insert(id);
                            } else {
                                view.dependencies.remove(&id);
                            }
                            cx.notify();
                        }))
                }))),
        );
        if self.tasks.is_empty() {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("没有可选择的依赖任务"),
            );
        }
        body.child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("save-project-task")
                        .primary()
                        .label(if self.busy {
                            "保存中…"
                        } else {
                            "保存任务"
                        })
                        .disabled(self.busy || !self.loaded)
                        .on_click(cx.listener(|view, _, _, cx| view.save(cx))),
                )
                .child(
                    Button::new("cancel-project-task-edit")
                        .label("取消编辑")
                        .disabled(self.busy)
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.editing = false;
                            view.creating = false;
                            view.error = None;
                            view.sync_list(window, cx);
                            cx.notify();
                        })),
                ),
        )
        .into_any_element()
    }

    fn details_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(task) = self
            .tasks
            .iter()
            .find(|task| Some(task.id) == self.selected)
        else {
            return div()
                .p_6()
                .text_color(cx.theme().muted_foreground)
                .child("选择一个任务查看详情，或创建新任务。")
                .into_any_element();
        };
        let id = task.id;
        let owner = cx.weak_entity();
        let current = task.status;
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::MEDIUM)
                    .child(task.title.clone()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("project-task-status")
                            .outline()
                            .label(status_label(current))
                            .disabled(self.busy)
                            .dropdown_menu(move |mut menu, _, _| {
                                for status in TaskStatus::ALL {
                                    let owner = owner.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(status_label(status))
                                            .checked(status == current)
                                            .on_click(move |_, _, cx| {
                                                let _ = owner.update(cx, |view, cx| {
                                                    view.request(
                                                        TaskAction::SetStatus { id, status },
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
                        Button::new("edit-project-task")
                            .small()
                            .label("编辑任务")
                            .disabled(self.busy)
                            .on_click(cx.listener(|view, _, window, cx| view.edit(window, cx))),
                    )
                    .when(task.owner_session_id.is_some(), |row| {
                        row.child(
                            Button::new("release-project-task")
                                .small()
                                .label("释放认领")
                                .disabled(self.busy)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.request(TaskAction::Release { id }, cx)
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("任务 #{} · 优先级 {}", id, task.priority)),
            )
            .when_some(task.owner_session_id.as_ref(), |body, session| {
                body.child(div().text_sm().child(format!("认领会话：{session}")))
            })
            .when(!task.details.is_empty(), |body| {
                body.child(div().whitespace_normal().child(task.details.clone()))
            })
            .when(!task.depends_on.is_empty(), |body| {
                body.child(
                    v_flex()
                        .gap_2()
                        .child("依赖任务")
                        .children(task.depends_on.iter().map(|dependency| {
                            let dependency = *dependency;
                            let label = self
                                .tasks
                                .iter()
                                .find(|task| task.id == dependency)
                                .map(|task| {
                                    format!("{} · {}", task.title, status_label(task.status))
                                })
                                .unwrap_or_else(|| format!("任务 #{dependency}"));
                            Button::new(SharedString::from(format!(
                                "open-task-dependency-{dependency}"
                            )))
                            .ghost()
                            .small()
                            .label(label)
                            .disabled(self.busy)
                            .on_click(cx.listener(
                                move |view, _, window, cx| {
                                    view.selected = Some(dependency);
                                    view.sync_list(window, cx);
                                    cx.notify();
                                },
                            ))
                        })),
                )
            })
            .when(!task.blocked_by.is_empty(), |body| {
                body.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().warning)
                        .child("还有未完成的依赖，暂时不能开始或完成。"),
                )
            })
            .into_any_element()
    }
}

impl Render for Tasks {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.list.read(cx).delegate().items.len();
        let narrow = self
            .width
            .is_none_or(|width| width < window.rem_size() * 60.);
        let measured_width = self.width;
        let owner = cx.weak_entity();
        let list_focus = self.list.focus_handle(cx);
        let list = v_flex()
            .min_h_0()
            .min_w_0()
            .when(narrow, |v| v.w_full().h(rems(14.)))
            .when(!narrow, |v| v.w(rems(21.)).flex_shrink_0())
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .when(count == 0, |list| {
                list.child(
                    v_flex()
                        .p_5()
                        .gap_2()
                        .when(self.busy && !self.loaded, |v| {
                            v.child(Spinner::new().small())
                        })
                        .child(if self.busy && !self.loaded {
                            "正在加载项目任务…"
                        } else if !self.loaded && self.error.is_some() {
                            "未能加载项目任务，请刷新重试。"
                        } else if self.tasks.is_empty() {
                            "项目还没有任务，创建一个开始跟踪。"
                        } else {
                            "没有符合筛选条件的任务。"
                        }),
                )
            })
            .when(count > 0, |list| {
                list.child(
                    div()
                        .size_full()
                        .track_focus(&list_focus)
                        .tab_index(0)
                        .child(List::new(&self.list)),
                )
            });
        let details = div()
            .id("project-task-details")
            .min_h_0()
            .min_w_0()
            .flex_1()
            .overflow_y_scroll()
            .child(div().p_4().child(if self.editing {
                self.editor(cx)
            } else {
                self.details_view(cx)
            }));
        v_flex()
            .id("project-tasks-page")
            .tab_group()
            .relative()
            .size_full()
            .min_h_0()
            .min_w_0()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        div()
                            .flex_1()
                            .min_w(rems(12.))
                            .child(Input::new(&self.query).aria_label("搜索项目任务")),
                    )
                    .child(
                        Checkbox::new("show-completed-project-tasks")
                            .label("显示已结束")
                            .checked(self.show_completed)
                            .on_change(cx.listener(|view, checked: &bool, window, cx| {
                                view.show_completed = *checked;
                                view.sync_list(window, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("refresh-project-tasks")
                            .ghost()
                            .small()
                            .label("刷新")
                            .disabled(self.busy)
                            .on_click(cx.listener(|view, _, _, cx| view.refresh(cx))),
                    )
                    .child(
                        Button::new("create-project-task")
                            .primary()
                            .small()
                            .icon(IconName::Plus)
                            .label("新建任务")
                            .disabled(self.busy || self.editing)
                            .on_click(
                                cx.listener(|view, _, window, cx| view.start_create(window, cx)),
                            ),
                    ),
            )
            .when_some(self.error.clone(), |body, error| {
                body.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .when(self.saved, |body| {
                body.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("任务已保存"),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .gap_4()
                    .items_stretch()
                    .when(narrow, |row| row.flex_col())
                    .child(list)
                    .child(details),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let width = bounds.size.width;
                        if width <= px(0.) || measured_width == Some(width) {
                            return;
                        }
                        let owner = owner.clone();
                        cx.defer(move |cx| {
                            let _ = owner.update(cx, |view, cx| {
                                if view.width != Some(width) {
                                    view.width = Some(width);
                                    cx.notify();
                                }
                            });
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }
}

struct TaskList {
    owner: WeakEntity<Tasks>,
    items: Vec<ProjectTask>,
    selected: Option<IndexPath>,
}

impl TaskList {
    fn new(owner: WeakEntity<Tasks>) -> Self {
        Self {
            owner,
            items: Vec::new(),
            selected: None,
        }
    }
}

impl ListDelegate for TaskList {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.items.len()
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let task = self.items.get(index.row)?;
        let owner = self.owner.upgrade()?;
        let state = owner.read(cx);
        Some(
            ListItem::new(SharedString::from(format!("project-task-{}", task.id)))
                .accessibility_label(format!("{}，{}", task.title, status_label(task.status)))
                .h_16()
                .px_3()
                .confirmed(state.selected == Some(task.id))
                .disabled(state.editing || state.busy)
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_sm().text_ellipsis().child(task.title.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_ellipsis()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "#{} · {}{}",
                                    task.id,
                                    status_label(task.status),
                                    if task.blocked_by.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" · {} 个未完成依赖", task.blocked_by.len())
                                    }
                                )),
                        ),
                ),
        )
    }

    fn set_selected_index(
        &mut self,
        selected: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = selected;
    }

    fn confirm(&mut self, _: bool, _: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(task) = self.selected.and_then(|index| self.items.get(index.row)) else {
            return;
        };
        let id = task.id;
        let _ = self.owner.update(cx, |view, cx| {
            if view.busy || view.editing {
                return;
            }
            view.selected = Some(id);
            view.error = None;
            view.saved = false;
            cx.notify();
        });
    }
}

fn closed(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Completed | TaskStatus::Cancelled)
}
fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "待处理",
        TaskStatus::InProgress => "进行中",
        TaskStatus::Blocked => "受阻",
        TaskStatus::Completed => "已完成",
        TaskStatus::Cancelled => "已取消",
    }
}
