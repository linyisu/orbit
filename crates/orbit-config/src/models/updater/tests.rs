use super::*;
use crate::test_support::{TestConfig, TestProvider};
use std::time::Duration;

#[tokio::test]
async fn cached_read_never_waits_for_network_or_the_refresh_gate() {
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let mut config = TestConfig::new()
        .auth(root.path())
        .provider(
            TestProvider::new("slow")
                .endpoint(format!("http://{}/v1", listener.local_addr().unwrap()))
                .models(["local-model"])
                .refresh(true),
        )
        .build()
        .unwrap();
    let mut refreshing = config.clone();
    let job = tokio::spawn(async move { refresh(&mut refreshing, false).await });
    let (_socket, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let read = tokio::time::timeout(Duration::from_millis(250), load_cached(&mut config)).await;
    job.abort();
    assert!(
        read.is_ok(),
        "a local catalog read waited for a stalled refresh"
    );
    assert_eq!(
        config.model_options().unwrap()[0].qualified_name(),
        "slow::local-model"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err(),
        "cache-only loading unexpectedly opened a network connection"
    );
}
