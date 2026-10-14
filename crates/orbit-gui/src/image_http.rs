//! GPUI image downloads use Orbit's existing Tokio runtime and HTTP stack.
use gpui_kit::http_client::{AsyncBody, HttpClient, Request, Response, http};
use std::{future::Future, pin::Pin, time::Duration};

pub(crate) struct ImageHttpClient {
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
}
impl ImageHttpClient {
    pub fn new(runtime: tokio::runtime::Handle) -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()?,
            runtime,
        })
    }
}
impl HttpClient for ImageHttpClient {
    fn user_agent(&self) -> Option<&http::HeaderValue> {
        None
    }
    fn proxy(&self) -> Option<&url::Url> {
        None
    }
    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Response<AsyncBody>>> + Send + 'static>> {
        let client = self.client.clone();
        let task = self.runtime.spawn(async move {
            anyhow::ensure!(request.method() == http::Method::GET, "仅支持图片 GET 请求");
            let url = url::Url::parse(&request.uri().to_string())?;
            anyhow::ensure!(matches!(url.scheme(), "http" | "https"), "不支持的图片地址");
            let mut response = client.get(url).send().await?;
            let limit = orbit_model::types::Attachment::MAX_IMAGE_BYTES;
            anyhow::ensure!(
                response
                    .content_length()
                    .is_none_or(|size| size <= limit as u64),
                "图片超过 20 MiB"
            );
            let mut result = Response::builder().status(response.status());
            *result.headers_mut().unwrap() = response.headers().clone();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                anyhow::ensure!(chunk.len() <= limit - bytes.len(), "图片超过 20 MiB");
                bytes.extend_from_slice(&chunk);
            }
            result.body(AsyncBody::from(bytes)).map_err(Into::into)
        });
        Box::pin(async move { task.await? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn downloads_images_and_bounds_responses_without_content_length() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = [0; 1024];
                let count = socket.read(&mut request).unwrap();
                if String::from_utf8_lossy(&request[..count]).contains("/image ") {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Type: image/png\r\nConnection: close\r\n\r\npng").unwrap();
                } else {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
                    let chunk = vec![b'x'; 1024 * 1024];
                    for _ in 0..21 {
                        if socket
                            .write_all(b"100000\r\n")
                            .and_then(|_| socket.write_all(&chunk))
                            .and_then(|_| socket.write_all(b"\r\n"))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = ImageHttpClient::new(runtime.handle().clone()).unwrap();
        runtime.block_on(async {
            let response = client
                .send(
                    Request::get(format!("http://{address}/image"))
                        .body(AsyncBody::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["content-type"], "image/png");
            assert!(
                client
                    .send(
                        Request::get(format!("http://{address}/large"))
                            .body(AsyncBody::empty())
                            .unwrap()
                    )
                    .await
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("20 MiB")
            );
        });
        server.join().unwrap();
    }
}
