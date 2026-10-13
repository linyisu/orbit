//! Application bindings are owned by the keymap; this view retains search,
//! recording and uncommitted text, including a draft after persistence fails.
use crate::{keymap, preferences::Preferences};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, ThemeStyled,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

#[derive(Clone)]
enum Change {
    Set {
        id: &'static str,
        text: String,
        replace: bool,
    },
    Clear(&'static str),
    Reset(&'static str),
    ResetAll,
}

struct Pending {
    revision: u64,
    expected: Vec<(&'static str, Option<String>)>,
    change: Change,
}

#[derive(Default)]
struct Validation {
    normalized: Option<String>,
    conflicts: Vec<&'static keymap::Command>,
    error: Option<String>,
}

struct Recording {
    original: String,
    strokes: Vec<String>,
}

enum Recorded {
    Ignore,
    Cancel,
    Finish { backwards: bool },
    Changed { text: String, complete: bool },
}

impl Recording {
    fn new(original: String) -> Self {
        Self {
            original,
            strokes: Vec::new(),
        }
    }

    fn key(&mut self, stroke: &Keystroke) -> Recorded {
        if stroke.key == "escape" && !stroke.modifiers.modified() {
            return Recorded::Cancel;
        }
        if stroke.key == "tab"
            && !stroke.modifiers.control
            && !stroke.modifiers.alt
            && !stroke.modifiers.platform
            && !stroke.modifiers.function
        {
            return Recorded::Finish {
                backwards: stroke.modifiers.shift,
            };
        }
        // GPUI also intercepts standalone modifier releases. They do not form
        // a command binding and must not use up one of the three strokes.
        if stroke.key.is_empty()
            || matches!(
                stroke.key.as_str(),
                "shift" | "ctrl" | "alt" | "cmd" | "super" | "fn"
            )
        {
            return Recorded::Ignore;
        }
        if self.strokes.len() >= 3 {
            return Recorded::Ignore;
        }
        self.strokes.push(stroke.unparse());
        Recorded::Changed {
            text: self.strokes.join(" "),
            complete: self.strokes.len() == 3,
        }
    }
}

pub(super) struct Shortcuts {
    query: Entity<InputState>,
    draft: Entity<InputState>,
    editing: Option<&'static str>,
    validation: Validation,
    recording: Option<Recording>,
    record_focus: FocusHandle,
    record_trigger: FocusHandle,
    interception: Option<Subscription>,
    pending: Option<Pending>,
    failed: Option<Change>,
    error: Option<String>,
    status: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl Shortcuts {
    pub(super) fn has_pending_work(&self, cx: &App) -> bool {
        self.recording.is_some()
            || self.pending.is_some()
            || self.failed.is_some()
            || self.editing.is_some_and(|id| {
                let saved = keymap::configured(id, cx)
                    .or_else(|| keymap::current(id, cx))
                    .unwrap_or_default();
                let text = self.draft.read(cx).value();
                text.trim() != saved
                    && keymap::validate(id, &text, cx)
                        .ok()
                        .is_none_or(|text| text != saved)
            })
    }

    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("搜索命令或按键…"));
        let draft = cx.new(|cx| InputState::new(window, cx).placeholder("例如 ctrl-k ctrl-b"));
        let record_focus = cx.focus_handle().tab_stop(false);
        let record_trigger = cx.focus_handle();
        let query_events = cx.subscribe(&query, |_: &mut Self, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let draft_events = cx.subscribe_in(
            &draft,
            window,
            |view: &mut Self, _, event, window, cx| match event {
                InputEvent::Change => {
                    view.status = None;
                    view.error = None;
                    view.failed = None;
                    view.validate(cx);
                }
                InputEvent::PressEnter { shift: false, .. } => view.save(false, window, cx),
                _ => {}
            },
        );
        let preferences =
            cx.observe_global_in::<Preferences>(window, |view: &mut Self, window, cx| {
                view.saved(window, cx);
                view.validate(cx);
            });
        let focus_out = cx.on_focus_out(&record_focus, window, |view: &mut Self, _, _, cx| {
            // Clicking another control or leaving this page ends capture. Do
            // not steal focus back from the control the user just activated.
            if view.recording.take().is_some() {
                view.interception.take();
                cx.notify();
            }
        });
        Self {
            query,
            draft,
            editing: None,
            validation: Validation::default(),
            recording: None,
            record_focus,
            record_trigger,
            interception: None,
            pending: None,
            failed: None,
            error: None,
            status: None,
            _subscriptions: vec![query_events, draft_events, preferences, focus_out],
        }
    }

    pub(super) fn deactivate(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let had_focus = self.recording.is_some() && self.record_focus.is_focused(window);
        self.stop_recording(true, false, window, cx);
        had_focus
    }

    fn edit(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_some() || self.editing.is_some_and(|editing| editing != id) {
            return;
        }
        self.editing = Some(id);
        self.error = None;
        self.failed = None;
        self.status = None;
        let value = keymap::configured(id, cx)
            .or_else(|| keymap::current(id, cx))
            .unwrap_or_default();
        self.draft.update(cx, |input, cx| {
            input.set_value(value, window, cx);
            input.focus(window, cx);
        });
        self.validate(cx);
    }

    fn validate(&mut self, cx: &mut Context<Self>) {
        self.validation = Validation::default();
        if let Some(id) = self.editing {
            let text = self.draft.read(cx).value();
            match keymap::conflicts(id, &text, cx) {
                Ok(conflicts) => {
                    self.validation.conflicts = conflicts;
                    if self.validation.conflicts.is_empty() {
                        match keymap::validate(id, &text, cx) {
                            Ok(normalized) => self.validation.normalized = Some(normalized),
                            Err(error) => self.validation.error = Some(error.to_string()),
                        }
                    }
                }
                Err(error) => self.validation.error = Some(error.to_string()),
            }
        }
        cx.notify();
    }

    fn start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_none() || self.pending.is_some() {
            return;
        }
        self.stop_recording(false, false, window, cx);
        self.recording = Some(Recording::new(self.draft.read(cx).value().to_string()));
        window.focus(&self.record_focus, cx);
        let entity = cx.weak_entity();
        self.interception = Some(cx.intercept_keystrokes(move |event, window, cx| {
            let _ = entity.update(cx, |view, cx| {
                if view.recording.is_some() && view.record_focus.is_focused(window) {
                    // This runs before keymap resolution, so recording Ctrl+N
                    // cannot create a task and a chord cannot leave pending input.
                    cx.stop_propagation();
                    view.record(&event.keystroke, window, cx);
                }
            });
        }));
        cx.notify();
    }

    fn record(&mut self, stroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let Some(recording) = &mut self.recording else {
            return;
        };
        match recording.key(stroke) {
            Recorded::Ignore => {}
            Recorded::Cancel => self.stop_recording(true, true, window, cx),
            Recorded::Finish { backwards } => {
                self.stop_recording(false, true, window, cx);
                if backwards {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
            }
            Recorded::Changed { text, complete } => {
                self.draft
                    .update(cx, |input, cx| input.set_value(text, window, cx));
                self.validate(cx);
                if complete {
                    self.stop_recording(false, true, window, cx);
                }
            }
        }
    }

    fn stop_recording(
        &mut self,
        cancel: bool,
        restore_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        self.interception.take();
        if cancel {
            self.draft.update(cx, |input, cx| {
                input.set_value(recording.original, window, cx)
            });
            self.validate(cx);
        }
        if restore_focus {
            window.focus(&self.record_trigger, cx);
        }
        cx.notify();
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            return;
        }
        self.stop_recording(true, false, window, cx);
        self.editing = None;
        self.validation = Validation::default();
        self.query.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn save(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.editing else {
            return;
        };
        if self.recording.is_some() {
            return;
        }
        self.validate(cx);
        if self.validation.error.is_some() || (!replace && !self.validation.conflicts.is_empty()) {
            return;
        }
        self.apply(
            Change::Set {
                id,
                text: self.draft.read(cx).value().to_string(),
                replace,
            },
            window,
            cx,
        );
    }

    fn replace(&mut self, confirmed: &[&'static str], window: &mut Window, cx: &mut Context<Self>) {
        self.validate(cx);
        let current: Vec<_> = self
            .validation
            .conflicts
            .iter()
            .map(|command| command.id())
            .collect();
        if current != confirmed {
            self.error = Some("冲突已变化，请核对列表后再次选择替换。".into());
            cx.notify();
            return;
        }
        self.save(true, window, cx);
    }

    fn apply(&mut self, change: Change, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            return;
        }
        self.stop_recording(false, false, window, cx);
        let result = match &change {
            Change::Set {
                id,
                text,
                replace: false,
            } => keymap::set(id, text, cx),
            Change::Set {
                id,
                text,
                replace: true,
            } => keymap::set_replacing(id, text, cx),
            Change::Clear(id) => keymap::clear(id, cx),
            Change::Reset(id) => keymap::reset(id, cx),
            Change::ResetAll => keymap::reset_all(cx),
        };
        self.status = None;
        match result {
            Ok(revision) => {
                self.error = None;
                self.failed = None;
                self.pending = Some(Pending {
                    revision,
                    expected: keymap::catalog()
                        .iter()
                        .map(|command| (command.id(), keymap::current(command.id(), cx)))
                        .collect(),
                    change,
                });
                self.saved(window, cx);
            }
            Err(error) => {
                if let (Change::Reset(id), keymap::ValidationError::Conflict { .. }) =
                    (&change, &error)
                {
                    self.edit(id, window, cx);
                    if let Some(command) =
                        keymap::catalog().iter().find(|command| command.id() == *id)
                    {
                        self.draft.update(cx, |input, cx| {
                            input.set_value(command.default_binding(), window, cx)
                        });
                    }
                } else {
                    self.error = Some(error.to_string());
                }
            }
        }
        self.validate(cx);
    }

    fn saved(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = &self.pending else {
            return;
        };
        let status = keymap::save_status(cx);
        if status.revision() < pending.revision {
            return;
        }
        match status {
            keymap::SaveStatus::Saving(_) => {}
            keymap::SaveStatus::Saved(_) => {
                let pending = self.pending.take().unwrap();
                if pending
                    .expected
                    .iter()
                    .all(|(id, expected)| keymap::current(id, cx) == *expected)
                {
                    self.failed = None;
                    self.error = None;
                    self.status = Some("快捷键已保存".into());
                    // Keep the form and its focus in place. Settings can have
                    // become hidden while the writer was running; completion
                    // must never focus an input on an invisible page.
                } else {
                    self.error = Some(
                        "保存期间快捷键已被其他设置更改。草稿已保留，请检查当前键位后重试。".into(),
                    );
                }
            }
            keymap::SaveStatus::Failed { message, .. } => {
                let pending = self.pending.take().unwrap();
                self.failed = Some(pending.change);
                self.error = Some(format!(
                    "键位已在本次运行应用，但无法保存：{message}。草稿已保留，可重试。"
                ));
            }
        }
        cx.notify();
    }

    fn render_editor(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let busy = self.pending.is_some();
        let recording = self.recording.is_some();
        let confirmed_conflicts: Vec<_> = self
            .validation
            .conflicts
            .iter()
            .map(|command| command.id())
            .collect();
        // The component Button owns a private keyed focus handle. Base Button
        // exposes caller-owned focus, which lets Escape return to this exact
        // recording trigger without reaching into component-private state.
        let record = gpui_kit::base::Button::new("shortcut-record")
            .track_focus(&self.record_trigger)
            .disabled(busy)
            .accessibility_label("录制快捷键")
            .px_3()
            .py_2()
            .text_sm()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .text_color(cx.theme().secondary_foreground)
            .when(!busy, |button| {
                button.hover(|button| button.bg(cx.theme().muted))
            })
            .when(busy, |button| button.opacity(0.5))
            .when(self.record_trigger.is_focused(window), |button| {
                button.focus_ring_style(window, cx)
            })
            .child(if recording {
                "重新录制"
            } else {
                "录制按键"
            })
            .on_click(cx.listener(|view, _, window, cx| view.start_recording(window, cx)));
        v_flex().w_full().min_w_0().gap_3().pt_3()
            .child(div().text_sm().child("新快捷键"))
            .child(Input::new(&self.draft).disabled(busy || recording).aria_label("新快捷键，空格分隔多击序列"))
            .child(h_flex().flex_wrap().gap_2()
                .child(record)
                .child(Button::new("shortcut-save").small().primary().label("保存")
                    .disabled(busy || recording || self.validation.normalized.is_none())
                    .on_click(cx.listener(|view, _, window, cx| view.save(false, window, cx))))
                .child(Button::new("shortcut-cancel").small().ghost().label(if self.status.is_some() { "完成" } else { "取消" })
                    .disabled(busy)
                    .on_click(cx.listener(|view, _, window, cx| view.cancel_edit(window, cx)))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("支持最多 3 击，以空格分隔，例如 ctrl-k ctrl-b。录制时 Tab 结束，Esc 取消录制。"))
            .when(recording, |view| view.child(
                div().id("shortcut-recording").track_focus(&self.record_focus).key_context("ShortcutRecording")
                    .p_3().border_1().border_color(cx.theme().primary).rounded(cx.theme().radius)
                    .child("正在录制：按下快捷键，或按 Tab 结束、Esc 取消。")))
            .when_some(self.validation.error.clone(), |view, error| view.child(
                div().text_sm().text_color(cx.theme().danger).child(error)))
            .when(!self.validation.conflicts.is_empty(), |view| view.child(
                v_flex().min_w_0().gap_2()
                    .child(div().text_sm().child("此按键与以下命令冲突；替换会清除这些命令的当前键位："))
                    .children(self.validation.conflicts.iter().map(|command| div().text_sm()
                        .child(format!("{} · {}", command.label(), command.scope()))))
                    .child(h_flex().child(Button::new("shortcut-replace").small().outline().label("替换冲突并保存")
                        .disabled(busy || recording)
                        .on_click(cx.listener(move |view, _, window, cx| view.replace(&confirmed_conflicts, window, cx)))))))
    }

    fn render_command(
        &self,
        command: &'static keymap::Command,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let id = command.id();
        let current = keymap::current(id, cx);
        let custom = keymap::is_customized(id, cx);
        let editing = self.editing == Some(id);
        let disabled = self.pending.is_some()
            || self.recording.is_some()
            || self.editing.is_some_and(|editing| editing != id);
        v_flex()
            .id(SharedString::from(format!("shortcut:{id}")))
            .w_full()
            .min_w_0()
            .gap_1()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .flex_basis(rems(8.))
                            .min_w_0()
                            .child(command.label()),
                    )
                    .child(binding(current.as_deref(), cx))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .child(
                                Button::new(SharedString::from(format!("shortcut-edit:{id}")))
                                    .small()
                                    .outline()
                                    .label("编辑…")
                                    .disabled(disabled || editing)
                                    .accessibility_label(format!("编辑{}快捷键", command.label()))
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.edit(id, window, cx)
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("shortcut-clear:{id}")))
                                    .small()
                                    .ghost()
                                    .label("清除")
                                    .disabled(disabled || editing || current.is_none())
                                    .accessibility_label(format!("清除{}快捷键", command.label()))
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.apply(Change::Clear(id), window, cx)
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("shortcut-reset:{id}")))
                                    .small()
                                    .ghost()
                                    .label("恢复默认")
                                    .disabled(disabled || editing || !custom)
                                    .accessibility_label(format!(
                                        "恢复{}默认快捷键",
                                        command.label()
                                    ))
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.apply(Change::Reset(id), window, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "默认 {} · {}{}",
                        command.default_binding(),
                        command.scope(),
                        if custom { " · 已自定义" } else { "" }
                    )),
            )
            .when(editing, |view| view.child(self.render_editor(window, cx)))
    }

    fn render_warnings(
        &self,
        warnings: Vec<keymap::LoadWarning>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let disabled = self.pending.is_some() || self.recording.is_some() || self.editing.is_some();
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("待修复的快捷键"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("部分保存配置无法应用。原内容仍保留，可编辑修复或恢复默认。"),
            )
            .children(warnings.into_iter().map(|warning| {
                let id = warning.id();
                let label = keymap::catalog()
                    .iter()
                    .find(|command| command.id() == id)
                    .map(keymap::Command::label)
                    .unwrap_or(id);
                v_flex()
                    .id(SharedString::from(format!("shortcut-warning:{id}")))
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .py_2()
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .flex_wrap()
                            .gap_2()
                            .child(div().flex_1().flex_basis(rems(8.)).min_w_0().child(label))
                            .child(
                                Button::new(SharedString::from(format!(
                                    "shortcut-warning-edit:{id}"
                                )))
                                .small()
                                .outline()
                                .label("编辑…")
                                .disabled(disabled)
                                .accessibility_label(format!("修复{label}快捷键"))
                                .on_click(
                                    cx.listener(move |view, _, window, cx| {
                                        view.edit(id, window, cx)
                                    }),
                                ),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "shortcut-warning-reset:{id}"
                                )))
                                .small()
                                .ghost()
                                .label("恢复默认")
                                .disabled(disabled)
                                .accessibility_label(format!("恢复{label}默认快捷键"))
                                .on_click(cx.listener(
                                    move |view, _, window, cx| {
                                        view.apply(Change::Reset(id), window, cx)
                                    },
                                )),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().warning)
                            .child(warning.message().to_owned()),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .text_sm()
                            .text_ellipsis()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("已保存：{}", warning.configured())),
                    )
            }))
    }
}

