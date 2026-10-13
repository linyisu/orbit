//! Source discovery and installation stay on the connected workspace service.
use super::*;
use gpui_kit::component::{StyledExt, checkbox::Checkbox, spinner::Spinner};
use orbit_protocol::resources::SkillCandidate;
use std::collections::BTreeSet;

pub(super) enum Event {
    Imported(Vec<Skill>, usize),
    Closed,
}

struct Scan {
    token: String,
    source: String,
    skills: Vec<SkillCandidate>,
    selected: BTreeSet<String>,
}

pub(super) struct Importer {
    owner: WeakEntity<Workspace>,
    session: String,
    workspace: String,
    source: Entity<InputState>,
    scan: Option<Scan>,
    busy: bool,
    installing: bool,
    error: Option<String>,
    task: Task<()>,
    _source_events: Subscription,
}

impl EventEmitter<Event> for Importer {}

impl Importer {
    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        session: String,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let source = cx.new(|cx| {
            InputState::new(window, cx).placeholder("GitHub 仓库、Git URL 或工作区主机上的目录")
        });
        source.update(cx, |input, cx| input.focus(window, cx));
        let events = cx.subscribe(&source, |view: &mut Self, _, event, cx| {
            match event {
                InputEvent::Change if !view.busy => {
                    // A token belongs to exactly the source the user scanned.
                    view.scan = None;
                    view.error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } if !view.busy => view.scan_source(cx),
                _ => {}
            }
        });
        Self {
            owner,
            session,
            workspace,
            source,
            scan: None,
            busy: false,
            installing: false,
            error: None,
            task: Task::ready(()),
            _source_events: events,
        }
    }

    fn scan_source(&mut self, cx: &mut Context<Self>) {
        let source = self.source.read(cx).value().trim().to_owned();
        if source.is_empty() || self.busy {
            return;
        }
        self.scan = None;
        self.request(Action::ScanSkills { source }, cx);
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        let Some(scan) = &self.scan else { return };
        if scan.selected.is_empty() || self.busy {
            return;
        }
        self.request(
            Action::ImportSkills {
                token: scan.token.clone(),
                paths: scan.selected.iter().cloned().collect(),
            },
            cx,
        );
    }

    fn request(&mut self, action: Action, cx: &mut Context<Self>) {
        let count = match &action {
            Action::ImportSkills { paths, .. } => paths.len(),
            _ => 0,
        };
        let (result, receiver) = tokio::sync::oneshot::channel();
        let session = self.session.clone();
        let workspace = self.workspace.clone();
        let sent = self.owner.update(cx, |view, _| {
            anyhow::ensure!(view.connected, "连接已断开，请重新连接后重试。");
            anyhow::ensure!(
                view.info
                    .as_ref()
                    .is_some_and(|info| info.id == session && info.workspace == workspace),
                "工作区已切换，请在当前项目的设置中重新打开技能。"
            );
            view.backend
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("请先连接工作区"))?
                .send(Command::Settings {
                    request: Request::Resources {
                        session: session.clone(),
                        expected_workspace: Some(workspace.clone()),
                        action,
                    },
                    result,
                })
        });
        if let Err(error) = sent
            .map_err(|e| e.to_string())
            .and_then(|result| result.map_err(|e| e.to_string()))
        {
            self.error = Some(error);
            cx.notify();
            return;
        }
        self.busy = true;
        self.installing = count > 0;
        self.error = None;
        self.task = cx.spawn(async move |entity, cx| {
            let result = receiver
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = entity.update(cx, |view, cx| {
                view.busy = false;
                view.installing = false;
                let current = view.owner.upgrade().is_some_and(|owner| {
                    let owner = owner.read(cx);
                    owner.connected
                        && owner
                            .info
                            .as_ref()
                            .is_some_and(|info| info.id == session && info.workspace == workspace)
                });
                if !current {
                    view.scan = None;
                    view.error =
                        Some("连接或工作区已改变。请返回原项目刷新技能列表，核对结果。".into());
                    cx.notify();
                    return;
                }
                match result {
                    Ok(Response::Resources(Snapshot::SkillSource {
                        token,
                        source,
                        skills,
                    })) if count == 0 => {
                        view.scan = Some(Scan {
                            token,
                            source,
                            skills,
                            selected: BTreeSet::new(),
                        });
                    }
                    Ok(Response::Resources(Snapshot::Skills(skills))) if count > 0 => {
                        cx.emit(Event::Imported(skills, count));
                    }
                    result => {
                        view.error = Some(match result {
                            Err(error) => error,
                            Ok(_) => "服务未返回技能导入结果，请更新工作区服务后重试。".into(),
                        });
                        // Re-scan after a failed/unknown install; never replay a mutation blindly.
                        if count > 0 {
                            view.scan = None;
                        }
                    }
                }
                cx.notify();
            });
        });
        cx.notify();
    }
}

