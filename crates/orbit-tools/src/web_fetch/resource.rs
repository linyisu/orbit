//! Bounded image and JSON resources share DNS, proxy and redirect checks.
use super::*;

const RESOURCE_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn fetch_image(value: &str) -> Result<Vec<u8>, ToolError> {
    WebClient::default().fetch_image(value).await
}

impl WebClient {
    pub async fn fetch_image(&self, value: &str) -> Result<Vec<u8>, ToolError> {
        tokio::time::timeout(RESOURCE_TIMEOUT, fetch(value, self, Resource::Image))
            .await
            .map_err(|_| failed("图片请求超时"))?
    }

    pub async fn fetch_json(
        &self,
        value: &str,
        max_bytes: usize,
    ) -> Result<serde_json::Value, ToolError> {
        let bytes = tokio::time::timeout(
            RESOURCE_TIMEOUT,
            fetch(value, self, Resource::Json(max_bytes)),
        )
        .await
        .map_err(|_| failed("JSON 请求超时"))??;
        serde_json::from_slice(&bytes).map_err(failed)
    }
}

#[derive(Clone, Copy)]
enum Resource {
    Image,
    Json(usize),
}

async fn fetch(value: &str, client: &WebClient, resource: Resource) -> Result<Vec<u8>, ToolError> {
    let mut url = client.checked_url(value)?;
    let mut clients = RequestClients::default();
    let mut visited = std::collections::HashSet::new();
    for hop in 0..=5 {
        if !visited.insert(url.to_string()) {
            return Err(failed("redirect loop detected"));
        }
        let route = client.route(&url)?;
        let response = clients
            .get_authorized(
                &url,
                route.as_ref().map(|route| &route.proxy),
                client.allows(&url),
            )
            .await?
            .get(url.clone())
            .header(
                header::ACCEPT,
                match resource {
                    Resource::Image => {
                        "image/png, image/jpeg, image/webp, image/gif, image/svg+xml"
                    }
                    Resource::Json(_) => "application/json",
                },
            )
            .header(header::ACCEPT_ENCODING, "identity")
            .header(
                header::USER_AGENT,
                match resource {
                    Resource::Image => DEFAULT_USER_AGENT,
                    Resource::Json(_) => concat!("Orbit/", env!("CARGO_PKG_VERSION")),
                },
            )
            .send()
            .await
            .map_err(|e| failed(e.without_url()))?;

        if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            if hop == 5 {
                return Err(failed("too many redirects (maximum 5)"));
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| failed("redirect has no valid Location header"))?;

            url = client.redirect_target(&url, location)?;
            continue;
        }

        if !response.status().is_success() {
            return Err(fetch_status_error(response.status(), response.headers()));
        }
        let media = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();

        let accepted = match resource {
            Resource::Image => matches!(
                media,
                "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/svg+xml"
            ),
            Resource::Json(_) => media == "application/json" || media.ends_with("+json"),
        };
        if !accepted {
            return Err(failed(match resource {
                Resource::Image => "响应不是受支持的图片类型",
                Resource::Json(_) => "响应不是 JSON 类型",
            }));
        }

        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
        {
            return Err(failed(match resource {
                Resource::Image => "压缩的图片响应不受支持",
                Resource::Json(_) => "压缩的 JSON 响应不受支持",
            }));
        }

        let (limit, message) = match resource {
            Resource::Image => (
                orbit_model::types::Attachment::MAX_IMAGE_BYTES,
                "图片超过 20 MiB",
            ),
            Resource::Json(limit) => (limit, "JSON 超过大小限制"),
        };
        return read_body_limit(response, limit, message).await;
    }

    Err(failed("too many redirects"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn preview_rejects_private_targets_and_credentials_before_network_io() {
        for url in [
            "http://127.0.0.1/a.png",
            "http://[::1]/a.png",
            "https://localhost/a.png",
            "https://local.test/a.png",
            "https://u:p@example.com/a.png",
            "file:///etc/passwd",
        ] {
            assert!(fetch_image(url).await.is_err(), "{url}");
            assert!(
                WebClient::default().fetch_json(url, 1024).await.is_err(),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn json_resources_enforce_type_size_and_redirect_checks() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (headers, body, limit, expected) in [
            (
                "Content-Type: application/json",
                r#"{"models":[]}"#,
                1024,
                None,
            ),
            (
                "Content-Type: application/json",
                r#"{"models":[]}"#,
                2,
                Some("大小限制"),
            ),
            (
                "Content-Type: text/html",
                "<html></html>",
                1024,
                Some("不是 JSON"),
            ),
            (
                "Content-Type: application/json\r\nContent-Encoding: gzip",
                "compressed",
                1024,
                Some("压缩"),
            ),
            (
                "Location: http://127.0.0.1/private",
                "",
                1024,
                Some("non-public"),
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy =
                reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(!request.contains("authorization:"));
                let status = if headers.starts_with("Location") {
                    "302 Found"
                } else {
                    "200 OK"
                };
                stream.write_all(format!("HTTP/1.1 {status}\r\n{headers}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            });
            let result = WebClient::for_test(Some(&proxy))
                .fetch_json("http://catalog.invalid/api.json", limit)
                .await;
            if let Some(expected) = expected {
                let error = result.unwrap_err().to_string();
                assert!(error.contains(expected), "{error}");
            } else {
                assert_eq!(result.unwrap(), serde_json::json!({"models": []}));
            }
            server.await.unwrap();
        }
    }
}