impl Render for Shortcuts {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.query.read(cx).value().to_string();
        let commands: Vec<_> = keymap::catalog()
            .iter()
            .filter(|command| {
                self.editing == Some(command.id())
                    || matches_query(
                        command,
                        keymap::current(command.id(), cx).as_deref(),
                        keymap::configured(command.id(), cx).as_deref(),
                        &query,
                    )
            })
            .collect();
        let customized = keymap::catalog()
            .iter()
            .any(|command| keymap::is_customized(command.id(), cx));
        let busy = self.pending.is_some();
        let warnings = keymap::load_warnings(cx);
        let failed = self.failed.clone();
        let persistence_error = match keymap::save_status(cx) {
            keymap::SaveStatus::Failed { message, .. } if self.error.is_none() => {
                Some(format!("设置尚未保存：{message}"))
            }
            _ => None,
        };
        v_flex().w_full().min_w_0().gap_4()
            .child(h_flex().w_full().min_w_0().flex_wrap().gap_3()
                .child(div().flex_1().flex_basis(rems(18.)).min_w_0()
                    .child(Input::new(&self.query).prefix(Icon::new(IconName::Search)).cleanable(true)
                        .disabled(busy || self.recording.is_some()).aria_label("搜索命令或快捷键")))
                .child(Button::new("shortcut-reset-all").small().outline().label("全部恢复默认")
                    .disabled(busy || !customized || self.editing.is_some())
                    .on_click(cx.listener(|view, _, window, cx| view.apply(Change::ResetAll, window, cx)))))
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("这些设置适用于所有工作区。应用命令与消息输入可自定义；文本编辑和文件页使用固定键位。"))
            .when(!warnings.is_empty(), |view| view.child(self.render_warnings(warnings, cx)))
            .when(busy, |view| view.child(div().text_sm().child("正在保存快捷键…")))
            .when_some(self.error.clone().or(persistence_error), |view, error| view.child(
                v_flex().min_w_0().gap_2().child(div().text_sm().text_color(cx.theme().danger).child(error))
                    .when_some(failed, |view, failed| view.child(h_flex().child(
                        Button::new("shortcut-retry").small().outline().label("重试保存").disabled(busy)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                // Previously cleared conflicts have already
                                // taken effect. A retry must reject any new
                                // conflict rather than reuse old consent.
                                let mut change = failed.clone();
                                if let Change::Set { replace, .. } = &mut change { *replace = false; }
                                view.apply(change, window, cx);
                            })))))))
            .when_some(self.status.clone(), |view, status| view.child(div().text_sm().text_color(cx.theme().muted_foreground).child(status)))
            .when(commands.is_empty(), |view| view.child(div().py_4().text_color(cx.theme().muted_foreground)
                .child("没有匹配的命令或按键，试试其他关键词。")))
            .children(commands.into_iter().map(|command| self.render_command(command, window, cx)))
    }
}

