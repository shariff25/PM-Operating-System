//! The Authority's store in Postgres: Agent Governance's own database, never PM OS's.
//!
//! A record and its effects go in one transaction. The record's primary key is its number,
//! so two writers that built on the same head cannot both commit.

use async_trait::async_trait;
use gov_core::enrol::EnrolKind;
use gov_core::receipt::ChainHead;
use gov_core::revocation::RevocationEntry;
use gov_core::{Digest, Envelope, Id, Scope};
use serde::de::DeserializeOwned;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::types::Json;
use sqlx::Row;

use crate::store::{
    Agent, Effect, EnrolledKey, Gate, IssuedMandate, NamedKey, PendingCode, Person, Published,
    SessionRow, Store, StoreError, StoreResult, StoredRecord,
};

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        StoreError::Other(e.to_string())
    }
}

fn bad(what: &str) -> StoreError {
    StoreError::Other(format!("an unexpected {what} in storage"))
}

fn kind_text(kind: EnrolKind) -> &'static str {
    match kind {
        EnrolKind::Agent => "agent",
        EnrolKind::Gate => "gate",
    }
}

fn stored(r: &PgRow) -> StoreResult<StoredRecord> {
    let envelope: Json<Envelope> = r.try_get("envelope")?;
    let seq: i64 = r.try_get("seq")?;
    let hash: String = r.try_get("hash")?;
    Ok(StoredRecord {
        seq: u64::try_from(seq).map_err(|_| bad("sequence number"))?,
        hash: Digest::try_from(hash).map_err(|_| bad("digest"))?,
        at: r.try_get("at")?,
        envelope: envelope.0,
    })
}

/// Held while its holder lives. Its session is its own, not the pool's, so dropping it
/// closes the session and frees the lock.
pub struct ServingLock(sqlx::PgConnection);

impl ServingLock {
    pub async fn release(self) {
        use sqlx::Connection;
        let _ = self.0.close().await;
    }
}

#[derive(Clone)]
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub async fn connect(url: &str) -> StoreResult<Self> {
        let pool = PgPoolOptions::new().max_connections(8).connect(url).await?;
        Ok(PgStore { pool })
    }

    /// The lock the serving program holds for as long as it runs, and the command that
    /// replaces the admin-record key must get: `None` when another holds it. It is the
    /// database session's, so a crash releases it (review finding GR-194).
    pub async fn serving_lock(&self, scope: &Scope) -> StoreResult<Option<ServingLock>> {
        let mut session = self.pool.acquire().await?;
        let got: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_lock(hashtext('gov-authority:' || current_schema() || ':' || $1))",
        )
        .bind(format!("{}:{}", scope.tenant, scope.env))
        .fetch_one(&mut *session)
        .await?;
        Ok(got.then(|| ServingLock(session.detach())))
    }

    pub fn from_pool(pool: PgPool) -> Self {
        PgStore { pool }
    }

    pub async fn migrate(&self) -> StoreResult<()> {
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.dangerous_set_table_name("_gov_authority_migrations");
        migrator
            .run(&self.pool)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))
    }

    /// One registry entry, by its kind and name.
    async fn entry<T: DeserializeOwned + Send + Unpin>(
        &self,
        scope: &Scope,
        kind: &str,
        name: &str,
    ) -> StoreResult<Option<T>> {
        let row = sqlx::query(
            "SELECT doc FROM authority_registry \
             WHERE tenant = $1 AND env = $2 AND kind = $3 AND name = $4",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(kind)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some(r.try_get::<Json<T>, _>("doc")?.0)),
            None => Ok(None),
        }
    }
}

