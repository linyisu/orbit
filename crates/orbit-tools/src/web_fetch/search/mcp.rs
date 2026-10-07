//! Stateless calls to hosted search tools. Uses the existing proxy, address,
//! size and timeout policies; no local MCP process or browser is required.
use super::*;
use serde_json::Value;

#[cfg(test)]
mod tests;

fn payload(args: &SearchArguments, provider: SearchProvider) -> Value {
    let (name, arguments) = match provider {
        SearchProvider::Parallel => (
            "web_search",
            json!({"objective": args.query, "search_queries": [&args.query]}),
        ),
        SearchProvider::Exa => (
            "web_search_exa",
            json!({"query": args.query, "numResults": args.max_results}),
        ),
        _ => unreachable!(),
    };
    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}})
}

pub(super) async fn request(
    args: &SearchArguments,
    client: &WebClient,
    provider: SearchProvider,
    endpoint: &str,
) -> Result<SearchPage, Failure> {
    let url = checked_url(endpoint)?;
    let route = client.route(&url)?;
    let response = RequestClients::default()
        .get(&url, route.as_ref().map(|route| &route.proxy))
        .await?
        .post(url.clone())
        .header(
            header::USER_AGENT,
            concat!("Orbit/", env!("CARGO_PKG_VERSION")),
        )
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::ACCEPT_ENCODING, "identity")
        .json(&payload(args, provider))
        .send()
        .await
        .map_err(|error| Failure {
            message: error.without_url().to_string(),
            retry: Some(Duration::from_millis(500)),
            limited: false,
        })?;
    // These endpoints are stateless. Do not replay a POST across redirects.
    if !response.status().is_success() {
        return Err(status_failure(response.status(), response.headers()));
    }
    if response
        .headers()
        .get(header::CONTENT_ENCODING)
        .is_some_and(|v| v != "identity")
    {
        return Err(failed("MCP search response uses unsupported compression").into());
    }
    let envelope = read_envelope(response).await?;
    let result = tool_result(envelope)?;
    let (mut results, mut warnings) = parse_results(&result, provider)?;
    // Reject unsafe/unsupported URLs before judging whether this provider has
    // results, so an invalid-only response falls through to the next provider.
    results = search_results::select(results, 50);
    if results.is_empty() {
        return Err(failed("MCP search returned no readable results").into());
    }
    let (checks, unconfirmed) = validation::results(&args.query, &mut results)?;
    warnings.extend(checks);
    let unsupported: Vec<_> = [
        args.language.as_ref().map(|_| "language"),
        args.region.as_ref().map(|_| "region"),
        args.freshness.as_ref().map(|_| "freshness"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !unsupported.is_empty() {
        warnings.push(format!(
            "{} MCP does not apply these filters: {}. Verify them on result pages.",
            provider.name(),
            unsupported.join(", ")
        ));
    }
    Ok(SearchPage {
        results,
        request_url: url.to_string(),
        response_url: url.to_string(),
        warnings,
        unconfirmed,
    })
}

async fn read_envelope(mut response: reqwest::Response) -> Result<Value, Failure> {
    let sse = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next().unwrap_or("").trim() == "text/event-stream");
    if !sse {
        return serde_json::from_slice(&read_body(response).await?)
            .map_err(|_| failed("invalid MCP JSON response").into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| failed(error.without_url()))?
    {
        if bytes.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(failed("MCP search response exceeds 2 MiB").into());
        }
        bytes.extend_from_slice(&chunk);
        // Stop at the matching response even if the server keeps SSE open.
        if let Some(envelope) = sse_envelope(&String::from_utf8_lossy(&bytes))? {
            return Ok(envelope);
        }
    }
    Err(failed("MCP stream ended without the requested response").into())
}