fn binding(value: Option<&str>, cx: &App) -> AnyElement {
    match value {
        Some(value) => h_flex()
            .flex_wrap()
            .gap_1()
            .children(
                value
                    .split_whitespace()
                    .map(|stroke| match Keystroke::parse(stroke) {
                        Ok(stroke) => Kbd::new(stroke).into_any_element(),
                        Err(_) => div().child(stroke.to_owned()).into_any_element(),
                    }),
            )
            .into_any_element(),
        None => div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("未绑定")
            .into_any_element(),
    }
}

fn matches_query(
    command: &keymap::Command,
    current: Option<&str>,
    configured: Option<&str>,
    query: &str,
) -> bool {
    let text = format!(
        "{} {} {} {} {} {} {} {}",
        command.id(),
        command.label(),
        command.action_name(),
        command.group(),
        command.scope(),
        current.unwrap_or("未绑定"),
        configured.unwrap_or_default(),
        command.default_binding()
    );
    search_matches(&text, query)
}

fn search_matches(text: &str, query: &str) -> bool {
    let text = text.to_lowercase().replace('+', "-");
    query
        .to_lowercase()
        .replace('+', "-")
        .split_whitespace()
        .all(|part| text.contains(part))
}

#[cfg(test)]
mod tests {
    use super::{Recorded, Recording, matches_query, search_matches};
    use gpui_kit::Keystroke;