impl Render for Importer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = v_flex().w_full().min_w_0().gap_4()
            .child(h_flex().gap_2()
                .child(Button::new("back-to-skills").small().ghost()
                    .icon(IconName::ArrowLeft).label("返回技能")
                    .disabled(self.busy)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Closed))))
                .child(div().font_semibold().child("从来源导入")))
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("填写来源，扫描其中的技能，再选择安装到当前项目。技能附带的脚本和参考文件会一起导入。"))
            .child(v_flex().gap_2()
                .child(div().text_sm().child("技能来源"))
                .child(h_flex().gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.source)
                        .aria_label("技能来源").disabled(self.busy).cleanable(true)))
                    .child(Button::new("scan-skill-source").label(if self.busy && !self.installing {"扫描中…"} else {"扫描来源"})
                        .disabled(self.busy || self.source.read(cx).value().trim().is_empty())
                        .on_click(cx.listener(|view, _, _, cx| view.scan_source(cx)))))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child("支持 owner/repo、Git URL、GitHub 子目录链接和本地技能目录。远程项目中的路径由远程主机读取。")))
            .when(self.busy, |view| view.child(h_flex().gap_2()
                .child(Spinner::new().small())
                .child(div().text_sm().child(if self.installing {"正在安装所选技能…"} else {"正在获取来源并扫描 SKILL.md…"}))))
            .when_some(self.error.clone(), |view, error| view.child(div().text_sm().text_color(cx.theme().danger).child(error)));

        if let Some(scan) = &self.scan {
            let available = scan.skills.iter().filter(|skill| !skill.conflict).count();
            let selected = scan.selected.len();
            content = content.child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .font_semibold()
                                    .child(format!("发现 {} 个技能", scan.skills.len())),
                            )
                            .child(
                                Button::new("select-importable-skills")
                                    .small()
                                    .ghost()
                                    .label(if selected == available && available > 0 {
                                        "取消全选"
                                    } else {
                                        "全选可导入项"
                                    })
                                    .disabled(self.busy || available == 0)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        if let Some(scan) = &mut view.scan {
                                            let paths: BTreeSet<_> = scan
                                                .skills
                                                .iter()
                                                .filter(|skill| !skill.conflict)
                                                .map(|skill| skill.path.clone())
                                                .collect();
                                            scan.selected = if scan.selected == paths {
                                                BTreeSet::new()
                                            } else {
                                                paths
                                            };
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_ellipsis()
                            .text_color(cx.theme().muted_foreground)
                            .child(scan.source.clone()),
                    ),
            );
            if scan.skills.is_empty() {
                content = content.child(
                    div()
                        .py_4()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("没有找到可导入的 SKILL.md。请检查目录或换一个来源。"),
                );
            }
            for skill in &scan.skills {
                let path = skill.path.clone();
                content = content.child(
                    h_flex()
                        .items_start()
                        .gap_3()
                        .py_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            Checkbox::new(SharedString::from(format!("import-skill:{path}")))
                                .flex_1()
                                .min_w_0()
                                .checked(scan.selected.contains(&path))
                                .disabled(self.busy || skill.conflict)
                                .label(skill.title.clone())
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(skill.description.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_ellipsis()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("/{} · {}", skill.name, skill.path)),
                                )
                                .on_click(cx.listener(move |view, checked, _, cx| {
                                    if let Some(scan) = &mut view.scan {
                                        if *checked {
                                            scan.selected.insert(path.clone());
                                        } else {
                                            scan.selected.remove(&path);
                                        }
                                    }
                                    cx.notify();
                                })),
                        )
                        .when(skill.conflict, |row| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(cx.theme().warning)
                                    .child("名称冲突"),
                            )
                        }),
                );
            }
            content = content.child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .py_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("已选 {selected} 项 · 安装到项目 .orbit/skills")),
                    )
                    .child(
                        Button::new("install-selected-skills")
                            .primary()
                            .label("安装所选技能")
                            .disabled(self.busy || selected == 0)
                            .on_click(cx.listener(|view, _, _, cx| view.install(cx))),
                    ),
            );
        }
        content
    }
}
