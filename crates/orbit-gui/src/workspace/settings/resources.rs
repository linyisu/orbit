//! Retained editors; settings requests report errors locally and never open report panels.
use super::super::*;
use gpui_kit::component::input::{Input, InputState};
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
    kind: Kind,
    skills: Vec<Skill>,
    memories: Vec<SessionMemory>,
    name: Entity<InputState>,
    body: Entity<TextareaState>,
    editing: bool,
    original: Option<String>,
    readonly: bool,
    busy: bool,
    error: Option<String>,
    task: Task<()>,
}
impl Resources {
    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        session: Option<String>,
        kind: Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let entity = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |view, cx| view.refresh(cx));
        });
        Self {
            owner,
            session,
            kind,
            skills: vec![],
            memories: vec![],
            name: cx.new(|cx| InputState::new(window, cx).placeholder("例如 code-review")),
            body: cx.new(|cx| TextareaState::new(window, cx).auto_grow(8, 18)),
            editing: false,
            original: None,
            readonly: false,
            busy: false,
            error: None,
            task: Task::ready(()),
        }
    }
    fn request(&mut self, request: Request, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (result, receiver) = tokio::sync::oneshot::channel();
        let sent = self.owner.update(cx, |view, _| {
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
        self.task = cx.spawn(async move |owner, cx| {
            let result = receiver.await.map_err(|e| e.to_string()).and_then(|r| r);
            let _ = owner.update_in(cx, |view, window, cx| {
                view.busy = false;
                match result {
                    Ok(Response::Resources(Snapshot::Skills(skills))) => {
                        view.skills = skills;
                        view.editing = false;
                    }
                    Ok(Response::Resources(Snapshot::Memories(memories))) => {
                        view.memories = memories;
                        view.editing = false;
                    }
                    Ok(Response::Resources(Snapshot::Skill {
                        name,
                        content,
                        editable,
                    })) => {
                        view.name
                            .update(cx, |input, cx| input.set_value(name, window, cx));
                        view.body
                            .update(cx, |input, cx| input.set_value(content.clone(), window, cx));
                        view.original = Some(content);
                        view.editing = true;
                        view.readonly = !editable;
                    }

                    Ok(_) => {
                        view.error = Some("服务没有返回设置数据，请更新工作区服务后重试".into())
                    }
                    Err(error) => view.error = Some(error),
                }
                cx.notify();
            });
        });
        cx.notify();
    }
    fn action(&mut self, action: Action, cx: &mut Context<Self>) {
        if let Some(session) = self.session.clone() {
            self.request(Request::Resources { session, action }, cx);
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
    fn new_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = true;
        self.original = None;
        self.readonly = false;
        self.error = None;
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
                    category: MemoryCategory::Fact,
                    content,
                }),
                cx,
            ),
        }
    }
}
impl Render for Resources {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (description, add, empty) = match self.kind {
            Kind::Skills => (
                "管理当前工作区可用的技能。新技能保存到项目，保存后自动刷新。",
                "新建技能",
                "还没有技能，创建一个来复用 Agent 的工作流程。",
            ),
            Kind::Memory => (
                "保存当前会话的事实与偏好，供后续对话使用。",
                "添加记忆",
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
            });
        if self.editing {
            let mut form = v_flex().gap_3();
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
            h_flex().child(
                Button::new("resource-add")
                    .icon(IconName::Plus)
                    .label(add)
                    .disabled(self.busy || self.session.is_none())
                    .on_click(cx.listener(|v, _, w, cx| v.new_item(w, cx))),
            ),
        );
        let mut rows = Vec::new();
        match self.kind {
            Kind::Skills => {
                for skill in &self.skills {
                    let name = skill.name.clone();
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
                                Button::new(SharedString::from(format!("skill-{}", skill.name)))
                                    .small()
                                    .label(if skill.editable { "编辑" } else { "查看" })
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
                    let id = memory.id;
                    rows.push(
                        h_flex()
                            .gap_4()
                            .py_3()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(div().flex_1().child(memory.content.clone()))
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
                    .child(if self.busy { "正在加载…" } else { empty }),
            );
        }
        content.children(rows)
    }
}
