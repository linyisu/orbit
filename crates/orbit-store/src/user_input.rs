use crate::{entities::user_inputs as inputs, store::Store};
use anyhow::Result;
use orbit_protocol::user_input::{History, Record};
use sea_orm::*;

impl Store {
    pub async fn save_user_input(&self, session: &str, record: &Record) -> Result<()> {
        let row = inputs::ActiveModel {
            session_id: Set(session.into()),
            request_id: Set(record.id.clone()),
            payload: Set(serde_json::to_string(record)?),
            ..Default::default()
        };
        self.write_access(|conn| async move {
            inputs::Entity::insert(row)
                .on_conflict(
                    sea_query::OnConflict::columns([
                        inputs::Column::SessionId,
                        inputs::Column::RequestId,
                    ])
                    .update_column(inputs::Column::Payload)
                    .to_owned(),
                )
                .exec(&conn)
                .await?;
            Ok(())
        })
        .await
    }
    pub async fn user_input(&self, session: &str, id: &str) -> Result<Option<Record>> {
        self.read_access(|conn| async move {
            inputs::Entity::find()
                .filter(inputs::Column::SessionId.eq(session))
                .filter(inputs::Column::RequestId.eq(id))
                .one(&conn)
                .await?
                .map(|row| serde_json::from_str(&row.payload).map_err(Into::into))
                .transpose()
        })
        .await
    }
    pub async fn user_input_history(&self, session: &str, before: Option<i64>) -> Result<History> {
        self.read_access(|conn| async move {
            let mut query = inputs::Entity::find().filter(inputs::Column::SessionId.eq(session));
            if let Some(before) = before {
                query = query.filter(inputs::Column::Id.lt(before));
            }
            let mut rows = query
                .order_by_desc(inputs::Column::Id)
                .limit(21)
                .all(&conn)
                .await?;
            let more = rows.len() > 20;
            rows.truncate(20);
            let next = more.then(|| rows.last().unwrap().id);
            let records = rows
                .into_iter()
                .map(|row| serde_json::from_str(&row.payload).map_err(Into::into))
                .collect::<Result<_>>()?;
            Ok(History {
                before,
                records,
                next,
            })
        })
        .await
    }
    pub async fn interrupt_user_inputs(&self, session: &str) -> Result<()> {
        self.write_access(|conn| async move {
            let payload = sea_query::Expr::col(inputs::Column::Payload);
            let status = sea_query::Func::cust(sea_query::Alias::new("json_extract"))
                .args([payload.clone().into(), sea_query::Expr::value("$.status")]);
            let interrupted = sea_query::Func::cust(sea_query::Alias::new("json_set")).args([
                payload.into(),
                sea_query::Expr::value("$.status"),
                sea_query::Expr::value("interrupted"),
            ]);
            inputs::Entity::update_many()
                .col_expr(inputs::Column::Payload, interrupted.into())
                .filter(inputs::Column::SessionId.eq(session))
                .filter(sea_query::Expr::expr(status).eq("pending"))
                .exec(&conn)
                .await?;
            Ok(())
        })
        .await
    }
}
