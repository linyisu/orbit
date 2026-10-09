use crate::{entities::workspace_requests as requests, store::Store};
use anyhow::{Result, ensure};
use sea_orm::*;
const UNKNOWN_OUTCOME: &str = "request was reserved but its outcome is unknown; inspect session history before submitting a new request";
#[derive(Debug, PartialEq, Eq)]
pub enum RequestClaim {
    New,
    Accepted,
    Rejected(String),
}
impl Store {
    /// A durable reservation precedes work; an interrupted reservation never authorizes replay.
    pub async fn claim_workspace_request(
        &self,
        session: &str,
        request: &str,
        fingerprint: &str,
    ) -> Result<RequestClaim> {
        ensure!(
            !request.is_empty() && request.len() <= 128,
            "invalid workspace request ID"
        );
        self.write_access(|conn| async move {
            let tx = conn.begin().await?;
            let result = if let Some(previous) =
                requests::Entity::find_by_id((session.to_owned(), request.to_owned()))
                    .one(&tx)
                    .await?
            {
                ensure!(
                    previous.fingerprint == fingerprint,
                    "request ID reused with different input"
                );
                match previous.error {
                    Some(error) => RequestClaim::Rejected(error),
                    None => RequestClaim::Accepted,
                }
            } else {
                requests::ActiveModel {
                    session_id: Set(session.into()),
                    request_id: Set(request.into()),
                    fingerprint: Set(fingerprint.into()),
                    error: Set(Some(UNKNOWN_OUTCOME.into())),
                }
                .insert(&tx)
                .await?;
                RequestClaim::New
            };
            tx.commit().await?;
            Ok(result)
        })
        .await
    }
    pub async fn finish_workspace_request(
        &self,
        session: &str,
        request: &str,
        error: Option<String>,
    ) -> Result<()> {
        self.write_access(|conn| async move {
            let updated = requests::Entity::update_many()
                .col_expr(requests::Column::Error, sea_query::Expr::value(error))
                .filter(requests::Column::SessionId.eq(session))
                .filter(requests::Column::RequestId.eq(request))
                .filter(requests::Column::Error.eq(UNKNOWN_OUTCOME))
                .exec(&conn)
                .await?;
            ensure!(
                updated.rows_affected == 1,
                "workspace request was not reserved or already finalized"
            );
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn receipts_survive_reopening_and_uncertain_requests_are_never_reexecuted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.db");
        let store = Store::open(&path).await.unwrap();
        let writer = store
            .create(
                directory.path().to_string_lossy().into_owned(),
                "mock".into(),
            )
            .await
            .unwrap();
        let id = writer.id().to_owned();
        assert_eq!(
            store
                .claim_workspace_request(&id, "one", "payload")
                .await
                .unwrap(),
            RequestClaim::New
        );
        assert!(matches!(
            store
                .claim_workspace_request(&id, "one", "payload")
                .await
                .unwrap(),
            RequestClaim::Rejected(_)
        ));
        store
            .finish_workspace_request(&id, "one", None)
            .await
            .unwrap();
        drop(writer);
        drop(store);
        let store = Store::open(&path).await.unwrap();
        assert_eq!(
            store
                .claim_workspace_request(&id, "one", "payload")
                .await
                .unwrap(),
            RequestClaim::Accepted
        );
        assert!(
            store
                .claim_workspace_request(&id, "one", "different")
                .await
                .is_err()
        );
        assert!(
            store
                .finish_workspace_request(&id, "one", Some("late".into()))
                .await
                .is_err()
        );
    }
}
