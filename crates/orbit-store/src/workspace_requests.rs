use anyhow::{Result, ensure};
use rusqlite::params;

use crate::store::Store;

const UNKNOWN_OUTCOME: &str = "request was reserved but its outcome is unknown; inspect session history before submitting a new request";

#[derive(Debug, PartialEq, Eq)]
pub enum RequestClaim {
    New,
    Accepted,
    Rejected(String),
}

impl Store {
    /// Reserve before starting any work. A crash in the gap leaves an explicit
    /// uncertain result, never permission to run the same mutation again.
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
        let (session, request, fingerprint) = (
            session.to_owned(),
            request.to_owned(),
            fingerprint.to_owned(),
        );
        self.access(move |connection| {
            let transaction = connection.transaction()?;
            let inserted = transaction.execute(
                "INSERT OR IGNORE INTO workspace_requests(session_id,request_id,fingerprint,error) VALUES(?1,?2,?3,?4)",
                params![session, request, fingerprint, UNKNOWN_OUTCOME],
            )?;
            let result = if inserted == 1 {
                RequestClaim::New
            } else {
                let (previous, error): (String, Option<String>) = transaction.query_row(
                    "SELECT fingerprint,error FROM workspace_requests WHERE session_id=?1 AND request_id=?2",
                    params![session, request], |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                ensure!(previous == fingerprint, "request ID reused with different input");
                match error { Some(error) => RequestClaim::Rejected(error), None => RequestClaim::Accepted }
            };
            transaction.commit()?;
            Ok(result)
        }).await
    }

    pub async fn finish_workspace_request(
        &self,
        session: &str,
        request: &str,
        error: Option<String>,
    ) -> Result<()> {
        let (session, request) = (session.to_owned(), request.to_owned());
        self.access(move |connection| {
            ensure!(connection.execute(
                "UPDATE workspace_requests SET error=?3 WHERE session_id=?1 AND request_id=?2 AND error=?4",
                params![session, request, error, UNKNOWN_OUTCOME],
            )? == 1, "workspace request was not reserved or already finalized");
            Ok(())
        }).await
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
                false,
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
