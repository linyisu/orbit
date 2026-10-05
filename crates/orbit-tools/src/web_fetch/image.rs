//! Binary image retrieval for the TUI, using the same DNS pinning and redirect
//! checks as web_fetch. No browser challenge bypass or credential forwarding.
use super::*;

pub async fn fetch_image(value: &str) -> Result<Vec<u8>, ToolError> {
    WebClient::default().fetch_image(value).await
}

impl WebClient {
    pub async fn fetch_image(&self, value: &str) -> Result<Vec<u8>, ToolError> {
        tokio::time::timeout(Duration::from_secs(15), fetch(value, self))
            .await
            .map_err(|_| failed("图片请求超时"))?
    }
}

async fn fetch(value: &str, client: &WebClient) -> Result<Vec<u8>, ToolError> {
    let mut url = checked_url(value)?;
    let mut clients = RequestClients::default();
    let mut visited = std::collections::HashSet::new();
    for hop in 0..=5 {
        if !visited.insert(url.to_string()) {
            return Err(failed("redirect loop detected"));
        }
        let route = client.route(&url)?;
        let response = clients
            .get(&url, route.as_ref().map(|route| &route.proxy))
            .await?
            .get(url.clone())
            .header(
                header::ACCEPT,
                "image/png, image/jpeg, image/webp, image/gif, image/svg+xml",
            )
            .header(header::ACCEPT_ENCODING, "identity")
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

            url = redirect_target(&url, location)?;
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

        if !matches!(
            media,
            "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/svg+xml"
        ) {
            return Err(failed("响应不是受支持的图片类型"));
        }

        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
        {
            return Err(failed("压缩的图片响应不受支持"));
        }

        return read_body_limit(
            response,
            orbit_model::types::Attachment::MAX_IMAGE_BYTES,
            "图片超过 20 MiB",
        )
        .await;
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
        }
    }
}
