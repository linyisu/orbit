//! Markdown images load through the owning workspace, including SSH/WSL paths.
use crate::file_preview::FileRequest;
use gpui_kit::*;
use orbit_client::protocol::Response;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

#[derive(Clone)]
enum Load {
    Pending,
    Ready(Arc<Image>),
    Failed(SharedString),
}
type Cache = Arc<Mutex<HashMap<String, Load>>>;

pub(super) struct ReplyImages {
    cache: Cache,
    loads: Vec<Task<()>>,
}

impl EventEmitter<FileRequest> for ReplyImages {}

impl ReplyImages {
    pub fn new(_: &mut Context<Self>) -> Self {
        Self {
            cache: Default::default(),
            loads: Vec::new(),
        }
    }

    pub fn resolver(
        owner: &Entity<Self>,
        cx: &App,
    ) -> impl Fn(&SharedUri) -> Option<ImageSource> + Send + Sync + 'static {
        let weak = owner.downgrade();
        let cache = owner.read(cx).cache.clone();
        move |uri| {
            let path = local_path(uri)?;
            if let Some(Load::Ready(image)) = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(uri.as_ref())
            {
                return Some(ImageSource::Image(image.clone()));
            }
            let uri = uri.to_string();
            let weak = weak.clone();
            // Only the image loader's window phase requests a missing resource.
            // No filesystem access or decoding happens in the Markdown parser.
            Some(ImageSource::Custom(Arc::new(move |_, cx| {
                let owner = weak.upgrade()?;
                let load = owner.update(cx, |images, cx| images.request(&uri, &path, cx));
                match load {
                    Load::Failed(error) => Some(Err(ImageCacheError::Asset(error))),
                    Load::Pending | Load::Ready(_) => None,
                }
            })))
        }
    }

    fn request(&mut self, uri: &str, path: &str, cx: &mut Context<Self>) -> Load {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(load) = cache.get(uri) {
            return load.clone();
        }
        cache.insert(uri.to_owned(), Load::Pending);
        drop(cache);
        let (response, received) = oneshot::channel();
        cx.emit(FileRequest::new(path.into(), response));
        let uri = uri.to_owned();
        self.loads
            .push(cx.spawn(async move |owner, cx| {
                let response = received
                    .await
                    .map_err(|_| "图片读取已取消".to_owned())
                    .and_then(|r| r);
                let decoded = cx
                    .background_spawn(async move {
                        let Response::File(encoded) = response? else {
                            return Err("图片响应格式无效".into());
                        };
                        let bytes = orbit_client::protocol::file_bytes(encoded)
                            .map_err(|e| e.to_string())?;
                        crate::image_preview::decode_image(bytes)
                            .map(|(image, _)| image)
                            .map_err(|e| e.to_string())
                    })
                    .await;
                let _ = owner.update(cx, |images, cx| {
                    let load = match decoded {
                        Ok(image) => Load::Ready(image),
                        Err(error) => Load::Failed(error.into()),
                    };
                    images
                        .cache
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(uri, load);
                    cx.notify();
                });
            }));
        Load::Pending
    }
}

/// Leave HTTP and embedded images to TextView. Local references are interpreted
/// by the target workspace, never by the desktop process's current directory.
fn local_path(source: &str) -> Option<String> {
    if source.is_empty() || source.starts_with("//") {
        return None;
    }
    let drive_path = source
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && source.as_bytes().get(1) == Some(&b':')
        && matches!(source.as_bytes().get(2), Some(b'/' | b'\\'));
    let (url, relative) = if drive_path {
        (url::Url::parse(&format!("file:///{source}")).ok()?, false)
    } else if let Ok(url) = url::Url::parse(source) {
        if url.scheme() != "file" {
            return None;
        }
        (url, false)
    } else {
        (
            url::Url::parse("file:///")
                .ok()?
                .join(source)
                .ok()?,
            !source.starts_with('/'),
        )
    };
    if url
        .host_str()
        .is_some_and(|host| host != "localhost")
    {
        return None;
    }
    // URL-to-Path conversion uses the desktop OS, but this path belongs to the session host.
    let decoded = percent_encoding::percent_decode_str(url.path())
        .decode_utf8()
        .ok()?;
    let drive_url = decoded
        .as_bytes()
        .get(1)
        .is_some_and(u8::is_ascii_alphabetic)
        && decoded.as_bytes().get(2) == Some(&b':')
        && decoded.as_bytes().get(3) == Some(&b'/');
    let path = if relative || drive_url {
        decoded.strip_prefix('/')?
    } else {
        &decoded
    };
    Some(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::local_path;

    #[test]
    fn local_references_decode_without_using_the_desktop_working_directory() {
        for (source, expected) in [
            ("/workspace/chart.png", "/workspace/chart.png"),
            ("images/chart.png", "images/chart.png"),
            ("./images/chart%20one.png", "images/chart one.png"),
            (
                "file:///workspace/chart%20one.svg",
                "/workspace/chart one.svg",
            ),
            ("images/图表.png#preview", "images/图表.png"),
            ("images/chart+one.png", "images/chart+one.png"),
            (
                "file:///C:/workspace/chart%20one.png",
                "C:/workspace/chart one.png",
            ),
            (r"C:\workspace\chart.png", "C:/workspace/chart.png"),
            ("C:/workspace/chart.png", "C:/workspace/chart.png"),
        ] {
            assert_eq!(local_path(source).as_deref(), Some(expected), "{source}");
        }
        for source in [
            "",
            "https://example.test/image.png",
            "data:image/png;base64,eA==",
            "//example.test/image.png",
            "file://another-host/image.png",
            "javascript:bad()",
            "file:///workspace/%FF.png",
        ] {
            assert_eq!(local_path(source), None, "{source}");
        }
    }
}