fn sse_envelope(body: &str) -> Result<Option<Value>, Failure> {
    let normalized = body.replace("\r\n", "\n");
    // Only complete events are parsed; a chunk can stop anywhere, including in
    // a JSON string or a UTF-8 character. Ignore notifications and other IDs.
    for event in normalized.split_inclusive("\n\n") {
        if !event.ends_with("\n\n") {
            continue;
        }
        let data = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let value: Value =
            serde_json::from_str(&data).map_err(|_| failed("invalid MCP SSE data"))?;
        if value["id"] == 1 {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn tool_result(envelope: Value) -> Result<Value, Failure> {
    if envelope["jsonrpc"] != "2.0" || envelope["id"] != 1 {
        return Err(failed("MCP response does not match the request").into());
    }
    if let Some(error) = envelope.get("error") {
        return Err(tool_failure(
            error["message"].as_str().unwrap_or("MCP request failed"),
            error["code"] == 429,
        ));
    }
    let result = envelope
        .get("result")
        .filter(|result| result.is_object())
        .ok_or_else(|| failed("MCP response has no tool result"))?;
    // Exa reports free-tier exhaustion as HTTP 200 without isError=true.
    if result["_meta"]["ai.exa/rateLimited"] == true {
        return Err(tool_failure("MCP free-tier rate limit reached", true));
    }
    if result["isError"] == true {
        let message = text_blocks(result).next().unwrap_or("MCP tool failed");
        return Err(tool_failure(message, false));
    }
    Ok(result.clone())
}

fn tool_failure(message: &str, limited: bool) -> Failure {
    let message = compact(message, 240);
    let lower = message.to_ascii_lowercase();
    let limited = limited
        || [
            "rate limit",
            "rate_limit",
            "too many requests",
            "quota exceeded",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
    Failure {
        message: format!("MCP search failed: {message}"),
        retry: limited.then(|| Duration::from_secs(60)),
        limited,
    }
}

fn text_blocks(result: &Value) -> impl Iterator<Item = &str> {
    result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
}

fn compact(text: &str, limit: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect()
}

fn parse_results(
    result: &Value,
    provider: SearchProvider,
) -> Result<(Vec<SearchResult>, Vec<String>), Failure> {
    if let Some(structured) = result.get("structuredContent")
        && structured["results"].is_array()
    {
        return Ok(structured_results(structured));
    }
    let mut results = Vec::new();
    let mut warnings = Vec::new();
    for text in text_blocks(result) {
        if let Ok(value) = serde_json::from_str::<Value>(text)
            && value["results"].is_array()
        {
            let (items, notices) = structured_results(&value);
            results.extend(items);
            warnings.extend(notices);
        } else if provider == SearchProvider::Exa {
            results.extend(exa_results(text));
        }
    }
    results.truncate(50);
    Ok((results, warnings))
}

fn structured_results(value: &Value) -> (Vec<SearchResult>, Vec<String>) {
    let results = value["results"]
        .as_array()
        .into_iter()
        .flatten()
        .take(50)
        .filter_map(|item| {
            let url = checked_url(item["url"].as_str()?).ok()?.to_string();
            let excerpts = item["excerpts"]
                .as_array()
                .or_else(|| item["highlights"].as_array());
            let snippet = if let Some(excerpts) = excerpts {
                excerpts
                    .iter()
                    .filter_map(Value::as_str)
                    .take(10)
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                item["text"]
                    .as_str()
                    .or_else(|| item["snippet"].as_str())
                    .unwrap_or("")
                    .into()
            };
            Some(SearchResult {
                title: compact(item["title"].as_str().unwrap_or(&url), 512),
                url,
                snippet: compact(&snippet, 2000),
            })
        })
        .collect();
    let warnings = value["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .take(10)
        .filter_map(|warning| warning.as_str().or_else(|| warning["message"].as_str()))
        .map(|warning| compact(warning, 500))
        .collect();
    (results, warnings)
}

fn exa_results(text: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut title = String::new();
    let mut url = String::new();
    let mut snippet = String::new();
    let mut in_snippet = false;
    let finish = |results: &mut Vec<SearchResult>, title: &str, url: &str, snippet: &str| {
        if let Ok(url) = checked_url(url) {
            results.push(SearchResult {
                title: compact(
                    if title.is_empty() || title == "N/A" {
                        url.as_str()
                    } else {
                        title
                    },
                    512,
                ),
                url: url.into(),
                snippet: compact(snippet, 2000),
            });
        }
    };
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Title:") {
            finish(&mut results, &title, &url, &snippet);
            if results.len() >= 50 {
                return results;
            }
            title = value.trim().into();
            url.clear();
            snippet.clear();
            in_snippet = false;
        } else if let Some(value) = line.strip_prefix("URL:") {
            url = value.trim().into();
            in_snippet = false;
        } else if let Some(value) = line
            .strip_prefix("Text:")
            .or_else(|| line.strip_prefix("Highlights:"))
        {
            snippet.push_str(value);
            in_snippet = true;
        } else if line.trim() == "---" {
            in_snippet = false;
        } else if in_snippet && snippet.len() < 16_000 {
            snippet.push('\n');
            snippet.push_str(line);
        }
    }
    finish(&mut results, &title, &url, &snippet);
    results
}
