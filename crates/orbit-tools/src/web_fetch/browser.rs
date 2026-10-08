//! Optional browser reader for sites that explicitly demand browser verification.
//! The configured proxy is the trusted egress boundary for this path.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use reqwest::{Url, header};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

use super::{MAX_BODY_BYTES, ToolError, checked_url, failed};

pub(super) struct BrowserPage {
    pub url: Url,
    pub title: String,
    pub text: String,
}

pub(super) fn is_browser_challenge(headers: &header::HeaderMap) -> bool {
    headers
        .get("cf-mitigated")
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"challenge"))
}

pub(super) async fn fetch(url: &Url, proxy: &Url) -> Result<BrowserPage, ToolError> {
    if proxy.scheme() != "http" || !proxy.username().is_empty() || proxy.password().is_some() {
        return Err(failed(
            "browser verification requires an unauthenticated HTTP [web].proxy",
        ));
    }

    let host = proxy
        .host_str()
        .ok_or_else(|| failed("web.proxy has no host"))?;
    let port = proxy
        .port_or_known_default()
        .ok_or_else(|| failed("web.proxy has no port"))?;
    let profile = tempfile::tempdir().map_err(failed)?;
    let prefs = format!(
        "user_pref(\"network.proxy.type\", 1);\n\
         user_pref(\"network.proxy.http\", {host:?});\n\
         user_pref(\"network.proxy.http_port\", {port});\n\
         user_pref(\"network.proxy.ssl\", {host:?});\n\
         user_pref(\"network.proxy.ssl_port\", {port});\n\
         user_pref(\"network.proxy.no_proxies_on\", \"\");\n\
         user_pref(\"network.proxy.allow_hijacking_localhost\", true);\n"
    );
    let mut file = tokio::fs::File::create(profile.path().join("user.js"))
        .await
        .map_err(failed)?;
    file.write_all(prefs.as_bytes()).await.map_err(failed)?;
    file.flush().await.map_err(failed)?;
    drop(file);

    let mut launcher = tokio::process::Command::new("/usr/bin/firefox");
    launcher
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", profile.path())
        .env("XDG_CONFIG_HOME", profile.path())
        .env("XDG_CACHE_HOME", profile.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = launcher
        .args(["--headless", "--no-remote", "--profile"])
        .arg(profile.path())
        .args(["--remote-debugging-port", "0"])
        .spawn()
        .map_err(|error| {
            failed(format!(
                "cannot start Firefox for browser verification: {error}"
            ))
        })?;

    let result = async {
        let stderr = child.stderr.take().ok_or_else(|| failed("Firefox stderr unavailable"))?;
        let mut lines = BufReader::new(stderr).lines();
        let endpoint = tokio::time::timeout(Duration::from_secs(20), async {
            while let Some(line) = lines.next_line().await.map_err(failed)? {
                if let Some(endpoint) = line.strip_prefix("WebDriver BiDi listening on ") {
                    return Ok::<String, ToolError>(endpoint.to_owned());
                }
            }

            Err(failed("Firefox exited before WebDriver BiDi started"))
        }).await.map_err(|_| failed("Firefox browser startup timed out"))??;

        if !endpoint.starts_with("ws://127.0.0.1:") && !endpoint.starts_with("ws://[::1]:") {
            return Err(failed("Firefox reported a non-local WebDriver endpoint"));
        }

        let (mut socket, _) = connect_async(format!("{endpoint}/session"))
            .await.map_err(|error| failed(format!("Firefox WebDriver connection failed: {error}")))?;
        command(&mut socket, 1, "session.new", json!({
            "capabilities": {
                "alwaysMatch": { "browserName": "firefox" }
            }
        })).await?;
        let created = command(&mut socket, 2, "browsingContext.create", json!({ "type": "tab" })).await?;
        let context = created["context"].as_str().ok_or_else(|| failed("Firefox did not create a tab"))?;
        command(&mut socket, 3, "browsingContext.navigate", json!({ "context": context, "url": url.as_str(), "wait": "complete" })).await?;
        let evaluated = command(&mut socket, 4, "script.evaluate", json!({
            "expression": "JSON.stringify({title:document.title,text:(document.body?.innerText||'').slice(0,2097153),url:location.href})",
            "target": { "context": context },
            "awaitPromise": false
        })).await?;
        let value = evaluated["result"]["value"].as_str()
            .ok_or_else(|| failed("Firefox returned no readable page text"))?;
        let data: Value = serde_json::from_str(value).map_err(failed)?;
        let final_url = checked_url(data["url"].as_str().ok_or_else(|| failed("Firefox returned no final URL"))?)?;

        if final_url.origin() != url.origin() {
            return Err(failed("browser verification redirected to a different origin"));
        }

        let title = data["title"].as_str().unwrap_or("").to_owned();
        let text = data["text"].as_str().ok_or_else(|| failed("Firefox returned no page body"))?.to_owned();

        if text.len() > MAX_BODY_BYTES {
            return Err(failed("browser page exceeds 2 MiB text limit"));
        }
        if title.contains("请稍候") || title.contains("Just a moment") {
            return Err(failed("Firefox could not complete the site's browser challenge"));
        }

        let _ = socket.close(None).await;

        Ok(BrowserPage { url: final_url, title, text })
    }.await;
    let _ = child.kill().await;

    result
}

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn command(
    socket: &mut Socket,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, ToolError> {
    socket
        .send(Message::Text(
            json!({ "id": id, "method": method, "params": params })
                .to_string()
                .into(),
        ))
        .await
        .map_err(failed)?;

    while let Some(message) = socket.next().await {
        let message = message.map_err(failed)?;
        let Message::Text(text) = message else {
            continue;
        };

        let reply: Value = serde_json::from_str(&text).map_err(failed)?;

        if reply["id"].as_u64() != Some(id) {
            continue;
        }
        if reply["type"] == "error" {
            return Err(failed(format!(
                "Firefox {method} failed: {}",
                reply["message"].as_str().unwrap_or("unknown error")
            )));
        }

        return Ok(reply["result"].clone());
    }

    Err(failed(format!("Firefox closed before {method} completed")))
}
