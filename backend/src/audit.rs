//! Append-only audit trail of who changed what.

use diesel::prelude::*;
use diesel::PgConnection;
use uuid::Uuid;

use crate::models::NewAuditRow;
use crate::schema::audit_log;

/// Append one audit entry. Call inside the same transaction as the change it
/// records so the log never disagrees with the data.
pub fn record(
    conn: &mut PgConnection,
    actor_id: Uuid,
    org_id: Option<Uuid>,
    project_id: Option<Uuid>,
    action: &str,
    detail: serde_json::Value,
) -> QueryResult<()> {
    diesel::insert_into(audit_log::table)
        .values(NewAuditRow {
            actor_id: Some(actor_id),
            org_id,
            project_id,
            action,
            detail,
        })
        .execute(conn)
        .map(|_| ())
}
