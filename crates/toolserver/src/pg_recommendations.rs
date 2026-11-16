//! Postgres side of approval recommendations (`approvals::ApprovalRepo::recommend`), kept
//! apart from `pg.rs`. Same rules as the in-memory store, in the same order.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPool, PgRow};
use sqlx::Row;

use crate::approvals::{Recommend, RecommendError, Recommendation, Status};
use crate::ledger::Reason;
use crate::store::{StoreError, StoreResult};

fn bad(what: &str, value: &str) -> StoreError {
    StoreError(format!("unexpected {what} `{value}` in database"))
}

fn from_row(r: &PgRow) -> StoreResult<Recommendation> {
    let recommend: String = r.try_get("recommend")?;
    let reason: Option<String> = r.try_get("reason")?;
    Ok(Recommendation {
        approval_id: r.try_get("approval_id")?,
        reviewer: r.try_get("reviewer")?,
        recommend: Recommend::parse(&recommend).ok_or_else(|| bad("recommendation", &recommend))?,
        reason: reason
            .map(|s| Reason::parse(&s).ok_or_else(|| bad("reason", &s)))
            .transpose()?,
        created_at: r.try_get("created_at")?,
    })
}

pub async fn recommend(
    pool: &PgPool,
    id: &str,
    reviewer: &str,
    recommend: Recommend,
    reason: Option<Reason>,
    now: DateTime<Utc>,
) -> Result<Recommendation, RecommendError> {
    let mut tx = pool.begin().await.map_err(StoreError::from)?;
    // A shared lock: a concurrent decision (FOR UPDATE) waits for this write, or this one
    // sees the decision's status.
    let row = sqlx::query("SELECT user_handle, status FROM approvals WHERE id = $1 FOR SHARE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(StoreError::from)?;
    let refused = match &row {
        None => Some(RecommendError::NotFound),
        Some(r) => {
            let user: String = r.try_get("user_handle").map_err(StoreError::from)?;
            let status: String = r.try_get("status").map_err(StoreError::from)?;
            if user == reviewer {
                Some(RecommendError::OwnItem)
            } else if Status::parse(&status) != Some(Status::Pending) {
                Some(RecommendError::NotPending)
            } else {
                None
            }
        }
    };
    if let Some(e) = refused {
        tx.rollback().await.map_err(StoreError::from)?;
        return Err(e);
    }
    sqlx::query(
        "INSERT INTO approval_recommendations (approval_id, reviewer, recommend, reason, created_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (approval_id, reviewer)
         DO UPDATE SET recommend = EXCLUDED.recommend, reason = EXCLUDED.reason,
                       created_at = EXCLUDED.created_at",
    )
    .bind(id)
    .bind(reviewer)
    .bind(recommend.as_str())
    .bind(reason.map(Reason::as_str))
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(StoreError::from)?;
    tx.commit().await.map_err(StoreError::from)?;
    Ok(Recommendation {
        approval_id: id.to_string(),
        reviewer: reviewer.to_string(),
        recommend,
        reason,
        created_at: now,
    })
}

pub async fn list(pool: &PgPool, id: &str) -> StoreResult<Vec<Recommendation>> {
    let rows = sqlx::query(
        "SELECT approval_id, reviewer, recommend, reason, created_at
         FROM approval_recommendations WHERE approval_id = $1
         ORDER BY created_at, reviewer",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(from_row).collect()
}

pub async fn counts(pool: &PgPool, ids: &[String]) -> StoreResult<BTreeMap<String, usize>> {
    let rows = sqlx::query(
        "SELECT approval_id, count(*) AS n FROM approval_recommendations
         WHERE approval_id = ANY($1) GROUP BY approval_id",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    let mut out = BTreeMap::new();
    for r in rows {
        let n: i64 = r.try_get("n")?;
        out.insert(r.try_get("approval_id")?, n as usize);
    }
    Ok(out)
}
