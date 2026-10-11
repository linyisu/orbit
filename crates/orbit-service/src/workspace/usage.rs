//! Read-only usage inspection without persistence or session-actor HTTP waits.
mod providers;

use anyhow::{Context, Result, bail, ensure};
use orbit_auth::{AuthStore, UsageAuthorization, UsageProvider};
use orbit_model::types::ModelConfig;
use orbit_protocol::usage::{Account, Session, Snapshot};
use orbit_store::store::Store;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) async fn snapshot(
    config: &ModelConfig,
    store: &Store,
    session: &str,
    persisted: bool,
) -> Snapshot {
    let (account, session) =
        tokio::join!(account(config), session_usage(store, session, persisted));
    Snapshot {
        model: config.model.clone(),
        account,
        session,
    }
}

async fn session_usage(store: &Store, id: &str, persisted: bool) -> Session {
    if !persisted {
        return Session::default();
    }
    match store.session_usage(id).await {
        Ok(usage) => usage,
        Err(_) => Session {
            error: Some("无法读取会话用量，请重试".into()),
            ..Default::default()
        },
    }
}

async fn account(config: &ModelConfig) -> Account {
    let mut account = Account {
        identity: Sha256::digest(
            serde_json::to_vec(&(&config.base_url, &config.credential, &config.api_key))
                .expect("serializable usage source"),
        )
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect(),
        name: config
            .credential
            .as_ref()
            .map(|credential| credential.name.clone())
            .unwrap_or_else(|| "配置的 API Key".into()),
        ..Default::default()
    };
    let result = tokio::time::timeout(QUERY_TIMEOUT, async {
        let authorization = if let Some(credential) = &config.credential {
            AuthStore::new(&credential.directory)?
                .resolve_usage(&credential.name)
                .await?
        } else if let Some(key) = &config.api_key {
            orbit_auth::usage_api_key(&config.base_url, key)?
        } else {
            None
        };
        let Some(authorization) = authorization else {
            account.note = Some("此认证尚不支持查询账号额度；可切换到会话用量。".into());
            return Ok::<_, anyhow::Error>(());
        };
        account.provider = authorization.provider.label().into();
        account.name = authorization.account.clone();
        let payload = fetch(&authorization).await?;
        providers::parse(authorization.provider, &payload, &mut account)?;
        account.fetched_at = Some(now());
        Ok(())
    })
    .await;
    account.error = match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("额度查询超时，请稍后刷新".into()),
    };
    account
}

async fn fetch(authorization: &UsageAuthorization) -> Result<Value> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(12))
        .user_agent("orbit")
        .build()
        .context("无法初始化额度查询")?;
    if authorization.provider == UsageProvider::Antigravity {
        let summary = query(&client, authorization, authorization.provider.endpoint()).await;
        if let Ok(payload) = &summary
            && providers::parse(UsageProvider::Antigravity, payload, &mut Account::default())
                .is_ok()
        {
            return Ok(payload.clone());
        }
        // The model catalog is only a fallback when shared quota is unavailable.
        // Its per-model counters must not be presented as additional plan limits.
        let catalog = query(
            &client,
            authorization,
            "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels",
        )
        .await;
        ensure!(
            summary.is_ok() || catalog.is_ok(),
            "Antigravity 额度服务暂不可用，请稍后刷新"
        );
        return Ok(
            serde_json::json!({"summary": summary.ok(), "models": catalog.ok().and_then(|value| value.get("models").cloned())}),
        );
    }
    query(&client, authorization, authorization.provider.endpoint()).await
}

