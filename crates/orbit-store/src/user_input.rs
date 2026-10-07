use crate::store::Store;
use anyhow::Result;
use orbit_protocol::user_input::{History, Record};
use rusqlite::{OptionalExtension, params};

impl Store {
    pub async fn save_user_input(&self, session: &str, record: &Record) -> Result<()> {
        let session = session.to_owned();
        let id = record.id.clone();
        let payload = serde_json::to_string(record)?;
        self.access(move |conn| {
            conn.execute(
                "INSERT INTO user_inputs(session_id,request_id,payload) VALUES(?1,?2,?3)
                ON CONFLICT(session_id,request_id) DO UPDATE SET payload=excluded.payload",
                params![session, id, payload],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn user_input(&self, session: &str, id: &str) -> Result<Option<Record>> {
        let (session, id) = (session.to_owned(), id.to_owned());
        self.read_access(move |conn| {
            let payload: Option<String> = conn
                .query_row(
                    "SELECT payload FROM user_inputs WHERE session_id=?1 AND request_id=?2",
                    params![session, id],
                    |r| r.get(0),
                )
                .optional()?;
            payload
                .map(|s| serde_json::from_str(&s).map_err(Into::into))
                .transpose()
        })
        .await
    }

    pub async fn user_input_history(&self, session: &str, before: Option<i64>) -> Result<History> {
        let session = session.to_owned();
        self.read_access(move |conn| {
            let mut stmt = conn.prepare("SELECT id,payload FROM user_inputs WHERE session_id=?1 AND (?2 IS NULL OR id<?2) ORDER BY id DESC LIMIT 21")?;
            let mut rows = stmt.query_map(params![session,before], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more = rows.len() > 20;
            rows.truncate(20);
            let next = more.then(||rows.last().unwrap().0);
            let records = rows.into_iter().map(|(_,s)|serde_json::from_str(&s).map_err(Into::into)).collect::<Result<_>>()?;
            Ok(History {before,records,next})
        }).await
    }

    pub async fn interrupt_user_inputs(&self, session: &str) -> Result<()> {
        let session = session.to_owned();
        self.access(move |conn| {
            conn.execute("UPDATE user_inputs SET payload=json_set(payload,'$.status','interrupted') WHERE session_id=?1 AND json_extract(payload,'$.status')='pending'",[session])?;
            Ok(())
        }).await
    }
}

pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS user_inputs (
        id INTEGER PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        request_id TEXT NOT NULL,
        payload TEXT NOT NULL,
        UNIQUE(session_id,request_id)
    ); CREATE INDEX IF NOT EXISTS user_inputs_session ON user_inputs(session_id,id); PRAGMA user_version=25;")?;
    Ok(())
}