/// The registry kind and name an effect writes, the agent's owner where it has one, and the
/// document, or `None` for the document when the entry is removed.
fn row(effect: &Effect) -> (String, String, Option<String>, Option<serde_json::Value>) {
    let json = |v: &dyn erased::ToJson| v.to_json();
    match effect {
        Effect::PutPerson(p) => ("person".into(), p.id.to_string(), None, Some(json(p))),
        Effect::PutAgent(a) => (
            "agent".into(),
            a.id.to_string(),
            Some(a.owner.to_string()),
            Some(json(a)),
        ),
        Effect::PutGate(g) => ("gate".into(), g.id.to_string(), None, Some(json(g))),
        Effect::AddKey(e) => ("key".into(), e.key.kid.to_string(), None, Some(json(e))),
        Effect::PutBundle(b) => ("bundle".into(), b.number.to_string(), None, Some(json(b))),
        Effect::PutMandate(m) => (
            "mandate".into(),
            m.id.to_string(),
            Some(m.agent.to_string()),
            Some(json(m)),
        ),
        Effect::AddRevocation(e) => ("revocation".into(), e.seq.to_string(), None, Some(json(e))),
        Effect::PutCode(c) => (
            format!("code-{}", kind_text(c.kind)),
            c.name.to_string(),
            None,
            Some(json(c)),
        ),
        Effect::DropCode(kind, name) => (
            format!("code-{}", kind_text(*kind)),
            name.to_string(),
            None,
            None,
        ),
        Effect::AddLink(link, person) => (
            "link".into(),
            link.as_str().to_string(),
            Some(person.to_string()),
            Some(json(person)),
        ),
        Effect::NameAdminKey(named) => (
            "admin-key".into(),
            "current".into(),
            None,
            Some(json(named)),
        ),
        Effect::PutSession(row) => (
            "session".into(),
            row.id.as_str().to_string(),
            Some(row.person.to_string()),
            Some(json(row)),
        ),
        Effect::DropSession(id) => ("session".into(), id.as_str().to_string(), None, None),
        // Handled by its own statement: it removes by owner, not by name.
        Effect::DropSessionsOf(person) => (
            "session".into(),
            String::new(),
            Some(person.to_string()),
            None,
        ),
    }
}

mod erased {
    /// Any registry entry, as the JSON it is stored as.
    pub trait ToJson {
        fn to_json(&self) -> serde_json::Value;
    }
    impl<T: serde::Serialize> ToJson for T {
        fn to_json(&self) -> serde_json::Value {
            serde_json::to_value(self).expect("a registry entry serialises")
        }
    }
}

#[async_trait]
impl Store for PgStore {
    async fn head(&self, scope: &Scope) -> StoreResult<Option<ChainHead>> {
        let row = sqlx::query(
            "SELECT seq, hash, at, envelope FROM authority_records \
             WHERE tenant = $1 AND env = $2 ORDER BY seq DESC LIMIT 1",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => {
                let s = stored(&r)?;
                Ok(Some(ChainHead {
                    seq: s.seq,
                    hash: s.hash,
                }))
            }
            None => Ok(None),
        }
    }

