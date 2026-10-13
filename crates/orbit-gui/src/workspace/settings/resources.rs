//! Retained editors; settings requests report errors locally and never open report panels.
mod import;
use super::super::*;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::radio::{Radio, RadioGroup};
use orbit_protocol::{
    project::{MemoryAction, MemoryCategory, SessionMemory},
    resources::{Action, Skill, Snapshot},
};

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Skills,
    Memory,
}
pub(super) struct Resources {
    owner: WeakEntity<Workspace>,
    session: Option<String>,
    workspace: String,
    kind: Kind,
    skills: Vec<Skill>,
    memories: Vec<SessionMemory>,
    query: Entity<InputState>,
    category: MemoryCategory,
    name: Entity<InputState>,
    body: Entity<TextareaState>,
    editing: bool,
    original: Option<String>,
    readonly: bool,
    busy: bool,
    error: Option<String>,
    status: Option<String>,
    task: Task<()>,
    importer: Option<Entity<import::Importer>>,
    import_events: Option<Subscription>,
    _query_events: Subscription,
}
impl Resources {
    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        session: Option<String>,
        workspace: String,
        kind: Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let entity = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |view, cx| view.refresh(cx));
        });
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("搜索名称或内容…"));
        let query_events = cx.subscribe(&query, |_: &mut Self, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            owner,
            session,
            workspace,
            kind,
            skills: vec![],
            memories: vec![],
            query,
            category: MemoryCategory::Fact,
            name: cx.new(|cx| InputState::new(window, cx).placeholder("例如 code-review")),
            body: cx.new(|cx| TextareaState::new(window, cx).auto_grow(8, 18)),
            editing: false,
            original: None,
            readonly: false,
            busy: false,
            error: None,
            status: None,
            task: Task::ready(()),
            importer: None,
            import_events: None,
            _query_events: query_events,
        }
    }
    fn request(&mut self, request: Request, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (result, receiver) = tokio::sync::oneshot::channel();
        let session = self.session.clone();
        let workspace = self.workspace.clone();
        let loading_skill = match &request {
            Request::SessionAction {
                action: orbit_protocol::SessionAction::LoadSkill { name },
                ..
            } => Some(name.clone()),
            _ => None,
        };
        self.status = None;
        let sent = self.owner.update(cx, |view, _| {
            if !view.info.as_ref().is_some_and(|info| {
                Some(&info.id) == session.as_ref() && info.workspace == workspace
            }) {
                anyhow::bail!("工作区已切换，请在当前项目的设置中重新打开资源。");
            }
            anyhow::ensure!(view.connected, "连接已断开，请重新连接后重试。");
            view.backend
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("请先连接工作区"))?
                .send(Command::Settings { request, result })
        });
        if let Err(error) = sent
            .map_err(|e| e.to_string())
            .and_then(|v| v.map_err(|e| e.to_string()))
        {
            self.error = Some(error);
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        self.status = loading_skill
            .as_ref()
            .map(|name| format!("正在将 /{name} 载入当前会话…"));
        self.task = cx.spawn(async move |owner, cx| {
            let result = receiver.await.map_err(|e| e.to_string()).and_then(|r| r);
            let _ = owner.update_in(cx, |view, window, cx| {
                view.busy = false;
                view.status = None;
                if loading_skill.is_some()
                    && !view.owner.upgrade().is_some_and(|owner| {
                        let owner = owner.read(cx);
                        owner.connected
                            && owner.info.as_ref().is_some_and(|info| {
                                Some(&info.id) == session.as_ref() && info.workspace == workspace
                            })
                    })
                {
                    view.error = Some(
                        "连接或目标会话已改变，加载结果尚未确认。请返回原会话核对；不会自动重发。"
                            .into(),
                    );
                    cx.notify();
                    return;
                }
                match (loading_skill, result) {
                    (Some(_), Ok(Response::Report(report))) => {
                        view.status = Some(report.text);
                    }
                    (None, Ok(Response::Resources(Snapshot::Skills(skills)))) => {
                        view.skills = skills;
                        view.editing = false;
                    }
                    (None, Ok(Response::Resources(Snapshot::Memories(memories)))) => {
                        view.memories = memories;
                        view.editing = false;
                    }
                    (
                        None,
                        Ok(Response::Resources(Snapshot::Skill {
                            name,
                            content,
                            editable,
                        })),
                    ) => {
                        view.name
                            .update(cx, |input, cx| input.set_value(name, window, cx));
                        view.body
                            .update(cx, |input, cx| input.set_value(content.clone(), window, cx));
                        view.original = Some(content);
                        view.editing = true;
                        view.readonly = !editable;
                    }

                    (_, Ok(_)) => {
                        view.error = Some("服务没有返回设置数据，请更新工作区服务后重试".into())
                    }
                    (_, Err(error)) => view.error = Some(error),
                }
                cx.notify();
            });
        });
        cx.notify();
    }
    fn action(&mut self, action: Action, cx: &mut Context<Self>) {
        if let Some(session) = self.session.clone() {
            self.request(
                Request::Resources {
                    session,
                    expected_workspace: Some(self.workspace.clone()),
                    action,
                },
                cx,
            );
        } else {
            self.error = Some("请先打开工作区".into());
            cx.notify();
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        match self.kind {
            Kind::Skills => self.action(Action::Skills, cx),
            Kind::Memory => self.action(Action::Memories, cx),
        }
    }
    fn load_skill(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.error = Some("请先打开工作区会话。".into());
            cx.notify();
            return;
        };
        self.request(
            Request::SessionAction {
                session,
                action: orbit_protocol::SessionAction::LoadSkill { name },
            },
            cx,
        );
    }
    fn new_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = true;
        self.original = None;
        self.readonly = false;
        self.error = None;
        self.status = None;
        self.name
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.body.update(cx,|input,cx|input.set_value(if matches!(self.kind,Kind::Skills) { "---\nname: 新技能\ndescription: 描述这个技能的用途\n---\n\n在这里编写 Agent 执行步骤。\n" } else { "" },window,cx));
        match self.kind {
            Kind::Memory => self.body.update(cx, |input, cx| input.focus(window, cx)),
            _ => self.name.update(cx, |input, cx| input.focus(window, cx)),
        };
        cx.notify();
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let content = self.body.read(cx).value().to_string();
        match self.kind {
            Kind::Skills => self.action(
                Action::SaveSkill {
                    name,
                    original: self.original.clone(),
                    content,
                },
                cx,
            ),
            Kind::Memory => self.action(
                Action::Memory(MemoryAction::Add {
                    category: self.category,
                    content,
                }),
                cx,
            ),
        }
    }

    fn import_skills(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let importer = cx.new(|cx| {
            import::Importer::new(
                self.owner.clone(),
                session,
                self.workspace.clone(),
                window,
                cx,
            )
        });
        self.import_events = Some(cx.subscribe_in(
            &importer,
            window,
            |view, _, event, window, cx| {
                if let import::Event::Imported(skills, count) = event {
                    view.skills = skills.clone();
                    view.status = Some(format!("已导入 {count} 个技能，可从下方加载到当前会话。"));
                }
                view.importer = None;
                view.import_events = None;
                view.query.update(cx, |input, cx| input.focus(window, cx));
                cx.notify();
            },
        ));
        self.error = None;
        self.status = None;
        self.importer = Some(importer);
        cx.notify();
    }
}
impl Render for Resources {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(importer) = &self.importer {
            return v_flex().w_full().min_w_0().child(importer.clone());
        }
        let (description, add, empty) = match self.kind {
            Kind::Skills => (
                "管理当前工作区可用的技能。新技能保存到项目，保存后自动刷新。",
                "新建技能…",
                "还没有技能。可以从仓库或目录导入，也可以新建技能。",
            ),
            Kind::Memory => (
                "保存当前会话的事实与偏好，供后续对话使用。",
                "添加记忆…",
                "还没有保存的记忆。开始会话后，可在这里添加。",
            ),
        };
        let mut content = v_flex()
            .w_full()
            .max_w(rems(60.))
            .gap_4()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(description),
                    )
                    .child(
                        Button::new("resource-refresh")
                            .ghost()
                            .small()
                            .label("刷新")
                            .disabled(self.busy || self.editing)
                            .on_click(cx.listener(|v, _, _, cx| v.refresh(cx))),
                    ),
            )
            .when_some(self.error.clone(), |v, e| {
                v.child(div().text_sm().text_color(cx.theme().danger).child(e))
            })
            .when_some(self.status.clone(), |view, status| {
                view.child(div().text_sm().whitespace_normal().child(status))
            });
        if self.editing {
            let mut form = v_flex().gap_3();
            if matches!(self.kind, Kind::Memory) {
                form = form.child("记忆类型").child(
                    RadioGroup::horizontal("memory-category")
                        .selected_index(
                            MemoryCategory::ALL
                                .iter()
                                .position(|category| *category == self.category),
                        )
                        .disabled(self.busy)
                        .children(MemoryCategory::ALL.into_iter().map(|category| {
                            Radio::new(category.as_str()).label(category_label(category))
                        }))
                        .on_change(cx.listener(|view, ix, _, cx| {
                            view.category = MemoryCategory::ALL[*ix];
                            cx.notify();
                        })),
                );
            }
            if !matches!(self.kind, Kind::Memory) {
                form = form.child("技能名称").child(
                    Input::new(&self.name)
                        .aria_label("技能名称")
                        .disabled(self.busy || self.original.is_some()),
                );
            }
            {
                form = form
                    .child(if matches!(self.kind, Kind::Skills) {
                        "技能说明（Markdown）"
                    } else {
                        "记忆内容"
                    })
                    .child(
                        Textarea::new(&self.body)
                            .aria_label(if matches!(self.kind, Kind::Skills) {
                                "技能说明"
                            } else {
                                "记忆内容"
                            })
                            .readonly(self.readonly)
                            .disabled(self.busy),
                    );
            }
            if self.readonly {
                form = form.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("导入的技能为只读；可复制内容并新建项目技能。"),
                );
            }
            return content.child(
                form.child(
                    h_flex()
                        .gap_2()
                        .when(!self.readonly, |v| {
                            v.child(
                                Button::new("resource-save")
                                    .primary()
                                    .label(if self.busy { "保存中…" } else { "保存" })
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|v, _, _, cx| v.save(cx))),
                            )
                        })
                        .child(
                            Button::new("resource-cancel")
                                .label("返回列表")
                                .disabled(self.busy)
                                .on_click(cx.listener(|v, _, _, cx| {
                                    v.editing = false;
                                    v.error = None;
                                    cx.notify();
                                })),
                        ),
                ),
            );
        }
        content = content.child(
            h_flex()
                .gap_3()
                .flex_wrap()
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(&self.query)
                            .aria_label(match self.kind {
                                Kind::Skills => "搜索技能",
                                Kind::Memory => "搜索会话记忆",
                            })
                            .prefix(Icon::new(IconName::Search))
                            .cleanable(true),
                    ),
                )
                .child(
                    Button::new("resource-add")
                        .icon(IconName::Plus)
                        .label(add)
                        .disabled(self.busy || self.session.is_none())
                        .on_click(cx.listener(|v, _, w, cx| v.new_item(w, cx))),
                )
                .when(matches!(self.kind, Kind::Skills), |row| {
                    row.child(
                        Button::new("resource-import")
                            .primary()
                            .label("从来源导入…")
                            .disabled(self.busy || self.session.is_none())
                            .on_click(
                                cx.listener(|view, _, window, cx| view.import_skills(window, cx)),
                            ),
                    )
                }),
        );
        let query = self.query.read(cx).value().trim().to_lowercase();
        let mut rows = Vec::new();
        match self.kind {
            Kind::Skills => {
                for skill in &self.skills {
                    if !query.is_empty()
                        && ![&skill.name, &skill.title, &skill.description]
                            .into_iter()
                            .any(|text| text.to_lowercase().contains(&query))
                    {
                        continue;
                    }
                    let name = skill.name.clone();
                    let load_name = name.clone();
                    rows.push(
                        h_flex()
                            .gap_4()
                            .flex_wrap()
                            .py_3()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(skill.title.clone())
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(skill.description.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{} · /{}", skill.source, skill.name)),
                                    ),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "load-skill-{}",
                                    skill.name
                                )))
                                .small()
                                .outline()
                                .label("加载到当前会话")
                                .accessibility_label(format!(
                                    "将技能 /{} 加载到当前会话",
                                    skill.name
                                ))
                                .tooltip(format!(
                                    "会话：{} · 工作区：{}",
                                    self.session.as_deref().unwrap_or("尚未连接"),
                                    self.workspace
                                ))
                                .disabled(self.busy || self.session.is_none())
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        view.load_skill(load_name.clone(), cx);
                                    },
                                )),
                            )
                            .child(
                                Button::new(SharedString::from(format!("skill-{}", skill.name)))
                                    .small()
                                    .label(if skill.editable {
                                        "编辑…"
                                    } else {
                                        "查看…"
                                    })
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        v.action(Action::ReadSkill { name: name.clone() }, cx)
                                    })),
                            )
                            .into_any_element(),
                    );
                }
            }
            Kind::Memory => {
                for memory in &self.memories {
                    if !query.is_empty()
                        && ![
                            memory.title.as_str(),
                            memory.content.as_str(),
                            category_label(memory.category),
                        ]
                        .into_iter()
                        .any(|text| text.to_lowercase().contains(&query))
                    {
                        continue;
                    }
                    let id = memory.id;
                    rows.push(
                        h_flex()
                            .gap_4()
                            .py_3()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(category_label(memory.category)),
                                    )
                                    .child(memory.content.clone()),
                            )
                            .child(
                                Button::new(SharedString::from(format!("memory-{id}")))
                                    .small()
                                    .label("移除")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |v, _, _, cx| {
                                        v.action(Action::Memory(MemoryAction::Forget { id }), cx)
                                    })),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        if rows.is_empty() {
            content = content.child(
                div()
                    .py_8()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.busy {
                        "正在加载…"
                    } else if !query.is_empty() {
                        "没有匹配的结果，试试其他关键词。"
                    } else {
                        empty
                    }),
            );
        }
        content.children(rows)
    }
}

fn category_label(category: MemoryCategory) -> &'static str {
    match category {
        MemoryCategory::Constraint => "约束",
        MemoryCategory::Decision => "决策",
        MemoryCategory::Fact => "事实",
        MemoryCategory::Preference => "偏好",
    }
}