async fn query(
    client: &reqwest::Client,
    authorization: &UsageAuthorization,
    endpoint: &'static str,
) -> Result<Value> {
    let request = match authorization.provider {
        UsageProvider::Gemini | UsageProvider::Antigravity => client
            .post(endpoint)
            .json(&serde_json::json!({"project": authorization.auth.project_id})),
        _ => client.get(endpoint),
    };
    let response = request
        .headers(authorization.auth.headers.clone())
        .send()
        .await;
    let mut response = match response {
        Ok(response) => response,
        Err(error) if error.is_timeout() => bail!("额度查询超时，请稍后刷新"),
        Err(_) => bail!("无法连接额度服务，请检查网络后刷新"),
    };
    let status = response.status();
    ensure!(
        status.is_success(),
        match status.as_u16() {
            401 => "额度认证已失效，请重新登录",
            403 => "该账号没有额度查询权限",
            429 => "额度查询过于频繁，请稍后刷新",
            _ => "额度服务暂不可用，请稍后刷新",
        }
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("额度响应读取失败"))?
    {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= MAX_RESPONSE_BYTES,
            "额度响应过大"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("额度服务返回了无法识别的数据")
}

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_agent::events::AgentEvent;
    use orbit_model::types::{StreamEvent, Usage};

    #[tokio::test]
    async fn usage_inspection_does_not_persist_a_draft_and_missing_tokens_stay_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("orbit.db"))
            .await
            .unwrap();
        let config = ModelConfig::default();
        let result = snapshot(&config, &store, "draft-not-in-database", false).await;
        assert!(result.account.note.is_some());
        assert!(result.session.input.is_none());
        assert!(store.list().await.unwrap().is_empty());

        let mut writer = store
            .create(
                directory.path().to_string_lossy().into_owned(),
                "test".into(),
            )
            .await
            .unwrap();
        writer.begin("hello".into(), "test".into()).await.unwrap();
        writer
            .append(vec![AgentEvent::Model(StreamEvent::Completed {
                finish_reason: None,
                usage: Some(Usage {
                    prompt_tokens: Some(100),
                    completion_tokens: Some(20),
                    total_tokens: Some(120),
                    cached_input_tokens: None,
                    cache_write_input_tokens: Some(0),
                }),
            })])
            .await
            .unwrap();
        writer
            .append(vec![
                AgentEvent::RequestMetrics {
                    profile: "test".into(),
                    estimated_input: 100,
                    retries: None,
                    elapsed_ms: 1,
                    usage: Some(Usage {
                        prompt_tokens: Some(100),
                        completion_tokens: Some(20),
                        total_tokens: None,
                        cached_input_tokens: None,
                        cache_write_input_tokens: Some(0),
                    }),
                },
                AgentEvent::TurnStarted { turn: 2 },
                AgentEvent::RequestMetrics {
                    profile: "test".into(),
                    estimated_input: 200,
                    retries: None,
                    elapsed_ms: 1,
                    usage: Some(Usage {
                        prompt_tokens: Some(200),
                        completion_tokens: Some(30),
                        total_tokens: None,
                        cached_input_tokens: None,
                        cache_write_input_tokens: Some(0),
                    }),
                },
            ])
            .await
            .unwrap();
        // Cross multiple storage pages without counting the completion and
        // receipt twice, then include a failed request without token evidence.
        for turn in 3..=22 {
            let usage = Usage {
                prompt_tokens: Some(100),
                completion_tokens: Some(20),
                total_tokens: None,
                cached_input_tokens: None,
                cache_write_input_tokens: Some(0),
            };
            writer
                .append(vec![
                    AgentEvent::TurnStarted { turn },
                    AgentEvent::Model(StreamEvent::Completed {
                        finish_reason: None,
                        usage: Some(usage.clone()),
                    }),
                    AgentEvent::RequestMetrics {
                        profile: "test".into(),
                        estimated_input: 100,
                        retries: None,
                        elapsed_ms: 1,
                        usage: Some(usage),
                    },
                ])
                .await
                .unwrap();
        }
        writer
            .finish("completed", vec![], Some(vec![]))
            .await
            .unwrap();
        writer
            .begin("second run".into(), "test".into())
            .await
            .unwrap();
        writer
            .append(vec![AgentEvent::RequestMetrics {
                profile: "test".into(),
                estimated_input: 100,
                retries: None,
                elapsed_ms: 1,
                usage: None,
            }])
            .await
            .unwrap();
        writer.finish("failed", vec![], None).await.unwrap();
        let result = session_usage(&store, writer.id(), true).await;
        assert_eq!(result.runs, 2);
        assert_eq!(result.runs_with_usage, 1);
        assert_eq!(result.requests, 23);
        assert_eq!(result.input, Some(2300));
        assert_eq!(result.output, Some(450));
        assert_eq!(result.cached_input, None);
        assert_eq!(result.cache_write, Some(0));
    }
}
