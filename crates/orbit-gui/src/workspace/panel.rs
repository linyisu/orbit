//! A persistent document pane for reports. Refreshes update its content without
//! moving focus or reopening a modal over the conversation.
use super::*;
use gpui_kit::component::{scroll::ScrollableElement, text::TextViewState};

pub(super) struct Panel {
    title: SharedString,
    text: Entity<TextViewState>,
    source: String,
    save: Task<()>,
    owner: WeakEntity<Workspace>,
}
impl Panel {
    pub(super) fn title(&self) -> &str {
        &self.title
    }
    pub(super) fn set(&mut self, text: String, cx: &mut Context<Self>) {
        self.text = cx.new(|cx| TextViewState::markdown(&text, cx));
        self.source = text;
        cx.notify();
    }
    pub(super) fn new(
        title: String,
        text: String,
        owner: WeakEntity<Workspace>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            title: title.into(),
            text: cx.new(|cx| TextViewState::markdown(&text, cx)),
            source: text,
            save: Task::ready(()),
            owner,
        }
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        let directory = std::env::current_dir().unwrap_or_default();
        let name = if self.title == "Git patch" {
            "changes.patch"
        } else {
            "orbit-report.md"
        };
        let prompt = cx.prompt_for_new_path(&directory, Some(name));
        let text = self.source.clone();
        let owner = self.owner.clone();
        self.save = cx.spawn(async move |_, cx| {
            let result = match prompt.await {
                Ok(Ok(Some(path))) => {
                    cx.background_executor()
                        .spawn(async move {
                            std::fs::write(path, text).map_err(|error| error.to_string())
                        })
                        .await
                }
                Ok(Ok(None)) => return,
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            if let Err(error) = result {
                let _ = owner.update(cx, |view, cx| {
                    view.report_error(format!("保存失败：{error}"), cx);
                    cx.notify();
                });
            }
        });
    }
}
impl Render for Panel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = self.owner.clone();
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .h_9()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_ellipsis()
                            .child(self.title.clone()),
                    )
                    .child(
                        Button::new("copy-document")
                            .ghost()
                            .small()
                            .label("复制")
                            .on_click(cx.listener(|view, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    view.source.clone(),
                                ))
                            })),
                    )
                    .child(
                        Button::new("save-document")
                            .ghost()
                            .small()
                            .label("保存")
                            .on_click(cx.listener(|view, _, _, cx| view.save(cx))),
                    )
                    .child(
                        Button::new("close-document")
                            .ghost()
                            .small()
                            .label("关闭")
                            .accessibility_label("关闭面板")
                            .tooltip("关闭面板")
                            .on_click(move |_, _, cx| {
                                let _ =
                                    owner.update(cx, |view, cx| {
                                        if view.panel.as_ref().is_some_and(|panel| {
                                            panel.read(cx).title() == "旁路提问"
                                        }) && let Some(info) = &view.info
                                        {
                                            view.command(
                                                Command::Request(Request::CloseQuestions {
                                                    session: info.id.clone(),
                                                }),
                                                cx,
                                            );
                                        }
                                        view.panel = None;
                                        cx.notify();
                                    });
                            }),
                    ),
            )
            .child(
                div().flex_1().min_h_0().overflow_y_scrollbar().child(
                    div()
                        .p_4()
                        .child(TextView::new(&self.text).selectable(true)),
                ),
            )
    }
}
