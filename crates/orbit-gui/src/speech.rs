//! GPUI speech capture and draft insertion; model preparation and inference live in orbit-client.
#[cfg(feature = "test-support")]
mod testing;
use std::{sync::Arc, time::Duration};

use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    component::{
        Disableable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        input::TextareaState,
        notification::Notification,
        speech::{
            AudioFormat, RecognitionSession, SpeechButton, SpeechError, SpeechEvent,
            SpeechRecognizer, SpeechSink, SpeechState, SpeechStatus, SpeechWaveform,
        },
    },
    *,
};
use orbit_client::speech::{self, CancellationToken, Capture, Progress, Recognizer};

const TRANSCRIPTION_TIMEOUT: Duration = Duration::from_secs(120);

enum Setup {
    Idle,
    Preparing {
        progress: Progress,
        cancel: CancellationToken,
    },
    Ready(Arc<Recognizer>),
    Failed(String),
}

struct Model {
    setup: Setup,
    runtime: Arc<tokio::runtime::Runtime>,
    task: Task<()>,
}

struct SharedModel(Entity<Model>);
impl Global for SharedModel {}

impl Model {
    fn prepare(&mut self, cx: &mut Context<Self>) {
        if matches!(self.setup, Setup::Preparing { .. } | Setup::Ready(_)) {
            return;
        }
        let directory = match speech::model_directory() {
            Ok(path) => path,
            Err(error) => {
                self.setup = Setup::Failed(error.to_string());
                cx.notify();
                return;
            }
        };
        let cancel = CancellationToken::new();
        let (progress, mut updates) = tokio::sync::watch::channel(Progress::default());
        let mut prepare = self
            .runtime
            .spawn(speech::prepare(directory, cancel.clone(), progress));
        self.setup = Setup::Preparing {
            progress: Progress::default(),
            cancel: cancel.clone(),
        };
        self.task = cx.spawn(async move |view, cx| {
            let mut watching = true;
            loop {
                tokio::select! {
                    result = &mut prepare => {
                        let _ = view.update(cx, |view, cx| {
                            if cancel.is_cancelled() {
                                return;
                            }
                            view.setup = match result {
                                Ok(Ok(engine)) => Setup::Ready(engine),
                                Ok(Err(error)) => Setup::Failed(error.to_string()),
                                Err(error) => Setup::Failed(format!("语音准备失败：{error}")),
                            };
                            cx.notify();
                        });
                        break;
                    }
                    changed = updates.changed(), if watching => {
                        watching = changed.is_ok();
                        let progress = *updates.borrow_and_update();
                        let _ = view.update(cx, |view, cx| {
                            if !cancel.is_cancelled()
                                && let Setup::Preparing { progress: value, .. } = &mut view.setup
                            {
                                *value = progress;
                                cx.notify();
                            }
                        });
                    }
                }
            }
        });
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Setup::Preparing { cancel, .. } = &self.setup {
            cancel.cancel();
            self.setup = Setup::Idle;
            cx.notify();
        }
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        if let Setup::Preparing { cancel, .. } = &self.setup {
            cancel.cancel();
        }
    }
}

struct LocalRecognizer(Entity<Model>);
impl SpeechRecognizer for LocalRecognizer {
    fn audio_format(&self) -> AudioFormat {
        AudioFormat::new(speech::SAMPLE_RATE_HZ as u32, 1)
    }

    fn is_available(&self, cx: &App) -> bool {
        matches!(self.0.read(cx).setup, Setup::Ready(_))
    }

    fn start(
        &self,
        sink: SpeechSink,
        cx: &mut App,
    ) -> Result<Box<dyn RecognitionSession>, SpeechError> {
        let model = self.0.read(cx);
        let Setup::Ready(engine) = &model.setup else {
            return Err(SpeechError::Unsupported);
        };
        let cancel = CancellationToken::new();
        let (updates, mut partials) = tokio::sync::watch::channel(String::new());
        let (capture, recognition) = engine.start(cancel.clone(), updates);
        let mut worker = model
            .runtime
            .spawn_blocking(move || recognition.run());
        let output = sink.clone();
        let task = cx.spawn(async move |cx| {
            let mut watching = true;
            loop {
                tokio::select! {
                    result = &mut worker => {
                        cx.update(|cx| match result {
                            Ok(Ok(text)) => {
                                output.phrase(text, cx);
                                output.finish(cx);
                            }
                            Ok(Err(error)) => output.error(SpeechError::recognizer(error), cx),
                            Err(error) => output.error(SpeechError::recognizer(error), cx),
                        });
                        break;
                    }
                    changed = partials.changed(), if watching => {
                        watching = changed.is_ok();
                        let text = partials.borrow_and_update().clone();
                        cx.update(|cx| output.hypothesis(text, cx));
                    }
                }
            }
        });
        sink.ready(cx);
        Ok(Box::new(Session {
            sink,
            capture: Some(capture),
            cancel,
            task: Some(task),
        }))
    }
}

