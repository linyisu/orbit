#[cfg(target_os = "android")]
use gpui_kit::component::input::InputEvent;

use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable, StyledExt,
        button::{Button, ButtonVariants},
        input::{Textarea, TextareaState},
        text::TextView,
    },
    prelude::*,
    *,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Sessions,
    Conversation,
    Tool,
    Approval,
}

#[derive(Clone, Copy)]
enum Decision {
    Allow,
    Reject,
}

pub struct Probe {
    page: Page,
    input: Entity<TextareaState>,
    submitted: Option<SharedString>,
    decision: Option<Decision>,
    scroll: ScrollHandle,
    _input_events: Subscription,
}

impl Probe {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input =
            cx.new(|cx| TextareaState::new(window, cx).placeholder("输入中文、表情和多行文字"));
        let events = cx.subscribe_in(&input, window, |_, _, event, _, cx| {
            #[cfg(target_os = "android")]
            match event {
                InputEvent::Focus => gpui_mobile::show_keyboard(),
                InputEvent::Blur => gpui_mobile::hide_keyboard(),
                _ => {}
            }
            let _ = event;
            cx.notify();
        });
        Self {
            page: Page::Sessions,
            input,
            submitted: None,
            decision: None,
            scroll: ScrollHandle::new(),
            _input_events: events,
        }
    }

    fn navigate(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        window.blur(cx);
        self.page = page;
        self.scroll = ScrollHandle::new();
        cx.notify();
    }

    fn record_input(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.submitted = Some(self.input.read(cx).value());
        cx.notify();
    }
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match self.page {
            Page::Sessions => "Orbit · Android 验证",
            Page::Conversation => "示例会话",
            Page::Tool => "工具输出",
            Page::Approval => "审批交互验证",
        };
        let body = match self.page {
            Page::Sessions => div()
                .v_flex()
                .gap_4()
                .child("此工程只验证手机界面，不连接电脑。")
                .child(
                    Button::new("open-conversation")
                        .label("打开示例会话")
                        .large()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.navigate(Page::Conversation, window, cx);
                        })),
                ),
            Page::Conversation => div()
                .v_flex()
                .gap_4()
                .child(TextView::markdown("sample-message", include_str!("sample.md")))
                .child(
                    Button::new("tool")
                        .label("查看工具输出")
                        .large()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.navigate(Page::Tool, window, cx);
                        })),
                )
                .child(
                    Button::new("approval")
                        .label("测试审批交互")
                        .large()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.navigate(Page::Approval, window, cx);
                        })),
                )
                .when_some(self.submitted.clone(), |view, text| {
                    view.child(div().child("输入结果（仅内存）").child(text))
                }),
            Page::Tool => div().child(TextView::markdown(
                "tool-output",
                "```text\nfixture-check\n\n12 checks passed\n0 files changed\n```\n\n这是合成输出，没有执行命令。",
            )),
            Page::Approval => div()
                .v_flex()
                .gap_4()
                .child("示例操作：读取合成项目中的说明文件")
                .child("点击只更新此验证页面，不授予电脑访问权。")
                .child(match self.decision {
                    None => "等待选择",
                    Some(Decision::Allow) => "已允许（模拟）",
                    Some(Decision::Reject) => "已拒绝（模拟）",
                })
                .child(
                    Button::new("reject")
                        .label("拒绝")
                        .large()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.decision = Some(Decision::Reject);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("allow")
                        .label("允许此次")
                        .large()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.decision = Some(Decision::Allow);
                            cx.notify();
                        })),
                ),
        };
        div()
            .size_full()
            .v_flex()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .map(|view| {
                #[cfg(target_os = "android")]
                if let Some(window) = gpui_mobile::android::jni::platform()
                    .and_then(|platform| platform.primary_window())
                {
                    // Android supplies logical insets for this surface, including resize changes.
                    let insets = window.safe_area_insets_logical();
                    return view
                        .pt(px(insets.top))
                        .pb(px(insets.bottom))
                        .pl(px(insets.left))
                        .pr(px(insets.right));
                }
                view
            })
            .child(
                div()
                    .h_flex()
                    .p_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .when(self.page != Page::Sessions, |view| {
                        view.child(
                            Button::new("back")
                                .label("返回")
                                .ghost()
                                .large()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let page = match this.page {
                                        Page::Tool | Page::Approval => Page::Conversation,
                                        _ => Page::Sessions,
                                    };
                                    this.navigate(page, window, cx);
                                })),
                        )
                    })
                    .child(title),
            )
            .child(
                div()
                    .id("content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(div().p_4().child(body)),
            )
            .when(self.page == Page::Conversation, |view| {
                view.child(
                    div()
                        .v_flex()
                        .p_4()
                        .gap_2()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(Textarea::new(&self.input).aria_label("验证输入"))
                        .child(
                            Button::new("record-input")
                                .label("显示输入结果")
                                .large()
                                .disabled(self.input.read(cx).value().is_empty())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.record_input(window, cx);
                                })),
                        ),
                )
            })
    }
}