    #[test]
    fn recording_keeps_chord_order_and_ends_after_three_strokes() {
        let mut recording = Recording::new("ctrl-p".into());
        for (source, expected, complete) in [
            ("ctrl-k", "ctrl-k", false),
            ("ctrl-b", "ctrl-k ctrl-b", false),
            ("alt-j", "ctrl-k ctrl-b alt-j", true),
        ] {
            match recording.key(&Keystroke::parse(source).unwrap()) {
                Recorded::Changed {
                    text,
                    complete: actual,
                } => {
                    assert_eq!(text, expected);
                    assert_eq!(actual, complete);
                }
                _ => panic!("stroke was not recorded"),
            }
        }
        assert!(matches!(
            recording.key(&Keystroke::parse("ctrl-f").unwrap()),
            Recorded::Ignore
        ));
    }

    #[test]
    fn escape_cancels_without_replacing_the_original_draft_and_tab_finishes() {
        let mut recording = Recording::new("ctrl-k ctrl-p".into());
        recording.key(&Keystroke::parse("ctrl-n").unwrap());
        assert!(matches!(
            recording.key(&Keystroke::parse("escape").unwrap()),
            Recorded::Cancel
        ));
        assert_eq!(recording.original, "ctrl-k ctrl-p");
        assert!(matches!(
            recording.key(&Keystroke::parse("tab").unwrap()),
            Recorded::Finish { backwards: false }
        ));
        assert!(matches!(
            recording.key(&Keystroke::parse("shift-tab").unwrap()),
            Recorded::Finish { backwards: true }
        ));
        assert!(matches!(
            recording.key(&Keystroke::parse("shift").unwrap()),
            Recorded::Ignore
        ));
    }

