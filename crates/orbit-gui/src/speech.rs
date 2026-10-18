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
use orbit_client::speech::{self, CancellationToken, Progress, Recognizer, Recording};

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
        let Setup::Ready(engine) = &self.0.read(cx).setup else {
            return Err(SpeechError::Unsupported);
        };
        let engine = engine.clone();
        sink.ready(cx);
        Ok(Box::new(Session {
            sink,
            engine,
            recording: Some(Recording::default()),
            cancel: CancellationToken::new(),
            task: None,
        }))
    }
}

struct Session {
    sink: SpeechSink,
    engine: Arc<Recognizer>,
    recording: Option<Recording>,
    cancel: CancellationToken,
    task: Option<Task<()>>,
}

impl RecognitionSession for Session {
    fn push_audio(&mut self, samples: &[i16], cx: &mut App) {
        if let Some(recording) = &mut self.recording
            && let Err(error) = recording.push(samples)
        {
            self.sink
                .error(SpeechError::recognizer(error), cx);
        }
    }

    fn finish(&mut self, cx: &mut App) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        let engine = self.engine.clone();
        let cancel = self.cancel.clone();
        let sink = self.sink.clone();
        self.task = Some(cx.spawn(async move |cx| {
            let interrupt = cancel.clone();
            let worker = cx.background_spawn(async move { engine.transcribe(recording, cancel) });
            let timeout = cx
                .background_executor()
                .timer(TRANSCRIPTION_TIMEOUT);
            let result = tokio::select! {
                result = worker => result,
                _ = timeout => {
                    interrupt.cancel();
                    Err(anyhow::anyhow!("本地语音转写超时，请缩短录音后重试"))
                }
            };
            cx.update(|cx| match result {
                Ok(text) => {
                    sink.phrase(text, cx);
                    sink.finish(cx);
                }
                Err(error) => sink.error(SpeechError::recognizer(error), cx),
            });
        }));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(crate) struct Dictation {
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
            cx.subscribe_in(&state, window, move |_, _, event, window, cx| match event {
                SpeechEvent::Final(text) if !text.trim().is_empty() => {
                    input.update(cx, |input, cx| input.insert(text.clone(), window, cx));
                }
                SpeechEvent::Error(error) => {
                    window.push_notification(
                        Notification::error(format!("语音输入失败：{error}")),
                        cx,
                    );
                }
                _ => {}
            }),
        ];
        Self {
            state,
            model,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.cancel(cx));
    }
}

impl Render for Dictation {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let status = self.state.read(cx).status();
        h_flex()
            .gap_1()
            .child(match &model.setup {
                Setup::Ready(_) => h_flex()
                    .gap_1()
                    .when(status.is_capturing(), |bar| {
                        bar.child(
                            SpeechWaveform::new(&self.state)
                                .w_12()
                                .small(),
                        )
                    })
                    .child(SpeechButton::new(&self.state).small())
                    .when(status == SpeechStatus::Stopping, |bar| {
                        bar.child(div().text_sm().child("转写中…"))
                    })
                    .when(status.is_active(), |bar| {
                        bar.child(
                            Button::new("cancel-dictation")
                                .small()
                                .ghost()
                                .icon(gpui_kit::assets::IconName::X)
                                .accessibility_label("取消语音输入")
                                .tooltip("取消语音输入")
                                .on_click(cx.listener(|view, _, _, cx| view.cancel(cx))),
                        )
                    })
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
                        _ => "启用本地语音输入，首次下载约 141 MiB；音频不会上传".to_owned(),
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