struct Session {
    sink: SpeechSink,
    capture: Option<Capture>,
    cancel: CancellationToken,
    task: Option<Task<()>>,
}

impl RecognitionSession for Session {
    fn push_audio(&mut self, samples: &[i16], cx: &mut App) {
        if let Some(capture) = &mut self.capture
            && let Err(error) = capture.push(samples)
        {
            self.sink
                .error(SpeechError::recognizer(error), cx);
        }
    }

    fn finish(&mut self, cx: &mut App) {
        self.capture = None;
        let cancel = self.cancel.clone();
        let sink = self.sink.clone();
        let worker = self.task.take();
        self.task = Some(cx.spawn(async move |cx| {
            let _worker = worker;
            cx.background_executor()
                .timer(TRANSCRIPTION_TIMEOUT)
                .await;
            cancel.cancel();
            cx.update(|cx| {
                sink.error(
                    SpeechError::recognizer(anyhow::anyhow!("本地语音收尾超时，请重试")),
                    cx,
                )
            });
        }));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct DraftPreview {
    original: String,
    original_range: std::ops::Range<usize>,
    current_range: std::ops::Range<usize>,
    expected: String,
}

impl DraftPreview {
    fn new(input: &TextareaState) -> Self {
        let original = input.value().to_string();
        let range = input.selected_range();
        Self {
            original: original.clone(),
            expected: original,
            original_range: range.clone(),
            current_range: range,
        }
    }

    fn apply(
        &mut self,
        text: &str,
        input: &mut TextareaState,
        window: &mut Window,
        cx: &mut Context<TextareaState>,
    ) -> bool {
        if input.value().as_ref() != self.expected {
            return false;
        }
        input.set_selected_range(self.current_range.clone(), cx);
        input.replace(text.to_owned(), window, cx);
        self.current_range.end = self.current_range.start + text.len();
        self.expected = input.value().to_string();
        true
    }

    fn restore(
        self,
        input: &mut TextareaState,
        window: &mut Window,
        cx: &mut Context<TextareaState>,
    ) {
        if input.value().as_ref() == self.expected {
            input.set_selected_range(self.current_range, cx);
            input.replace(
                self.original[self.original_range.clone()].to_owned(),
                window,
                cx,
            );
            input.set_selected_range(self.original_range, cx);
        }
    }
}

pub(crate) struct Dictation {
    input: Entity<TextareaState>,
    preview: Option<DraftPreview>,
    state: Entity<SpeechState>,
    model: Entity<Model>,
    _subscriptions: Vec<Subscription>,
}

impl Dictation {
    pub(crate) fn new(
        input: Entity<TextareaState>,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = if let Some(model) = cx.try_global::<SharedModel>() {
            model.0.clone()
        } else {
            let model = cx.new(|_| Model {
                setup: Setup::Idle,
                runtime,
                task: Task::ready(()),
            });
            cx.set_global(SharedModel(model.clone()));
            model
        };
        let state = cx.new(|cx| {
            let state = SpeechState::new(cx)
                .system_fallback(false)
                .recognizer(LocalRecognizer(model.clone()))
                .stop_timeout(TRANSCRIPTION_TIMEOUT + Duration::from_secs(1));
            #[cfg(feature = "test-support")]
            let state = if let Some(path) = std::env::var_os("ORBIT_GUI_SPEECH_TEST_PCM") {
                state.input(testing::FileInput(path.into()))
            } else {
                state
            };
            state
        });
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.subscribe_in(&state, window, |view, _, event, window, cx| match event {
                SpeechEvent::Started => {
                    view.preview = Some(DraftPreview::new(view.input.read(cx)));
                }
                SpeechEvent::Partial(text) | SpeechEvent::Final(text) => {
                    if text.is_empty() {
                        if matches!(event, SpeechEvent::Final(_)) {
                            view.restore(window, cx);
                        }
                        return;
                    }
                    if let Some(preview) = &mut view.preview {
                        let applied = view
                            .input
                            .update(cx, |input, cx| preview.apply(text, input, window, cx));
                        if !applied {
                            view.commit(cx);
                        }
                    }
                    if matches!(event, SpeechEvent::Final(_)) {
                        view.preview = None;
                    }
                }
                SpeechEvent::Cancelled => {
                    view.restore(window, cx);
                }
                SpeechEvent::Error(error) => {
                    view.preview = None;
                    window.push_notification(
                        Notification::error(format!("语音输入失败：{error}")),
                        cx,
                    );
                }
            }),
        ];
        Self {
            input,
            preview: None,
            state,
            model,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn is_active(&self, cx: &App) -> bool {
        self.state.read(cx).status().is_active()
    }

    fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(preview) = self.preview.take() {
            self.input
                .update(cx, |input, cx| preview.restore(input, window, cx));
        }
    }

    pub(crate) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.restore(window, cx);
        self.state
            .update(cx, |state, cx| state.cancel(cx));
    }

    pub(crate) fn commit(&mut self, cx: &mut Context<Self>) {
        self.preview = None;
        self.state
            .update(cx, |state, cx| state.cancel(cx));
    }
}

impl Render for Dictation {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let status = self.state.read(cx).status();
        h_flex()
            .when(status.is_active(), |bar| bar.flex_1().min_w_0())
            .gap_1()
            .child(match &model.setup {
                Setup::Ready(_) if status.is_active() => h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        Button::new("cancel-dictation")
                            .small()
                            .ghost()
                            .rounded_full()
                            .icon(gpui_kit::assets::IconName::X)
                            .accessibility_label("取消语音输入并还原草稿")
                            .tooltip("取消语音输入并还原草稿")
                            .on_click(cx.listener(|view, _, window, cx| view.cancel(window, cx))),
                    )
                    .child(
                        SpeechWaveform::new(&self.state)
                            .w_0()
                            .flex_1()
                            .min_w_0()
                            .small(),
                    )
                    .when(status == SpeechStatus::Stopping, |bar| {
                        bar.child(div().text_sm().child("转写中…"))
                    })
                    .child(
                        Button::new("finish-dictation")
                            .small()
                            .ghost()
                            .rounded_full()
                            .icon(gpui_kit::assets::IconName::Square)
                            .loading(status == SpeechStatus::Stopping)
                            .disabled(!status.is_capturing())
                            .accessibility_label("完成录音并保留文字")
                            .tooltip("完成录音并保留文字")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.state
                                    .update(cx, |state, cx| state.stop(cx));
                            })),
                    )
                    .into_any_element(),
                Setup::Ready(_) => SpeechButton::new(&self.state)
                    .small()
                    .into_any_element(),
                Setup::Preparing { progress, .. } => {
                    let label = if progress.total_bytes == 0 {
                        "准备语音…".to_owned()
                    } else if progress.received_bytes == progress.total_bytes {
                        "加载语音模型…".to_owned()
                    } else {
                        format!(
                            "下载语音模型 {}%",
                            progress.received_bytes * 100 / progress.total_bytes
                        )
                    };
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("preparing-speech")
                                .small()
                                .ghost()
                                .loading(true)
                                .label(label)
                                .disabled(true),
                        )
                        .child(
                            Button::new("cancel-speech-download")
                                .small()
                                .ghost()
                                .icon(gpui_kit::assets::IconName::X)
                                .accessibility_label("取消语音准备")
                                .tooltip("取消语音准备")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.model
                                        .update(cx, |model, cx| model.cancel(cx))
                                })),
                        )
                        .into_any_element()
                }
                Setup::Idle | Setup::Failed(_) => {
                    let tooltip = match &model.setup {
                        Setup::Failed(error) => format!("{error}；点击重试"),
                        _ => "启用本地语音输入，首次下载约 226 MiB；音频不会上传".to_owned(),
                    };
                    Button::new("prepare-speech")
                        .small()
                        .ghost()
                        .icon(gpui_kit::assets::IconName::Mic)
                        .accessibility_label("启用本地语音输入")
                        .when(matches!(model.setup, Setup::Failed(_)), |button| {
                            button.label("重试语音")
                        })
                        .tooltip(tooltip)
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.model
                                .update(cx, |model, cx| model.prepare(cx))
                        }))
                        .into_any_element()
                }
            })
    }
}