    #[test]
    fn modified_tab_and_composer_enter_are_recorded_as_bindings() {
        let mut recording = Recording::new(String::new());
        for source in ["ctrl-tab", "enter", "shift-enter"] {
            assert!(matches!(
                recording.key(&Keystroke::parse(source).unwrap()),
                Recorded::Changed { .. }
            ));
        }
        assert_eq!(recording.strokes, ["ctrl-tab", "enter", "shift-enter"]);
    }

    #[test]
    fn search_accepts_command_text_and_user_facing_key_notation() {
        let text = "workspace_commands 新任务 ctrl-shift-n 默认 ctrl-n";
        assert!(search_matches(text, "新任务 CTRL+SHIFT+N"));
        assert!(search_matches(text, "workspace_commands ctrl-n"));
        assert!(!search_matches(text, "ctrl-p"));
    }

    #[test]
    fn search_finds_a_repairable_saved_binding_when_the_active_binding_fell_back() {
        let command = crate::keymap::catalog()
            .iter()
            .find(|command| command.id() == "new_conversation")
            .unwrap();
        assert!(matches_query(
            command,
            Some("ctrl-n"),
            Some("ctrl-v"),
            "ctrl+v"
        ));
        assert!(matches_query(
            command,
            Some("ctrl-n"),
            Some("ctrl-v"),
            "ctrl+n"
        ));
        assert!(!matches_query(
            command,
            Some("ctrl-n"),
            Some("ctrl-v"),
            "ctrl-q"
        ));
    }
}
