//! Read-only attachment views. Session reads are routed by the owning workspace.
use gpui_kit::component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Editor, EditorState},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::*;
use orbit_client::{attachments::FilePreview, protocol::Response};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

type FileResponse = oneshot::Sender<Result<Response, String>>;

#[derive(Clone)]
pub(crate) struct FileRequest {
    path: String,
    response: Arc<Mutex<Option<FileResponse>>>,
    preview: Option<(String, Entity<Preview>)>,
}

impl FileRequest {
    pub(crate) fn new(path: String, response: FileResponse) -> Self {
        Self {
            path,
            response: Arc::new(Mutex::new(Some(response))),
            preview: None,
        }
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn preview(&self) -> Option<(String, Entity<Preview>)> {
        self.preview.clone()
    }

    pub(crate) fn take_response(&self) -> Option<FileResponse> {
        self.response
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

enum PreviewState {
    Loading,
    Text {
        editor: Entity<EditorState>,
        note: String,
        truncated: bool,
    },
    Unavailable(String),
}

pub(crate) struct Preview {
    state: PreviewState,
    _load: Task<()>,
}

/// Open immediately while the owning conversation forwards the one-shot service read.
pub(crate) fn open(
    file: orbit_protocol::MessageImage,
    window: &mut Window,
    cx: &mut App,
) -> FileRequest {
    let (response, received) = oneshot::channel();
    let read = cx.background_spawn(async move {
        let response = received
            .await
            .map_err(|_| "附件读取已取消".to_owned())??;
        let Response::File(encoded) = response else {
            return Err("附件响应格式无效".into());
        };
        orbit_client::protocol::file_bytes(encoded)
            .map(FilePreview::from_bytes)
            .map_err(|error| error.to_string())
    });
    let preview = preview(read, window, cx);
    let mut request = FileRequest::new(file.path, response);
    request.preview = Some((file.name, preview));
    request
}

/// Draft attachments belong to the desktop host until submission.
pub(crate) fn open_path(path: PathBuf, window: &mut Window, cx: &mut App) -> Entity<Preview> {
    let read = cx.background_spawn(async move {
        orbit_client::attachments::read_file(&path)
            .map(FilePreview::from_bytes)
            .map_err(|error| error.to_string())
    });
    preview(read, window, cx)
}

fn preview(
    read: Task<Result<FilePreview, String>>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Preview> {
    cx.new(|cx: &mut Context<Preview>| Preview {
        state: PreviewState::Loading,
        _load: cx.spawn_in(window, async move |owner, cx| {
            let result = read.await;
            let _ = owner.update_in(cx, |preview, window, cx| {
                preview.state = match result {
                    Ok(FilePreview::Text {
                        content,
                        bytes,
                        truncated,
                    }) => PreviewState::Text {
                        note: if truncated {
                            format!("{bytes} 字节 · 仅预览前 1 MiB，原文件保持完整")
                        } else if content.is_empty() {
                            "空文件".into()
                        } else {
                            format!("{bytes} 字节 · 只读")
                        },
                        editor: cx.new(|cx| {
                            EditorState::new(window, cx)
                                .language("plaintext")
                                .line_number(false)
                                .soft_wrap(true)
                                .default_value(content)
                        }),
                        truncated,
                    },
                    Ok(FilePreview::Binary { bytes }) => PreviewState::Unavailable(format!(
                        "此附件不是 UTF-8 文本，暂不支持文本预览（{bytes} 字节）。"
                    )),
                    Err(error) => PreviewState::Unavailable(format!("无法读取附件：{error}")),
                };
                cx.notify();
            });
        }),
    })
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.state {
            PreviewState::Loading => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child("正在读取附件…")
                .into_any_element(),
            PreviewState::Unavailable(message) => div()
                .whitespace_normal()
                .child(message.clone())
                .into_any_element(),
            PreviewState::Text {
                editor,
                note,
                truncated,
            } => {
                let source = editor.clone();
                v_flex()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .gap_2()
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(note.clone()),
                            )
                            .child(
                                Button::new("copy-attachment")
                                    .small()
                                    .ghost()
                                    .icon(gpui_kit::component::IconName::Copy)
                                    .label(if *truncated { "复制预览" } else { "复制" })
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            source.read(cx).value().to_string(),
                                        ));
                                    }),
                            ),
                    )
                    .child(
                        div().flex_1().min_h_0().child(
                            Editor::new(editor)
                                .readonly(true)
                                .h_full()
                                .aria_label("附件文本，只读"),
                        ),
                    )
                    .into_any_element()
            }
        }
    }
}