    async fn commit(
        &self,
        scope: &Scope,
        record: &StoredRecord,
        effects: &[Effect],
    ) -> StoreResult<()> {
        let seq = i64::try_from(record.seq).map_err(|_| bad("sequence number"))?;
        let mut tx = self.pool.begin().await?;
        // The head must be the record before this one. Two writers that both read the same
        // head both pass this check; the primary key then lets one of them in.
        let head: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(seq) FROM authority_records WHERE tenant = $1 AND env = $2",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .fetch_one(&mut *tx)
        .await?;
        if head.unwrap_or(0) + 1 != seq {
            return Err(StoreError::HeadMoved);
        }
        let inserted = sqlx::query(
            "INSERT INTO authority_records (tenant, env, seq, hash, at, envelope) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(seq)
        .bind(record.hash.as_str())
        .bind(record.at)
        .bind(Json(&record.envelope))
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() != 1 {
            return Err(StoreError::HeadMoved);
        }
        // A revocation takes the next number, and no other. The record's own primary key
        // already lets only one of two writers in, so the number read here is the one kept.
        let newest: Option<i64> = sqlx::query_scalar(
            "SELECT MAX((doc ->> 'seq')::bigint) FROM authority_registry \
             WHERE tenant = $1 AND env = $2 AND kind = 'revocation'",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .fetch_one(&mut *tx)
        .await?;
        let mut next =
            u64::try_from(newest.unwrap_or(0)).map_err(|_| bad("revocation number"))? + 1;
        for effect in effects {
            if let Effect::AddRevocation(e) = effect {
                if e.seq != next {
                    return Err(StoreError::HeadMoved);
                }
                next += 1;
            }
        }
        for effect in effects {
            let (kind, name, owner, doc) = row(effect);
            // A key is enrolled once: no replacing. The key id is the row's name and the
            // public key has a unique index of its own, so either being taken fails the
            // insert, and with it the whole transaction.
            if let Effect::DropSessionsOf(_) = effect {
                sqlx::query(
                    "DELETE FROM authority_registry \
                     WHERE tenant = $1 AND env = $2 AND kind = 'session' AND owner = $3",
                )
                .bind(scope.tenant.as_str())
                .bind(scope.env.as_str())
                .bind(&owner)
                .execute(&mut *tx)
                .await?;
                continue;
            }
            if let (Effect::AddKey(_) | Effect::AddLink(..), Some(doc)) = (effect, &doc) {
                let added = sqlx::query(
                    "INSERT INTO authority_registry (tenant, env, kind, name, owner, doc) \
                     VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
                )
                .bind(scope.tenant.as_str())
                .bind(scope.env.as_str())
                .bind(&kind)
                .bind(&name)
                .bind(&owner)
                .bind(Json(doc))
                .execute(&mut *tx)
                .await?;
                if added.rows_affected() != 1 {
                    return Err(StoreError::KeyTaken);
                }
                continue;
            }
            match doc {
                Some(doc) => {
                    sqlx::query(
                        "INSERT INTO authority_registry (tenant, env, kind, name, owner, doc) \
                         VALUES ($1, $2, $3, $4, $5, $6) \
                         ON CONFLICT (tenant, env, kind, name) \
                         DO UPDATE SET owner = EXCLUDED.owner, doc = EXCLUDED.doc",
                    )
                    .bind(scope.tenant.as_str())
                    .bind(scope.env.as_str())
                    .bind(kind)
                    .bind(name)
                    .bind(owner)
                    .bind(Json(doc))
                    .execute(&mut *tx)
                    .await?;
                }
                None => {
                    sqlx::query(
                        "DELETE FROM authority_registry \
                         WHERE tenant = $1 AND env = $2 AND kind = $3 AND name = $4",
                    )
                    .bind(scope.tenant.as_str())
                    .bind(scope.env.as_str())
                    .bind(kind)
                    .bind(name)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn records_after(
        &self,
        scope: &Scope,
        seq: u64,
        limit: usize,
    ) -> StoreResult<Vec<StoredRecord>> {
        let rows = sqlx::query(
            "SELECT seq, hash, at, envelope FROM authority_records \
             WHERE tenant = $1 AND env = $2 AND seq > $3 ORDER BY seq LIMIT $4",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(i64::try_from(seq).map_err(|_| bad("sequence number"))?)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(stored).collect()
    }

    async fn any_person(&self, scope: &Scope) -> StoreResult<bool> {
        let found: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM authority_registry \
             WHERE tenant = $1 AND env = $2 AND kind = 'person' LIMIT 1",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(found.is_some())
    }

    async fn person(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Person>> {
        self.entry(scope, "person", id.as_str()).await
    }

    async fn admin_key(&self, scope: &Scope) -> StoreResult<Option<NamedKey>> {
        self.entry(scope, "admin-key", "current").await
    }

    async fn linked(&self, scope: &Scope, link: &Digest) -> StoreResult<Option<Id>> {
        self.entry(scope, "link", link.as_str()).await
    }

    async fn session(&self, scope: &Scope, id: &Digest) -> StoreResult<Option<SessionRow>> {
        self.entry(scope, "session", id.as_str()).await
    }

    async fn forget_session(&self, scope: &Scope, id: &Digest) -> StoreResult<()> {
        sqlx::query(
            "DELETE FROM authority_registry \
             WHERE tenant = $1 AND env = $2 AND kind = 'session' AND name = $3",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(id.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn touch_session(&self, scope: &Scope, id: &Digest, now: i64) -> StoreResult<()> {
        sqlx::query(
            "UPDATE authority_registry SET doc = jsonb_set(doc, '{last_used}', to_jsonb($4::bigint)) \
             WHERE tenant = $1 AND env = $2 AND kind = 'session' AND name = $3",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(id.as_str())
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn agent(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Agent>> {
        self.entry(scope, "agent", id.as_str()).await
    }

    async fn agents_of(&self, scope: &Scope, owner: &Id) -> StoreResult<Vec<Agent>> {
        let rows = sqlx::query(
            "SELECT doc FROM authority_registry \
             WHERE tenant = $1 AND env = $2 AND kind = 'agent' AND owner = $3 ORDER BY name",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(owner.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get::<Json<Agent>, _>("doc")?.0))
            .collect()
    }

    async fn gate(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Gate>> {
        self.entry(scope, "gate", id.as_str()).await
    }

    async fn key(&self, scope: &Scope, kid: &Id) -> StoreResult<Option<EnrolledKey>> {
        self.entry(scope, "key", kid.as_str()).await
    }

    async fn mandate(&self, scope: &Scope, id: &Id) -> StoreResult<Option<IssuedMandate>> {
        self.entry(scope, "mandate", id.as_str()).await
    }

    async fn mandates(&self, scope: &Scope) -> StoreResult<Vec<IssuedMandate>> {
        let rows = sqlx::query(
            "SELECT doc FROM authority_registry \
             WHERE tenant = $1 AND env = $2 AND kind = 'mandate' ORDER BY name",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get::<Json<IssuedMandate>, _>("doc")?.0))
            .collect()
    }

    async fn revocations_after(
        &self,
        scope: &Scope,
        after: u64,
    ) -> StoreResult<Vec<RevocationEntry>> {
        let after = i64::try_from(after).map_err(|_| bad("revocation number"))?;
        let rows = sqlx::query(
            "SELECT doc FROM authority_registry \
             WHERE tenant = $1 AND env = $2 AND kind = 'revocation' \
               AND (doc ->> 'seq')::bigint > $3 ORDER BY (doc ->> 'seq')::bigint",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(after)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get::<Json<RevocationEntry>, _>("doc")?.0))
            .collect()
    }

    async fn latest_bundle(&self, scope: &Scope) -> StoreResult<Option<Published>> {
        let row = sqlx::query(
            "SELECT doc FROM authority_registry \
             WHERE tenant = $1 AND env = $2 AND kind = 'bundle' \
             ORDER BY (doc ->> 'number')::bigint DESC LIMIT 1",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| Ok(r.try_get::<Json<Published>, _>("doc")?.0))
            .transpose()
    }

    async fn bundle(&self, scope: &Scope, number: u64) -> StoreResult<Option<Published>> {
        self.entry(scope, "bundle", &number.to_string()).await
    }

    async fn keys(&self, scope: &Scope) -> StoreResult<Vec<EnrolledKey>> {
        let rows = sqlx::query(
            "SELECT doc FROM authority_registry \
             WHERE tenant = $1 AND env = $2 AND kind = 'key' ORDER BY name",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get::<Json<EnrolledKey>, _>("doc")?.0))
            .collect()
    }

    async fn code(
        &self,
        scope: &Scope,
        kind: EnrolKind,
        name: &Id,
    ) -> StoreResult<Option<PendingCode>> {
        self.entry(scope, &format!("code-{}", kind_text(kind)), name.as_str())
            .await
    }

    async fn count_wrong_code(
        &self,
        scope: &Scope,
        kind: EnrolKind,
        name: &Id,
    ) -> StoreResult<u32> {
        let wrong: Option<i64> = sqlx::query_scalar(
            "UPDATE authority_registry \
             SET doc = jsonb_set(doc, '{wrong}', to_jsonb((doc->>'wrong')::bigint + 1)) \
             WHERE tenant = $1 AND env = $2 AND kind = $3 AND name = $4 \
             RETURNING (doc->>'wrong')::bigint",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(format!("code-{}", kind_text(kind)))
        .bind(name.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(u32::try_from(wrong.unwrap_or(0)).unwrap_or(u32::MAX))
    }

    async fn take_code(
        &self,
        scope: &Scope,
        kind: EnrolKind,
        name: &Id,
        digest: &Digest,
    ) -> StoreResult<Option<PendingCode>> {
        // One statement: the row is deleted and returned to exactly one caller.
        let row = sqlx::query(
            "DELETE FROM authority_registry \
             WHERE tenant = $1 AND env = $2 AND kind = $3 AND name = $4 \
               AND doc ->> 'digest' = $5 \
             RETURNING doc",
        )
        .bind(scope.tenant.as_str())
        .bind(scope.env.as_str())
        .bind(format!("code-{}", kind_text(kind)))
        .bind(name.as_str())
        .bind(digest.as_str())
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some(r.try_get::<Json<PendingCode>, _>("doc")?.0)),
            None => Ok(None),
        }
    }
}
