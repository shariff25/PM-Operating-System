//! Slice G4a: the Authority's registry, enrolment and admin records, on both stores.

use std::collections::BTreeSet;
use std::str::FromStr;

use async_trait::async_trait;
use gov_authority::pg::PgStore;
use gov_authority::store::StoreResult;
use gov_authority::{
    Agent, Authority, AuthorityError, Effect, EnrolledKey, Gate, MemoryStore, PendingCode, Person,
    Role, Status, Store, StoreError, StoredRecord, CODE_SECS, WRONG_CODES,
};
use gov_core::admin::{follow, AdminAction, AdminRecord, SubjectKind};
use gov_core::enrol::{code_digest, code_names_root, EnrolKind, EnrolRequest};
use gov_core::receipt::ChainHead;
use gov_core::{open, seal, Envelope, Id, KeySet, PublicKey, Scope, SigningKey};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, Connection};

const NOW: i64 = 1_800_000_000;

fn id(s: &str) -> Id {
    Id::new(s).unwrap()
}
fn scope() -> Scope {
    Scope::new("acme", "prod").unwrap()
}
fn key(kid: &str, seed: u8) -> SigningKey {
    SigningKey::from_seed(id(kid), &[seed; 32]).unwrap()
}
fn admin_key() -> SigningKey {
    key("authority-admin-1", 60)
}
fn root() -> PublicKey {
    key("authority-root-1", 61).public()
}

fn authority<S: Store>(store: S) -> Authority<S> {
    Authority::new(store, scope(), id("authority"), admin_key(), root())
        .with_online_keys(mandates_key(), leases_key())
}
fn mandates_key() -> SigningKey {
    key("authority:mandates-1", 71)
}
fn leases_key() -> SigningKey {
    key("authority:leases-1", 72)
}

/// A Postgres store in a fresh schema of Agent Governance's own test database. Needs
/// `GOV_TEST_DATABASE_URL`; skipped locally without it and fails in CI (`CI` set).
struct TestPg {
    store: Option<PgStore>,
    base: PgConnectOptions,
    schema: String,
}
impl TestPg {
    async fn new() -> Option<Self> {
        let Ok(url) = std::env::var("GOV_TEST_DATABASE_URL") else {
            assert!(
                std::env::var_os("CI").is_none(),
                "GOV_TEST_DATABASE_URL must be set in CI so the Postgres tests run"
            );
            eprintln!("skipping Postgres case: GOV_TEST_DATABASE_URL is not set");
            return None;
        };
        let base = PgConnectOptions::from_str(&url).expect("GOV_TEST_DATABASE_URL parses");
        let schema = format!("a_{}", uuid::Uuid::new_v4().simple());
        let mut admin = base.connect().await.expect("connect");
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut admin)
            .await
            .expect("create schema");
        admin.close().await.ok();
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(base.clone().options([("search_path", schema.as_str())]))
            .await
            .expect("connect to test schema");
        let store = PgStore::from_pool(pool);
        store.migrate().await.expect("migrations apply");
        Some(TestPg {
            store: Some(store),
            base,
            schema,
        })
    }
    async fn drop_schema(self) {
        let mut admin = self.base.connect().await.expect("connect");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA {} CASCADE",
            self.schema
        )))
        .execute(&mut admin)
        .await
        .ok();
    }
}

macro_rules! both_stores {
    ($name:ident, $body:ident) => {
        #[tokio::test]
        async fn $name() {
            $body(MemoryStore::default()).await;
            if let Some(mut pg) = TestPg::new().await {
                $body(pg.store.take().unwrap()).await;
                pg.drop_schema().await;
            }
        }
    };
}

/// Every record in the chain, opened and checked link by link.
async fn chain<S: Store>(a: &Authority<S>) -> Vec<AdminRecord> {
    let keys = KeySet::new([a.admin_public()]);
    let mut head: Option<ChainHead> = None;
    let mut out = Vec::new();
    for stored in a.store().records_after(&scope(), 0, 10_000).await.unwrap() {
        let opened = open::<AdminRecord>(&stored.envelope, &keys, &scope()).unwrap();
        assert_eq!((opened.doc.seq, &opened.hash), (stored.seq, &stored.hash));
        head = Some(follow(head.as_ref(), &opened).unwrap());
        out.push(opened.doc);
    }
    out
}

fn request(
    signer: &SigningKey,
    kind: EnrolKind,
    name: &str,
    key: PublicKey,
    code: &str,
) -> Envelope {
    seal(
        &EnrolRequest {
            v: 1,
            tenant: id("acme"),
            env: id("prod"),
            authority: id("authority"),
            kind,
            name: id(name),
            key,
            code: code_digest(code),
        },
        signer,
    )
    .unwrap()
}

/// An installation with one admin, one owner, an agent and a gate registered.
async fn installed<S: Store>(store: S) -> Authority<S> {
    let a = authority(store);
    let ana = id("ana");
    a.bootstrap(&ana, NOW).await.unwrap();
    a.add_person(&ana, &id("ben"), Role::Owner, NOW)
        .await
        .unwrap();
    a.register_agent(
        &ana,
        &id("refund-bot"),
        &id("ben"),
        BTreeSet::from([id("triage")]),
        NOW,
    )
    .await
    .unwrap();
    a.register_gate(&ana, &id("gate-1"), NOW).await.unwrap();
    a
}

async fn who_may_act<S: Store>(store: S) {
    let a = authority(store);
    let (ana, ben, eve) = (id("ana"), id("ben"), id("eve"));
    // Before anyone is known, nobody can act; the first admin is named once.
    assert!(matches!(
        a.register_gate(&ana, &id("gate-1"), NOW).await,
        Err(AuthorityError::NotAllowed)
    ));
    a.bootstrap(&ana, NOW).await.unwrap();
    assert!(matches!(
        a.bootstrap(&eve, NOW).await,
        Err(AuthorityError::Exists(_))
    ));
    a.add_person(&ana, &ben, Role::Owner, NOW).await.unwrap();
    // An owner is not an admin, and a stranger is nobody.
    for actor in [&ben, &eve] {
        assert!(matches!(
            a.add_person(actor, &id("mallory"), Role::Admin, NOW).await,
            Err(AuthorityError::NotAllowed)
        ));
        assert!(matches!(
            a.register_gate(actor, &id("gate-9"), NOW).await,
            Err(AuthorityError::NotAllowed)
        ));
        assert!(matches!(
            a.offer_enrolment(actor, EnrolKind::Gate, &id("gate-1"), NOW)
                .await,
            Err(AuthorityError::NotAllowed)
        ));
    }
    // An agent needs an owner who is here; names are not reused.
    assert!(matches!(
        a.register_agent(&ana, &id("bot"), &eve, BTreeSet::new(), NOW)
            .await,
        Err(AuthorityError::Unknown(_))
    ));
    a.register_agent(&ana, &id("bot"), &ben, BTreeSet::new(), NOW)
        .await
        .unwrap();
    assert!(matches!(
        a.register_agent(&ana, &id("bot"), &ben, BTreeSet::new(), NOW)
            .await,
        Err(AuthorityError::Exists(_))
    ));
    assert!(matches!(
        a.add_person(&ana, &ben, Role::Admin, NOW).await,
        Err(AuthorityError::Exists(_))
    ));

    // Everything done is in the chain, in order; nothing refused is.
    let records = chain(&a).await;
    let actions: Vec<AdminAction> = records.iter().map(|r| r.action).collect();
    assert_eq!(
        actions,
        vec![
            AdminAction::PersonAdded,
            AdminAction::PersonAdded,
            AdminAction::AgentRegistered
        ]
    );
    // Nobody is signed in yet: no record reads as a person's proven act (GR-111).
    assert!(records.iter().all(|r| !r.actor.verified));
    assert!(records.iter().all(|r| r.authority == id("authority")));
}
both_stores!(only_an_admin_acts_and_every_act_is_recorded, who_may_act);

async fn enrolment<S: Store>(store: S) {
    let a = installed(store).await;
    let ana = id("ana");
    let agent = key("refund-bot:k1", 9);

    // No code was made for this name: nothing to enrol with.
    let early = request(
        &agent,
        EnrolKind::Agent,
        "refund-bot",
        agent.public(),
        "anything",
    );
    assert!(matches!(
        a.enrol("anything", &early, NOW).await,
        Err(AuthorityError::Refused)
    ));
    // A code cannot be made for a name nobody registered.
    assert!(matches!(
        a.offer_enrolment(&ana, EnrolKind::Agent, &id("ghost"), NOW)
            .await,
        Err(AuthorityError::Unknown(_))
    ));

    let code = a
        .offer_enrolment(&ana, EnrolKind::Agent, &id("refund-bot"), NOW)
        .await
        .unwrap();
    // Only the code's digest is kept.
    let pending = a
        .store()
        .code(&scope(), EnrolKind::Agent, &id("refund-bot"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.digest, code_digest(&code));
    assert!(!serde_json::to_string(&pending).unwrap().contains(&code));

    // The agent enrols its key with the code, and can check the root key it is handed.
    let good = request(
        &agent,
        EnrolKind::Agent,
        "refund-bot",
        agent.public(),
        &code,
    );
    let answered = a.enrol(&code, &good, NOW + 60).await.unwrap();
    assert_eq!(answered, root());
    // The code, handed over by the admin, says which root key that must be (GR-115).
    assert!(code_names_root(&code, &answered));
    assert!(!code_names_root(
        &code,
        &key("an-impostor-root", 62).public()
    ));
    let stored = a
        .store()
        .key(&scope(), &id("refund-bot:k1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (stored.kind, stored.name, stored.key),
        (EnrolKind::Agent, id("refund-bot"), agent.public())
    );
    // One use: the same code and request again enrol nothing.
    assert!(matches!(
        a.enrol(&code, &good, NOW + 61).await,
        Err(AuthorityError::Refused)
    ));

    // The record says whose code it was, which key, and for which agent.
    let records = chain(&a).await;
    let last = records.last().unwrap();
    assert_eq!(last.action, AdminAction::KeyEnrolled);
    assert_eq!(
        (last.subject.kind, &last.subject.id),
        (SubjectKind::Key, &id("refund-bot:k1"))
    );
    assert_eq!(last.about.as_ref().unwrap().id, id("refund-bot"));
    assert_eq!(last.actor.id, ana);
    assert!(last.value.is_some());
    assert_eq!(
        records[records.len() - 2].action,
        AdminAction::EnrolmentOffered
    );
}
both_stores!(a_key_is_enrolled_with_a_one_time_code, enrolment);

/// A request to enrol, made for a given code.
type MakeRequest<'a> = Box<dyn Fn(&str) -> Envelope + 'a>;

/// A fresh code for the gate.
async fn offer<S: Store>(a: &Authority<S>) -> String {
    a.offer_enrolment(&id("ana"), EnrolKind::Gate, &id("gate-1"), NOW)
        .await
        .unwrap()
}
/// Whether a code is waiting for the gate.
async fn pending<S: Store>(a: &Authority<S>) -> bool {
    a.store()
        .code(&scope(), EnrolKind::Gate, &id("gate-1"))
        .await
        .unwrap()
        .is_some()
}
async fn nothing_enrolled<S: Store>(a: &Authority<S>) {
    assert!(a.store().keys(&scope()).await.unwrap().is_empty());
}

async fn enrolment_attacks<S: Store>(store: S) {
    let a = installed(store).await;
    let ana = id("ana");
    let gate = key("gate-1:k1", 20);
    let refused = |r: Result<_, AuthorityError>| assert!(matches!(r, Err(AuthorityError::Refused)));

    // The digest is what the Authority stores. Presenting it enrols nothing (GR-107).
    let code = offer(&a).await;
    let good = request(&gate, EnrolKind::Gate, "gate-1", gate.public(), &code);
    refused(a.enrol(code_digest(&code).as_str(), &good, NOW).await);
    assert!(pending(&a).await, "a wrong guess does not use the code up");

    // Wrong guesses are counted, and the fifth cancels the code: the right one is then too
    // late. The cancellation is recorded, with the Authority itself as its actor: an
    // enrolment an admin offered was stopped, and nobody decided it (GR-117).
    for _ in 1..WRONG_CODES {
        refused(a.enrol("not-the-code", &good, NOW).await);
    }
    assert!(!pending(&a).await);
    refused(a.enrol(&code, &good, NOW).await);
    nothing_enrolled(&a).await;
    let last = chain(&a).await.pop().unwrap();
    assert_eq!(
        (last.action, &last.subject.id, &last.actor.id),
        (
            AdminAction::EnrolmentCancelled,
            &id("gate-1"),
            &id("authority")
        )
    );

    // The right code with a request that does not hold: refused, and the code is spent, so
    // the honest request that follows is too late and an admin must make another.
    let stranger = key("stranger-k1", 99);
    let cases: Vec<(&str, MakeRequest)> = vec![
        // Signed by another key than the one it asks to enrol.
        (
            "another signer",
            Box::new(|c| request(&stranger, EnrolKind::Gate, "gate-1", gate.public(), c)),
        ),
        // For another Authority.
        (
            "another authority",
            Box::new(|c| {
                seal(
                    &EnrolRequest {
                        v: 1,
                        tenant: id("acme"),
                        env: id("prod"),
                        authority: id("someone-else"),
                        kind: EnrolKind::Gate,
                        name: id("gate-1"),
                        key: gate.public(),
                        code: code_digest(c),
                    },
                    &gate,
                )
                .unwrap()
            }),
        ),
        // For another tenant.
        (
            "another tenant",
            Box::new(|c| {
                seal(
                    &EnrolRequest {
                        v: 1,
                        tenant: id("other"),
                        env: id("prod"),
                        authority: id("authority"),
                        kind: EnrolKind::Gate,
                        name: id("gate-1"),
                        key: gate.public(),
                        code: code_digest(c),
                    },
                    &gate,
                )
                .unwrap()
            }),
        ),
        // Signed over another code's digest.
        (
            "another code signed",
            Box::new(|_| {
                request(
                    &gate,
                    EnrolKind::Gate,
                    "gate-1",
                    gate.public(),
                    "some-other-code",
                )
            }),
        ),
    ];
    for (what, make) in &cases {
        let code = offer(&a).await;
        refused(a.enrol(&code, &make(&code), NOW).await);
        assert!(!pending(&a).await, "{what}: the code is spent");
        let honest = request(&gate, EnrolKind::Gate, "gate-1", gate.public(), &code);
        refused(a.enrol(&code, &honest, NOW).await);
        nothing_enrolled(&a).await;
    }

    // A code is for one name and one kind: the agent's code does not enrol the gate, and a
    // request naming the agent with the gate's code finds no such code.
    let agent_code = a
        .offer_enrolment(&ana, EnrolKind::Agent, &id("refund-bot"), NOW)
        .await
        .unwrap();
    let gate_code = offer(&a).await;
    let crossed = request(&gate, EnrolKind::Gate, "gate-1", gate.public(), &agent_code);
    refused(a.enrol(&agent_code, &crossed, NOW).await);
    let as_agent = request(
        &gate,
        EnrolKind::Agent,
        "refund-bot",
        gate.public(),
        &gate_code,
    );
    refused(a.enrol(&gate_code, &as_agent, NOW).await);
    nothing_enrolled(&a).await;

    // A code is good for fifteen minutes.
    let code = offer(&a).await;
    let late = request(&gate, EnrolKind::Gate, "gate-1", gate.public(), &code);
    refused(a.enrol(&code, &late, NOW + CODE_SECS).await);
    nothing_enrolled(&a).await;

    // A key's id is under the name it is for, so nobody takes an id another will want
    // (GR-116): not a bare id, not another gate's, not the name with nothing after it.
    for kid in ["k1", "gate-2:k1", "gate-1:", "gate-1-k1"] {
        let odd = SigningKey::from_seed(id(kid), &[23; 32]).unwrap();
        let code = offer(&a).await;
        let asked = request(&odd, EnrolKind::Gate, "gate-1", odd.public(), &code);
        refused(a.enrol(&code, &asked, NOW).await);
    }
    nothing_enrolled(&a).await;

    // A gate disabled after its code was made: the code enrols nothing (GR-118).
    a.register_gate(&ana, &id("gate-3"), NOW).await.unwrap();
    let g3 = key("gate-3:k1", 24);
    let code3 = a
        .offer_enrolment(&ana, EnrolKind::Gate, &id("gate-3"), NOW)
        .await
        .unwrap();
    a.disable_gate(&ana, &id("gate-3"), NOW).await.unwrap();
    // Disabling the gate dropped its code.
    assert!(a
        .store()
        .code(&scope(), EnrolKind::Gate, &id("gate-3"))
        .await
        .unwrap()
        .is_none());
    let asked = request(&g3, EnrolKind::Gate, "gate-3", g3.public(), &code3);
    refused(a.enrol(&code3, &asked, NOW).await);
    nothing_enrolled(&a).await;
    // A key id is enrolled once, and so is a key: a second key under the same id is
    // refused, and so is the same key under a second id, whoever asks.
    let code = offer(&a).await;
    let first = request(&gate, EnrolKind::Gate, "gate-1", gate.public(), &code);
    a.enrol(&code, &first, NOW).await.unwrap();
    let same_id = SigningKey::from_seed(id("gate-1:k1"), &[21; 32]).unwrap();
    let code = offer(&a).await;
    let again = request(&same_id, EnrolKind::Gate, "gate-1", same_id.public(), &code);
    refused(a.enrol(&code, &again, NOW).await);
    let same_key = SigningKey::from_seed(id("gate-1:k2"), &[20; 32]).unwrap();
    let code = offer(&a).await;
    let again = request(
        &same_key,
        EnrolKind::Gate,
        "gate-1",
        same_key.public(),
        &code,
    );
    refused(a.enrol(&code, &again, NOW).await);
    assert_eq!(a.store().keys(&scope()).await.unwrap().len(), 1);

    // Nothing that was refused left a "key enrolled" record.
    let enrolled = chain(&a)
        .await
        .iter()
        .filter(|r| r.action == AdminAction::KeyEnrolled)
        .count();
    assert_eq!(enrolled, 1);

    // Last, since it writes a made-up record to put a code back: were a code still there for a disabled gate (the gate disabled in the instant
    // after the code was taken), enrolling checks the gate again and refuses.
    let records = a.store().records_after(&scope(), 0, 1000).await.unwrap();
    let last = records.last().unwrap();
    let put_back = StoredRecord {
        seq: last.seq + 1,
        ..last.clone()
    };
    let lingering = Effect::PutCode(PendingCode {
        kind: EnrolKind::Gate,
        name: id("gate-3"),
        digest: code_digest(&code3),
        expires_at: NOW + CODE_SECS,
        by: ana.clone(),
        wrong: 0,
    });
    a.store()
        .commit(&scope(), &put_back, &[lingering])
        .await
        .unwrap();
    refused(a.enrol(&code3, &asked, NOW).await);
    assert_eq!(a.store().keys(&scope()).await.unwrap().len(), 1);
}
both_stores!(what_does_not_enrol_a_key, enrolment_attacks);

async fn offboarding<S: Store>(store: S) {
    let a = installed(store).await;
    let (ana, ben) = (id("ana"), id("ben"));
    a.register_agent(&ana, &id("growth-bot"), &ben, BTreeSet::new(), NOW)
        .await
        .unwrap();
    assert!(a.agent_usable(&id("refund-bot")).await.unwrap());
    assert!(!a.agent_usable(&id("ghost")).await.unwrap());

    // Suspended and resumed; disabled for good.
    a.suspend_agent(&ana, &id("growth-bot"), NOW).await.unwrap();
    assert!(!a.agent_usable(&id("growth-bot")).await.unwrap());
    assert!(a.suspend_agent(&ana, &id("growth-bot"), NOW).await.is_err());
    a.resume_agent(&ana, &id("growth-bot"), NOW).await.unwrap();
    assert!(a.agent_usable(&id("growth-bot")).await.unwrap());

    // The owner leaves: both agents are suspended, with a record each, before the person
    // is marked; and no code can be made for them.
    assert!(matches!(
        a.offboard_person(&ana, &ana, NOW).await,
        Err(AuthorityError::NotAllowed)
    ));
    a.offboard_person(&ana, &ben, NOW).await.unwrap();
    for agent in ["refund-bot", "growth-bot"] {
        assert!(!a.agent_usable(&id(agent)).await.unwrap());
    }
    assert!(a
        .offer_enrolment(&ana, EnrolKind::Agent, &id("refund-bot"), NOW)
        .await
        .is_err());
    let records = chain(&a).await;
    let tail: Vec<AdminAction> = records[records.len() - 3..]
        .iter()
        .map(|r| r.action)
        .collect();
    assert_eq!(
        tail,
        vec![
            AdminAction::AgentSuspended,
            AdminAction::AgentSuspended,
            AdminAction::PersonOffboarded
        ]
    );
    // Resuming the agent does not bring it back while its owner is gone.
    a.resume_agent(&ana, &id("refund-bot"), NOW).await.unwrap();
    assert!(!a.agent_usable(&id("refund-bot")).await.unwrap());
    // A person who has left cannot act, and a disabled agent is not resumed.
    a.add_person(&ana, &id("cy"), Role::Admin, NOW)
        .await
        .unwrap();
    a.offboard_person(&ana, &id("cy"), NOW).await.unwrap();
    assert!(matches!(
        a.register_gate(&id("cy"), &id("gate-2"), NOW).await,
        Err(AuthorityError::NotAllowed)
    ));
    a.disable_agent(&ana, &id("growth-bot"), NOW).await.unwrap();
    assert!(a.resume_agent(&ana, &id("growth-bot"), NOW).await.is_err());
    a.disable_gate(&ana, &id("gate-1"), NOW).await.unwrap();
    assert!(a.disable_gate(&ana, &id("gate-1"), NOW).await.is_err());
    assert!(a
        .offer_enrolment(&ana, EnrolKind::Gate, &id("gate-1"), NOW)
        .await
        .is_err());
}
both_stores!(an_offboarded_owners_agents_stop, offboarding);

async fn two_writers<S: Store + 'static>(store: S) {
    // Two admins act at once on one chain: each record still follows the one before it,
    // and nothing is lost or written twice.
    let a = std::sync::Arc::new(installed(store).await);
    let ana = id("ana");
    let mut tasks = Vec::new();
    for n in 0..12 {
        let (a, ana) = (a.clone(), ana.clone());
        tasks.push(tokio::spawn(async move {
            a.register_gate(&ana, &id(&format!("gate-x{n}")), NOW).await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    let records = chain(&a).await;
    assert_eq!(records.len(), 4 + 12);
    let gates: BTreeSet<Id> = records
        .iter()
        .filter(|r| r.action == AdminAction::GateRegistered)
        .map(|r| r.subject.id.clone())
        .collect();
    assert_eq!(gates.len(), 13);
}
both_stores!(two_admins_at_once_keep_one_chain, two_writers);

/// A store whose writes fail: what an action leaves behind when its record cannot be kept.
#[derive(Default)]
struct Failing {
    inner: MemoryStore,
    fail: std::sync::atomic::AtomicBool,
    /// Mandates this store does not show yet, as if they were issued a moment later than
    /// they were: see [`UNTIL_AGENT_STOPPED`] and the others.
    late: std::sync::atomic::AtomicU8,
}
/// Mandates are not listed until the record that stops an agent is written.
const UNTIL_AGENT_STOPPED: u8 = 1;
/// Mandates are not listed until the record that marks a person as gone is written.
const UNTIL_PERSON_GONE: u8 = 2;
/// Mandates are not listed, until the test says otherwise.
const UNTIL_TOLD: u8 = 3;
#[async_trait]
impl Store for Failing {
    async fn head(&self, scope: &Scope) -> StoreResult<Option<ChainHead>> {
        self.inner.head(scope).await
    }
    async fn commit(
        &self,
        scope: &Scope,
        record: &StoredRecord,
        effects: &[Effect],
    ) -> StoreResult<()> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.fail.load(SeqCst) {
            return Err(StoreError::Other("the disk is full".into()));
        }
        self.inner.commit(scope, record, effects).await?;
        let agent_stopped = effects
            .iter()
            .any(|e| matches!(e, Effect::PutAgent(a) if a.status != Status::Active));
        let person_gone = effects
            .iter()
            .any(|e| matches!(e, Effect::PutPerson(p) if !p.active));
        let late = self.late.load(SeqCst);
        if (late == UNTIL_AGENT_STOPPED && agent_stopped)
            || (late == UNTIL_PERSON_GONE && person_gone)
        {
            self.late.store(0, SeqCst);
        }
        Ok(())
    }
    async fn records_after(
        &self,
        scope: &Scope,
        seq: u64,
        limit: usize,
    ) -> StoreResult<Vec<StoredRecord>> {
        self.inner.records_after(scope, seq, limit).await
    }
    async fn any_person(&self, scope: &Scope) -> StoreResult<bool> {
        self.inner.any_person(scope).await
    }
    async fn person(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Person>> {
        self.inner.person(scope, id).await
    }
    async fn agent(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Agent>> {
        self.inner.agent(scope, id).await
    }
    async fn agents_of(&self, scope: &Scope, owner: &Id) -> StoreResult<Vec<Agent>> {
        self.inner.agents_of(scope, owner).await
    }
    async fn gate(&self, scope: &Scope, id: &Id) -> StoreResult<Option<Gate>> {
        self.inner.gate(scope, id).await
    }
    async fn key(&self, scope: &Scope, kid: &Id) -> StoreResult<Option<EnrolledKey>> {
        self.inner.key(scope, kid).await
    }
    async fn keys(&self, scope: &Scope) -> StoreResult<Vec<EnrolledKey>> {
        self.inner.keys(scope).await
    }
    async fn latest_bundle(&self, scope: &Scope) -> StoreResult<Option<gov_authority::Published>> {
        self.inner.latest_bundle(scope).await
    }
    async fn mandate(
        &self,
        scope: &Scope,
        id: &Id,
    ) -> StoreResult<Option<gov_authority::IssuedMandate>> {
        self.inner.mandate(scope, id).await
    }
    async fn mandates(&self, scope: &Scope) -> StoreResult<Vec<gov_authority::IssuedMandate>> {
        if self.late.load(std::sync::atomic::Ordering::SeqCst) != 0 {
            return Ok(Vec::new());
        }
        self.inner.mandates(scope).await
    }
    async fn revocations_after(
        &self,
        scope: &Scope,
        after: u64,
    ) -> StoreResult<Vec<gov_core::revocation::RevocationEntry>> {
        self.inner.revocations_after(scope, after).await
    }
    async fn bundle(
        &self,
        scope: &Scope,
        number: u64,
    ) -> StoreResult<Option<gov_authority::Published>> {
        self.inner.bundle(scope, number).await
    }
    async fn code(
        &self,
        scope: &Scope,
        kind: EnrolKind,
        name: &Id,
    ) -> StoreResult<Option<PendingCode>> {
        self.inner.code(scope, kind, name).await
    }
    async fn count_wrong_code(
        &self,
        scope: &Scope,
        kind: EnrolKind,
        name: &Id,
    ) -> StoreResult<u32> {
        self.inner.count_wrong_code(scope, kind, name).await
    }
    async fn take_code(
        &self,
        scope: &Scope,
        kind: EnrolKind,
        name: &Id,
        digest: &gov_core::Digest,
    ) -> StoreResult<Option<PendingCode>> {
        self.inner.take_code(scope, kind, name, digest).await
    }
}

/// No effect without its record (review finding GR-108): when the record cannot be written,
/// nothing the action would have changed has changed.
#[tokio::test]
async fn an_action_whose_record_cannot_be_kept_changes_nothing() {
    let a = installed(Failing::default()).await;
    let ana = id("ana");
    let before = chain(&a).await.len();
    a.store()
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);

    assert!(a.register_gate(&ana, &id("gate-2"), NOW).await.is_err());
    assert!(a.suspend_agent(&ana, &id("refund-bot"), NOW).await.is_err());
    assert!(a
        .offer_enrolment(&ana, EnrolKind::Gate, &id("gate-1"), NOW)
        .await
        .is_err());
    assert!(a.offboard_person(&ana, &id("ben"), NOW).await.is_err());

    a.store()
        .fail
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(a
        .store()
        .gate(&scope(), &id("gate-2"))
        .await
        .unwrap()
        .is_none());
    assert!(a.agent_usable(&id("refund-bot")).await.unwrap());
    assert!(a
        .store()
        .code(&scope(), EnrolKind::Gate, &id("gate-1"))
        .await
        .unwrap()
        .is_none());
    assert!(
        a.store()
            .person(&scope(), &id("ben"))
            .await
            .unwrap()
            .unwrap()
            .active
    );
    assert_eq!(chain(&a).await.len(), before);
    assert_eq!(
        a.store()
            .agent(&scope(), &id("refund-bot"))
            .await
            .unwrap()
            .unwrap()
            .status,
        Status::Active
    );
}

async fn stale_head<S: Store>(store: S) {
    // A store takes a record only when it is the next one, and applies nothing otherwise.
    let a = installed(store).await;
    let records = a.store().records_after(&scope(), 0, 100).await.unwrap();
    let head = records.last().unwrap().clone();
    let gate = |name: &str| {
        Effect::PutGate(Gate {
            id: id(name),
            status: Status::Active,
        })
    };
    // The same number again (a writer that read an old head), and a number that skips one.
    for (seq, name) in [(head.seq, "gate-stale"), (head.seq + 2, "gate-gap")] {
        let record = StoredRecord {
            seq,
            ..head.clone()
        };
        let refused = a.store().commit(&scope(), &record, &[gate(name)]).await;
        assert!(matches!(refused, Err(StoreError::HeadMoved)), "{name}");
        assert!(a.store().gate(&scope(), &id(name)).await.unwrap().is_none());
    }
    assert_eq!(
        a.store().records_after(&scope(), 0, 100).await.unwrap(),
        records
    );
    // Another tenant's chain is its own: it starts at 1 whatever this one holds.
    let other = Scope::new("other", "prod").unwrap();
    assert!(a.store().head(&other).await.unwrap().is_none());
    assert!(a
        .store()
        .records_after(&other, 0, 100)
        .await
        .unwrap()
        .is_empty());
    assert!(a
        .store()
        .person(&other, &id("ana"))
        .await
        .unwrap()
        .is_none());
    assert!(!a.store().any_person(&other).await.unwrap());
}
both_stores!(a_record_that_is_not_the_next_one_is_not_stored, stale_head);

async fn used_once<S: Store>(store: S) {
    // A code is taken by one caller, however many present it at once (GR-114); and a key,
    // or a key id, is added once, whatever a commit asks.
    let a = installed(store).await;
    let code = offer(&a).await;
    let digest = code_digest(&code);
    let gate_name = id("gate-1");
    let here = scope();
    let (first, second) = tokio::join!(
        a.store()
            .take_code(&here, EnrolKind::Gate, &gate_name, &digest),
        a.store()
            .take_code(&here, EnrolKind::Gate, &gate_name, &digest),
    );
    let got = [first.unwrap().is_some(), second.unwrap().is_some()];
    assert_eq!(got.iter().filter(|g| **g).count(), 1, "{got:?}");
    assert!(!pending(&a).await);
    // A wrong digest takes nothing and leaves the code where it is.
    let code = offer(&a).await;
    let wrong = code_digest("not-the-code");
    assert!(a
        .store()
        .take_code(&scope(), EnrolKind::Gate, &gate_name, &wrong)
        .await
        .unwrap()
        .is_none());
    assert!(pending(&a).await);
    drop(code);

    // Two requests with one code, at the same moment: one key is enrolled.
    let code = offer(&a).await;
    let (k1, k2) = (key("gate-1:k1", 20), key("gate-1:k2", 25));
    let r1 = request(&k1, EnrolKind::Gate, "gate-1", k1.public(), &code);
    let r2 = request(&k2, EnrolKind::Gate, "gate-1", k2.public(), &code);
    let (e1, e2) = tokio::join!(a.enrol(&code, &r1, NOW), a.enrol(&code, &r2, NOW));
    assert_eq!([e1.is_ok(), e2.is_ok()].iter().filter(|ok| **ok).count(), 1);
    assert_eq!(a.store().keys(&scope()).await.unwrap().len(), 1);

    // A commit that would add a key id, or a key, that is there already writes nothing.
    let held = a.store().keys(&scope()).await.unwrap().remove(0);
    let head = a.store().records_after(&scope(), 0, 100).await.unwrap();
    let next = StoredRecord {
        seq: head.last().unwrap().seq + 1,
        ..head.last().unwrap().clone()
    };
    let other_key = key(held.key.kid.as_str(), 26).public();
    let same_id = Effect::AddKey(EnrolledKey {
        key: other_key,
        ..held.clone()
    });
    let seed = if held.key.kid == id("gate-1:k1") {
        20
    } else {
        25
    };
    let same_key = Effect::AddKey(EnrolledKey {
        key: key("gate-1:k9", seed).public(),
        ..held.clone()
    });
    for effect in [same_id, same_key] {
        let refused = a.store().commit(&scope(), &next, &[effect]).await;
        assert!(matches!(refused, Err(StoreError::KeyTaken)), "{refused:?}");
    }
    assert_eq!(
        a.store().records_after(&scope(), 0, 100).await.unwrap(),
        head
    );
    assert_eq!(a.store().keys(&scope()).await.unwrap(), vec![held]);
}
both_stores!(a_code_and_a_key_are_used_once, used_once);

async fn first_admin_once<S: Store + 'static>(store: S) {
    // Two installers naming a first admin at the same moment: one is named (GR-119).
    let a = std::sync::Arc::new(authority(store));
    let mut tasks = Vec::new();
    for name in ["ana", "eve", "mallory", "trent"] {
        let a = a.clone();
        tasks.push(tokio::spawn(async move {
            a.bootstrap(&id(name), NOW).await.is_ok()
        }));
    }
    let mut named = 0;
    for task in tasks {
        named += usize::from(task.await.unwrap());
    }
    assert_eq!(named, 1);
    assert_eq!(chain(&a).await.len(), 1);
}
both_stores!(the_first_admin_is_named_once, first_admin_once);

async fn time_forward<S: Store>(store: S) {
    // A record's time never goes back along the chain, whatever the clock does (GR-121).
    let a = installed(store).await;
    a.register_gate(&id("ana"), &id("gate-late"), NOW + 100)
        .await
        .unwrap();
    a.register_gate(&id("ana"), &id("gate-early"), NOW - 500)
        .await
        .unwrap();
    let times: Vec<i64> = chain(&a).await.iter().map(|r| r.at).collect();
    assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");
    assert_eq!(*times.last().unwrap(), NOW + 100);
}
both_stores!(a_records_time_does_not_go_back, time_forward);

fn root_key() -> SigningKey {
    key("authority-root-1", 61)
}

/// A signed bundle with this number, for `issuer`, with one rule per id given.
fn signed_bundle(number: u64, issuer: &str, rules: &[&str], signer: &SigningKey) -> Envelope {
    use gov_core::policy::{Effect as Then, Match, OnlineKeys, PolicyBundle, Rule};
    let bundle = PolicyBundle {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id(issuer),
        version: id("v"),
        number,
        issued_at: NOW,
        keys: OnlineKeys {
            mandates: vec![key("authority:mandates-1", 71).public()],
            leases: vec![key("authority:leases-1", 72).public()],
        },
        freshness: Default::default(),
        rules: rules
            .iter()
            .map(|r| Rule {
                id: id(r),
                matches: Match::default(),
                then: Then::Refuse,
                freshness: None,
            })
            .collect(),
        connectors: Vec::new(),
        templates: Vec::new(),
    };
    seal(&bundle, signer).unwrap()
}

/// Publishing a bundle (spec §7.6.1, point 4): it opens under the root key, is this
/// Authority's, carries the next number, and is kept with a record of its digest.
async fn publishing<S: Store>(store: S) {
    let a = installed(store).await;
    let ana = id("ana");
    let before = chain(&a).await.len();
    assert!(a.latest_bundle().await.unwrap().is_none());
    let first = signed_bundle(1, "authority", &["a"], &root_key());
    // Only an admin publishes.
    assert!(matches!(
        a.publish_bundle(&id("ben"), &first, NOW).await,
        Err(AuthorityError::NotAllowed)
    ));
    // Not signed by the root: by the admin-record key, or by an online key it names.
    for signer in [admin_key(), key("authority:mandates-1", 71)] {
        let forged = signed_bundle(1, "authority", &["a"], &signer);
        assert!(a.publish_bundle(&ana, &forged, NOW).await.is_err());
    }
    // Another Authority's bundle, and a first bundle that is not number 1.
    let elsewhere = signed_bundle(1, "another", &["a"], &root_key());
    assert!(a.publish_bundle(&ana, &elsewhere, NOW).await.is_err());
    let early = signed_bundle(2, "authority", &["a"], &root_key());
    assert!(matches!(
        a.publish_bundle(&ana, &early, NOW).await,
        Err(AuthorityError::NotNext(1))
    ));
    assert!(a.latest_bundle().await.unwrap().is_none());
    assert_eq!(chain(&a).await.len(), before);

    // Published: kept as it was signed, with a record carrying its number and digest.
    let digest = a.publish_bundle(&ana, &first, NOW).await.unwrap();
    let opened = gov_core::policy::open_bundle(&first, &root(), &scope()).unwrap();
    assert_eq!(digest, opened.digest);
    let kept = a.latest_bundle().await.unwrap().unwrap();
    assert_eq!(
        (kept.number, &kept.digest, &kept.envelope),
        (1, &digest, &first)
    );
    let records = chain(&a).await;
    let last = records.last().unwrap();
    assert_eq!(records.len(), before + 1);
    assert_eq!(
        (last.action, last.subject.kind, last.subject.id.as_str()),
        (AdminAction::PolicyPublished, SubjectKind::Policy, "1")
    );
    assert_eq!(last.value.as_ref(), Some(&digest));

    // The same number again, with other content or the same, and a number skipped: only
    // the next is taken.
    for (number, rules) in [(1, vec!["a"]), (1, vec!["b"]), (3, vec!["a"])] {
        let offered = signed_bundle(number, "authority", &rules, &root_key());
        assert!(matches!(
            a.publish_bundle(&ana, &offered, NOW).await,
            Err(AuthorityError::NotNext(2))
        ));
    }
    assert_eq!(chain(&a).await.len(), before + 1);
    let second = signed_bundle(2, "authority", &["a", "b"], &root_key());
    let digest_2 = a.publish_bundle(&ana, &second, NOW).await.unwrap();
    assert_ne!(digest_2, digest);
    assert_eq!(a.latest_bundle().await.unwrap().unwrap().envelope, second);
    // The earlier one is still held under its number.
    let held = a.store().bundle(&scope(), 1).await.unwrap().unwrap();
    assert_eq!(held.envelope, first);
    assert!(a.store().bundle(&scope(), 3).await.unwrap().is_none());
    // Ten and more: the latest is the highest number, not the last in the order of text.
    let mut latest = 2;
    for number in 3..=11 {
        let b = signed_bundle(number, "authority", &["a"], &root_key());
        a.publish_bundle(&ana, &b, NOW).await.unwrap();
        latest = number;
    }
    assert_eq!(a.latest_bundle().await.unwrap().unwrap().number, latest);
}
both_stores!(
    a_bundle_is_published_under_the_root_key_and_the_next_number,
    publishing
);

/// An agent and a gate may not share a name, nor take the Authority's (GR-127): a key's
/// id is under the name it is for.
async fn names_apart<S: Store>(store: S) {
    let a = installed(store).await;
    let ana = id("ana");
    let before = chain(&a).await.len();
    let exists = |r: Result<u64, AuthorityError>| {
        assert!(matches!(r, Err(AuthorityError::Exists("name"))), "{r:?}");
    };
    exists(a.register_gate(&ana, &id("refund-bot"), NOW).await);
    exists(a.register_gate(&ana, &id("authority"), NOW).await);
    exists(
        a.register_agent(&ana, &id("gate-1"), &id("ben"), BTreeSet::new(), NOW)
            .await,
    );
    exists(
        a.register_agent(&ana, &id("authority"), &id("ben"), BTreeSet::new(), NOW)
            .await,
    );
    assert_eq!(chain(&a).await.len(), before);
}
both_stores!(an_agent_and_a_gate_do_not_share_a_name, names_apart);

// ----- G4c: mandates from the Authority (spec §7.6.2) --------------------------------------

use gov_authority::MandateAsk;
use gov_core::action::Grant;
use gov_core::mandate::{
    verify_chain, ChainCheck, DelegateRequest, IssuerKind, Limits, Mandate, Template,
};
use gov_core::revocation::{Revocable, RevocationFeed, RevocationSet};

fn grant(ops: &[&str], resources: &[&str]) -> Grant {
    Grant {
        system: id("jira"),
        operations: ops.iter().map(|o| id(o)).collect(),
        resources: resources
            .iter()
            .map(|r| gov_core::ResourcePattern::new(r).unwrap())
            .collect(),
    }
}

fn template(name: &str, delegable: bool, people: &[&str], receivers: &[&str]) -> Template {
    Template {
        id: id(name),
        grants: vec![grant(
            &["issue.read", "issue.update"],
            &["jira/project/pay/*"],
        )],
        max_life_secs: 3600,
        limits: Limits {
            max_actions: Some(100),
            max_writes: Some(10),
        },
        delegable,
        people: people.iter().map(|p| id(p)).collect(),
        receivers: receivers.iter().map(|p| id(p)).collect(),
    }
}

fn templates() -> Vec<Template> {
    vec![
        template("triage", true, &[], &[]),
        template("solo", false, &[], &[]),
        template("shared", true, &["cy"], &["cy"]),
    ]
}

/// A signed bundle carrying the templates, with these keys for mandates.
fn policy(number: u64, mandate_keys: Vec<PublicKey>) -> Envelope {
    policy_with(number, mandate_keys, leases_key().public())
}

fn policy_with(number: u64, mandate_keys: Vec<PublicKey>, leases: PublicKey) -> Envelope {
    use gov_core::policy::{OnlineKeys, PolicyBundle};
    let bundle = PolicyBundle {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        issuer: id("authority"),
        version: id("v"),
        number,
        issued_at: NOW,
        keys: OnlineKeys {
            mandates: mandate_keys,
            leases: vec![leases],
        },
        freshness: Default::default(),
        rules: Vec::new(),
        connectors: Vec::new(),
        templates: templates(),
    };
    seal(&bundle, &root_key()).unwrap()
}

async fn enrol_agent<S: Store>(a: &Authority<S>, name: &str, k: &SigningKey) {
    let code = a
        .offer_enrolment(&id("ana"), EnrolKind::Agent, &id(name), NOW)
        .await
        .unwrap();
    let asked = request(k, EnrolKind::Agent, name, k.public(), &code);
    a.enrol(&code, &asked, NOW).await.unwrap();
}

fn ask(agent: &str, kid: &str, template: &str) -> MandateAsk {
    MandateAsk {
        agent: id(agent),
        key: id(kid),
        template: id(template),
        grants: vec![grant(&["issue.update"], &["jira/project/pay/*"])],
        life_secs: 1800,
        limits: Limits::default(),
    }
}

/// Check a mandate as a gate holding the templates would.
fn as_a_gate(mandate: &Envelope, revoked: &RevocationSet) -> Result<Vec<Mandate>, gov_core::Error> {
    let issuers = KeySet::new([mandates_key().public()]);
    let held = templates();
    verify_chain(
        mandate,
        &ChainCheck {
            scope: &scope(),
            authority: &issuers,
            now: NOW,
            revoked,
            templates: Some(&held),
        },
    )
    .map(|c| c.links().to_vec())
}

fn delegate_request(
    signer: &SigningKey,
    parent: &Id,
    to: (&str, &str),
    change: &dyn Fn(&mut DelegateRequest),
) -> Envelope {
    let mut r = DelegateRequest {
        v: 1,
        tenant: id("acme"),
        env: id("prod"),
        authority: id("authority"),
        parent: parent.clone(),
        agent: id(to.0),
        key: id(to.1),
        grants: vec![grant(&["issue.update"], &["jira/project/pay/issue/*"])],
        expires_at: NOW + 600,
        limits: Limits::default(),
        nonce: id("n-1"),
        at: NOW,
    };
    change(&mut r);
    seal(&r, signer).unwrap()
}

/// An installation with a published bundle, three agents with enrolled keys (two of ben's,
/// one of cy's), and nothing issued yet.
async fn ready<S: Store>(store: S) -> (Authority<S>, SigningKey, SigningKey, SigningKey) {
    let a = installed(store).await;
    let ana = id("ana");
    a.add_person(&ana, &id("cy"), Role::Owner, NOW)
        .await
        .unwrap();
    a.add_person(&ana, &id("aud"), Role::Auditor, NOW)
        .await
        .unwrap();
    for (agent, owner) in [("helper-bot", "ben"), ("cy-bot", "cy")] {
        a.register_agent(&ana, &id(agent), &id(owner), BTreeSet::new(), NOW)
            .await
            .unwrap();
    }
    let (refund, helper, cys) = (
        key("refund-bot:k1", 9),
        key("helper-bot:k1", 12),
        key("cy-bot:k1", 13),
    );
    enrol_agent(&a, "refund-bot", &refund).await;
    enrol_agent(&a, "helper-bot", &helper).await;
    enrol_agent(&a, "cy-bot", &cys).await;
    (a, refund, helper, cys)
}

/// Issuing to a person's request: who may ask, what may be asked for, and what is written.
async fn issuing<S: Store>(store: S) {
    let (a, _refund, _, _) = ready(store).await;
    let (ana, ben, cy) = (id("ana"), id("ben"), id("cy"));
    let good = ask("refund-bot", "refund-bot:k1", "triage");

    // No bundle published: there is no template to issue from.
    assert!(a.issue_mandate(&ben, &good, NOW).await.is_err());
    // A bundle that does not list this Authority's mandates key: it issues nothing.
    let other = key("authority:mandates-9", 79).public();
    a.publish_bundle(&ana, &policy(1, vec![other]), NOW)
        .await
        .unwrap();
    assert!(matches!(
        a.issue_mandate(&ben, &good, NOW).await,
        Err(AuthorityError::Unknown("mandates key in the bundle"))
    ));
    a.publish_bundle(&ana, &policy(2, vec![mandates_key().public()]), NOW)
        .await
        .unwrap();
    let before = chain(&a).await.len();
    let refused = |r: Result<Envelope, AuthorityError>| assert!(r.is_err(), "issued");

    // The owner asks, and gets a mandate a gate holding the same templates accepts.
    let issued = a.issue_mandate(&ben, &good, NOW).await.unwrap();
    let links = as_a_gate(&issued, &RevocationSet::default()).unwrap();
    let m = &links[0];
    assert_eq!(m.agent.id, id("refund-bot"));
    assert_eq!(m.agent.key, key("refund-bot:k1", 9).public());
    assert_eq!(m.template.as_str(), "triage@2");
    assert_eq!((m.issued_at, m.expires_at), (NOW, NOW + 1800));
    // A ceiling not asked for is the template's, never none.
    assert_eq!(m.limits, template("triage", true, &[], &[]).limits);
    // Nobody is signed in yet, and the mandate says so.
    assert_eq!(
        (m.issued_by.kind, &m.issued_by.id),
        (IssuerKind::Person, &ben)
    );
    assert!(m.issued_by.session.is_none());
    // Signed with the mandates key, and with no other key of the Authority's.
    for wrong in [admin_key().public(), leases_key().public(), root()] {
        assert!(open::<Mandate>(&issued, &KeySet::new([wrong]), &scope()).is_err());
    }
    // One record, in the same step: the mandate's id, the agent, the digest of the mandate.
    let records = chain(&a).await;
    assert_eq!(records.len(), before + 1);
    let last = records.last().unwrap();
    assert_eq!(
        (
            last.action,
            &last.subject.id,
            last.about.as_ref().map(|s| &s.id)
        ),
        (AdminAction::MandateIssued, &m.id, Some(&id("refund-bot")))
    );
    let digest = open::<Mandate>(&issued, &KeySet::new([mandates_key().public()]), &scope())
        .unwrap()
        .hash;
    assert_eq!(last.value.as_ref(), Some(&digest));
    let kept = a.store().mandate(&scope(), &m.id).await.unwrap().unwrap();
    assert_eq!(
        (&kept.envelope, kept.revoked, &kept.root_person),
        (&issued, false, &ben)
    );
    // Two mandates never share an id.
    let again = a.issue_mandate(&ben, &good, NOW).await.unwrap();
    assert_ne!(
        as_a_gate(&again, &RevocationSet::default()).unwrap()[0].id,
        m.id
    );
    let before = chain(&a).await.len();

    // An admin does not thereby hold an agent's authority; nor does another owner.
    for other in [&ana, &cy] {
        assert!(matches!(
            a.issue_mandate(other, &good, NOW).await,
            Err(AuthorityError::NotAllowed)
        ));
    }
    // Unless the template names them.
    a.issue_mandate(&cy, &ask("refund-bot", "refund-bot:k1", "shared"), NOW)
        .await
        .unwrap();
    let before = before + 1;
    // Nobody, and somebody offboarded.
    assert!(a.issue_mandate(&id("nobody"), &good, NOW).await.is_err());

    // More than the template allows: an operation, a resource, a life, a ceiling.
    let with = |change: &dyn Fn(&mut MandateAsk)| {
        let mut wider = good.clone();
        change(&mut wider);
        wider
    };
    for wider in [
        with(&|m| m.grants = vec![grant(&["issue.delete"], &["jira/project/pay/*"])]),
        with(&|m| m.grants = vec![grant(&["issue.update"], &["jira/project/ops/*"])]),
        with(&|m| m.grants = Vec::new()),
        with(&|m| m.life_secs = 3601),
        with(&|m| m.life_secs = 0),
        with(&|m| m.limits.max_writes = Some(11)),
        // A template the bundle does not have.
        with(&|m| m.template = id("admin")),
        // A key that is not this agent's: another agent's, a gate's, nobody's.
        with(&|m| m.key = id("helper-bot:k1")),
        with(&|m| m.key = id("ghost:k1")),
        // An agent nobody registered.
        with(&|m| m.agent = id("ghost")),
    ] {
        refused(a.issue_mandate(&ben, &wider, NOW).await);
    }
    assert_eq!(chain(&a).await.len(), before);
    // A narrower ceiling than the template's is kept as asked.
    let narrow = with(&|m| m.limits.max_writes = Some(3));
    let issued = a.issue_mandate(&ben, &narrow, NOW).await.unwrap();
    let links = as_a_gate(&issued, &RevocationSet::default()).unwrap();
    assert_eq!(links[0].limits.max_writes, Some(3));

    // A person who has left asks for nothing, though the template still names them.
    a.offboard_person(&ana, &cy, NOW).await.unwrap();
    assert!(matches!(
        a.issue_mandate(&cy, &ask("refund-bot", "refund-bot:k1", "shared"), NOW)
            .await,
        Err(AuthorityError::NotAllowed)
    ));
    // Nor do they see anything.
    assert!(matches!(
        a.list_mandates(&cy).await,
        Err(AuthorityError::NotAllowed)
    ));

    // A suspended agent is issued nothing.
    a.suspend_agent(&ana, &id("refund-bot"), NOW).await.unwrap();
    refused(a.issue_mandate(&ben, &good, NOW).await);
}
both_stores!(
    a_mandate_is_issued_to_its_agents_owner_inside_a_template,
    issuing
);

/// Delegating: at the signed request of the agent holding the parent, narrower, to an
/// agent whose owner agrees, once.
async fn delegating<S: Store>(store: S) {
    let (a, refund, helper, cys) = ready(store).await;
    let (ana, ben) = (id("ana"), id("ben"));
    a.publish_bundle(&ana, &policy(1, vec![mandates_key().public()]), NOW)
        .await
        .unwrap();
    let parent_env = a
        .issue_mandate(&ben, &ask("refund-bot", "refund-bot:k1", "triage"), NOW)
        .await
        .unwrap();
    let parent = as_a_gate(&parent_env, &RevocationSet::default()).unwrap()[0].clone();
    let to_helper = ("helper-bot", "helper-bot:k1");
    let before = chain(&a).await.len();
    let refused = |r: Result<Envelope, AuthorityError>| {
        assert!(
            matches!(r, Err(AuthorityError::Refused)),
            "{:?}",
            r.map(|_| ())
        );
    };

    // What is not a request the Authority serves.
    for bad in [
        // Signed by another agent's key, or naming another Authority.
        delegate_request(&helper, &parent.id, to_helper, &|_| {}),
        delegate_request(&refund, &parent.id, to_helper, &|r| {
            r.authority = id("another")
        }),
        // Too old, or dated ahead.
        delegate_request(&refund, &parent.id, to_helper, &|r| r.at = NOW - 301),
        delegate_request(&refund, &parent.id, to_helper, &|r| r.at = NOW + 301),
        // A mandate that does not exist.
        delegate_request(&refund, &id("m-none"), to_helper, &|_| {}),
        // Wider than the parent: an operation it lacks, a resource outside, a later end.
        delegate_request(&refund, &parent.id, to_helper, &|r| {
            r.grants = vec![grant(&["issue.read"], &["jira/project/pay/*"])]
        }),
        delegate_request(&refund, &parent.id, to_helper, &|r| {
            r.grants = vec![grant(&["issue.update"], &["jira/project/ops/*"])]
        }),
        delegate_request(&refund, &parent.id, to_helper, &|r| {
            r.expires_at = NOW + 1801
        }),
        delegate_request(&refund, &parent.id, to_helper, &|r| {
            r.limits.max_writes = Some(11)
        }),
        // To itself, to a key that is not the receiver's, to an agent nobody registered.
        delegate_request(
            &refund,
            &parent.id,
            ("refund-bot", "refund-bot:k1"),
            &|_| {},
        ),
        delegate_request(&refund, &parent.id, ("helper-bot", "cy-bot:k1"), &|_| {}),
        delegate_request(&refund, &parent.id, ("ghost", "ghost:k1"), &|_| {}),
        // To an agent whose owner is not the person at the root: nobody asked cy (GR-139).
        delegate_request(&refund, &parent.id, ("cy-bot", "cy-bot:k1"), &|_| {}),
    ] {
        refused(a.delegate(&bad, NOW).await);
    }
    assert_eq!(chain(&a).await.len(), before);

    // Served: a child a gate accepts as a chain of two, under the parent's template.
    let asked = delegate_request(&refund, &parent.id, to_helper, &|_| {});
    let child_env = a.delegate(&asked, NOW).await.unwrap();
    let links = as_a_gate(&child_env, &RevocationSet::default()).unwrap();
    assert_eq!(links.len(), 2);
    let child = &links[0];
    assert_eq!(
        (&child.agent.id, &child.agent.key),
        (&id("helper-bot"), &helper.public())
    );
    assert_eq!(
        (child.issued_by.kind, &child.issued_by.id),
        (IssuerKind::Agent, &id("refund-bot"))
    );
    assert_eq!(
        (&child.template, child.expires_at),
        (&parent.template, NOW + 600)
    );
    assert_eq!(links[1].id, parent.id);
    // The record is about the child agent, and the asking agent is its actor.
    let records = chain(&a).await;
    let last = records.last().unwrap();
    assert_eq!(records.len(), before + 1);
    assert_eq!(
        (
            last.action,
            &last.subject.id,
            last.about.as_ref().map(|s| &s.id)
        ),
        (
            AdminAction::MandateIssued,
            &child.id,
            Some(&id("helper-bot"))
        )
    );
    assert_eq!(
        (&last.actor.id, last.actor.verified),
        (&id("refund-bot"), false)
    );
    // An agent, and the record says so: a person may be called refund-bot too (GR-154).
    assert_eq!(last.actor.kind, gov_core::admin::ActorKind::Agent);
    // Every other record so far is a person's, and its signed text does not carry the
    // field at all, as records written before the field existed do not.
    for record in &records[..records.len() - 1] {
        assert_eq!(record.actor.kind, gov_core::admin::ActorKind::Person);
    }
    let stored = a.store().records_after(&scope(), 0, 10_000).await.unwrap();
    let text = |e: &Envelope| {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&e.payload)
            .unwrap();
        String::from_utf8(bytes).unwrap()
    };
    assert!(text(&stored.last().unwrap().envelope).contains(r#""kind":"agent""#));
    assert!(text(&stored[0].envelope).contains(r#""actor":{"id":"ana","verified":false}"#));
    let kept = a
        .store()
        .mandate(&scope(), &child.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (&kept.root_person, &kept.parent),
        (&ben, &Some(parent.id.clone()))
    );

    // The same request is never served twice.
    refused(a.delegate(&asked, NOW).await);
    assert_eq!(chain(&a).await.len(), before + 1);

    // A template that names who may receive lets cy's agent take a delegation.
    let shared = a
        .issue_mandate(&ben, &ask("refund-bot", "refund-bot:k1", "shared"), NOW)
        .await
        .unwrap();
    let shared = as_a_gate(&shared, &RevocationSet::default()).unwrap()[0].clone();
    let to_cy = delegate_request(&refund, &shared.id, ("cy-bot", "cy-bot:k1"), &|_| {});
    let cys_env = a.delegate(&to_cy, NOW).await.unwrap();
    // And the holder of that child asks in its turn, with its own key, for a third link.
    let cys_mandate = as_a_gate(&cys_env, &RevocationSet::default()).unwrap()[0].clone();
    let third = delegate_request(&cys, &cys_mandate.id, to_helper, &|r| {
        r.nonce = id("n-3");
        r.expires_at = NOW + 300;
    });
    // helper-bot is ben's, and ben is the person at the root.
    let third = a.delegate(&third, NOW).await.unwrap();
    assert_eq!(
        as_a_gate(&third, &RevocationSet::default()).unwrap().len(),
        3
    );

    // A template that does not allow delegating.
    let solo = a
        .issue_mandate(&ben, &ask("refund-bot", "refund-bot:k1", "solo"), NOW)
        .await
        .unwrap();
    let solo = as_a_gate(&solo, &RevocationSet::default()).unwrap()[0].clone();
    refused(
        a.delegate(
            &delegate_request(&refund, &solo.id, to_helper, &|_| {}),
            NOW,
        )
        .await,
    );

    // A revoked parent is delegated from no more, and its child no longer verifies at a
    // gate that has read the feed: one entry covers the descendants.
    a.revoke_mandate(&ben, &parent.id, NOW).await.unwrap();
    let fresh = delegate_request(&refund, &parent.id, to_helper, &|r| r.nonce = id("n-9"));
    refused(a.delegate(&fresh, NOW).await);
    let mut revoked = RevocationSet::default();
    revoked.revoke(Revocable::Mandate, parent.id.clone());
    assert_eq!(
        as_a_gate(&child_env, &revoked).unwrap_err(),
        gov_core::Error::Revoked
    );
}
both_stores!(
    a_mandate_is_delegated_narrower_at_its_holders_signed_request,
    delegating
);

/// Listing, revoking, and the feed a gate reads.
async fn revoking<S: Store>(store: S) {
    let (a, _refund, _, _) = ready(store).await;
    let (ana, ben, cy, aud) = (id("ana"), id("ben"), id("cy"), id("aud"));
    a.publish_bundle(&ana, &policy(1, vec![mandates_key().public()]), NOW)
        .await
        .unwrap();
    let mut issued = Vec::new();
    for (owner, agent) in [(&ben, "refund-bot"), (&ben, "helper-bot"), (&cy, "cy-bot")] {
        let env = a
            .issue_mandate(owner, &ask(agent, &format!("{agent}:k1"), "triage"), NOW)
            .await
            .unwrap();
        issued.push(
            as_a_gate(&env, &RevocationSet::default()).unwrap()[0]
                .id
                .clone(),
        );
    }
    let (m_refund, m_helper, m_cy) = (&issued[0], &issued[1], &issued[2]);

    // A person sees the mandates of agents they own; an admin and an auditor see all.
    let seen = |who: Id| {
        let a = &a;
        async move {
            let mut ids: Vec<Id> = a
                .list_mandates(&who)
                .await
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect();
            ids.sort();
            ids
        }
    };
    let sorted = |mut v: Vec<Id>| {
        v.sort();
        v
    };
    assert_eq!(
        seen(ben.clone()).await,
        sorted(vec![m_refund.clone(), m_helper.clone()])
    );
    assert_eq!(seen(cy.clone()).await, vec![m_cy.clone()]);
    assert_eq!(seen(ana.clone()).await, sorted(issued.clone()));
    assert_eq!(seen(aud.clone()).await, sorted(issued.clone()));
    assert!(a.list_mandates(&id("nobody")).await.is_err());

    // Before anything is revoked the feed is empty, and is the leases key's to sign.
    let leases = KeySet::new([leases_key().public()]);
    let feed = |after: u64, now: i64| {
        let (a, leases) = (&a, &leases);
        async move {
            let env = a.revocation_feed(after, now).await.unwrap();
            for wrong in [admin_key().public(), mandates_key().public(), root()] {
                assert!(open::<RevocationFeed>(&env, &KeySet::new([wrong]), &scope()).is_err());
            }
            let feed = open::<RevocationFeed>(&env, leases, &scope()).unwrap().doc;
            feed.validate().unwrap();
            feed
        }
    };
    let empty = feed(0, NOW).await;
    assert_eq!(
        (empty.through, empty.dropped, empty.entries.len()),
        (0, 0, 0)
    );

    // Another owner, and an auditor, revoke nothing of ben's. Ben does; so may an admin.
    let before = chain(&a).await.len();
    for other in [&cy, &aud, &id("nobody")] {
        assert!(matches!(
            a.revoke_mandate(other, m_refund, NOW).await,
            Err(AuthorityError::NotAllowed)
        ));
    }
    assert_eq!(a.revoke_mandate(&ben, m_refund, NOW).await.unwrap(), 1);
    assert_eq!(a.revoke_mandate(&ana, m_cy, NOW).await.unwrap(), 2);
    // Revoked once.
    assert!(a.revoke_mandate(&ben, m_refund, NOW).await.is_err());
    assert!(a.revoke_mandate(&ben, &id("m-none"), NOW).await.is_err());
    let records = chain(&a).await;
    assert_eq!(records.len(), before + 2);
    let last = records.last().unwrap();
    assert_eq!(
        (last.action, &last.subject.id),
        (AdminAction::Revoked, m_cy)
    );
    assert!(
        a.store()
            .mandate(&scope(), m_refund)
            .await
            .unwrap()
            .unwrap()
            .revoked
    );
    assert!(a
        .list_mandates(&ben)
        .await
        .unwrap()
        .iter()
        .any(|m| &m.id == m_refund && m.revoked));

    // The feed: numbered from 1 with no gaps, and what it names is what a gate refuses.
    let all = feed(0, NOW).await;
    assert_eq!((all.after, all.through, all.dropped), (0, 2, 0));
    assert_eq!(
        all.entries
            .iter()
            .map(|e| (e.seq, e.kind, &e.id))
            .collect::<Vec<_>>(),
        vec![
            (1, Revocable::Mandate, m_refund),
            (2, Revocable::Mandate, m_cy)
        ]
    );
    assert_eq!(all.entries[0].until, NOW + 1800);
    let mut set = RevocationSet::default();
    all.apply(&mut set);
    let kept = a
        .store()
        .mandate(&scope(), m_refund)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        as_a_gate(&kept.envelope, &set).unwrap_err(),
        gov_core::Error::Revoked
    );
    // From a number already read, only what is later; from beyond the newest, nothing.
    let later = feed(1, NOW).await;
    assert_eq!((later.entries.len(), later.entries[0].seq), (1, 2));
    assert!(matches!(
        a.revocation_feed(3, NOW).await,
        Err(AuthorityError::Refused)
    ));

    // Suspending an agent revokes its live mandates, each with its own record, and puts
    // nothing in the feed that would have to be undone: no agent id (GR-140).
    let before = chain(&a).await.len();
    a.suspend_agent(&ana, &id("helper-bot"), NOW).await.unwrap();
    let records = chain(&a).await;
    assert_eq!(records.len(), before + 2);
    assert_eq!(
        (records[before].action, &records[before].subject.id),
        (AdminAction::Revoked, m_helper)
    );
    assert_eq!(records[before + 1].action, AdminAction::AgentSuspended);
    let after_suspend = feed(0, NOW).await;
    assert_eq!(after_suspend.through, 3);
    assert!(after_suspend
        .entries
        .iter()
        .all(|e| e.kind == Revocable::Mandate));
    // Resumed, the agent is issued a new mandate; the old one stays revoked.
    a.resume_agent(&ana, &id("helper-bot"), NOW).await.unwrap();
    a.issue_mandate(&ben, &ask("helper-bot", "helper-bot:k1", "triage"), NOW)
        .await
        .unwrap();
    assert!(
        a.store()
            .mandate(&scope(), m_helper)
            .await
            .unwrap()
            .unwrap()
            .revoked
    );

    // Disabling is for good: the new mandate is revoked, and the agent's id joins the feed.
    a.disable_agent(&ana, &id("helper-bot"), NOW).await.unwrap();
    let after_disable = feed(0, NOW).await;
    assert_eq!(after_disable.through, 5);
    let last = after_disable.entries.last().unwrap();
    assert_eq!(
        (last.seq, last.kind, &last.id, last.until),
        (5, Revocable::Agent, &id("helper-bot"), i64::MAX)
    );

    // Long after the mandates would have expired anyway, their entries are left out and
    // counted, so the asker sees every number accounted for; the agent's stays.
    let much_later = feed(0, NOW + 1800 + 301).await;
    assert_eq!((much_later.through, much_later.dropped), (5, 4));
    assert_eq!(much_later.entries.len(), 1);
    assert_eq!(much_later.entries[0].kind, Revocable::Agent);
    // Just inside the allowance they are all still there.
    assert_eq!(feed(0, NOW + 1800 + 300).await.dropped, 0);

    // Offboarding a person revokes their agents' mandates too.
    let m_new = a
        .issue_mandate(&cy, &ask("cy-bot", "cy-bot:k1", "triage"), NOW)
        .await
        .unwrap();
    let m_new = as_a_gate(&m_new, &RevocationSet::default()).unwrap()[0]
        .id
        .clone();
    a.offboard_person(&ana, &cy, NOW).await.unwrap();
    assert!(
        a.store()
            .mandate(&scope(), &m_new)
            .await
            .unwrap()
            .unwrap()
            .revoked
    );

    // A bundle that does not list this Authority's leases key: it signs no feed.
    let other = key("authority:leases-9", 78).public();
    a.publish_bundle(
        &ana,
        &policy_with(2, vec![mandates_key().public()], other),
        NOW,
    )
    .await
    .unwrap();
    assert!(matches!(
        a.revocation_feed(0, NOW).await,
        Err(AuthorityError::Unknown("leases key in the bundle"))
    ));

    // Last, since it writes a made-up record: the store takes a revocation only under the
    // next number, and writes nothing of a commit that carries any other.
    let held: Vec<u64> = a
        .store()
        .revocations_after(&scope(), 0)
        .await
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    let newest = *held.last().unwrap();
    let records = a.store().records_after(&scope(), 0, 1000).await.unwrap();
    let head = records.last().unwrap().clone();
    for wrong in [newest, newest + 2, 1] {
        let record = StoredRecord {
            seq: head.seq + 1,
            ..head.clone()
        };
        let entry = gov_core::revocation::RevocationEntry {
            seq: wrong,
            kind: Revocable::Mandate,
            id: id("m-made-up"),
            until: NOW,
        };
        let refused = a
            .store()
            .commit(&scope(), &record, &[Effect::AddRevocation(entry)])
            .await;
        assert!(matches!(refused, Err(StoreError::HeadMoved)), "{wrong}");
    }
    let after: Vec<u64> = a
        .store()
        .revocations_after(&scope(), 0)
        .await
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(after, held);
    assert_eq!(
        a.store().records_after(&scope(), 0, 1000).await.unwrap(),
        records
    );
}
both_stores!(a_mandate_is_listed_revoked_and_named_in_the_feed, revoking);

// ----- stopping an agent, and a person leaving (review findings GR-152 and GR-153) -------

async fn mandate_id(issued: &Envelope) -> Id {
    as_a_gate(issued, &RevocationSet::default()).unwrap()[0]
        .id
        .clone()
}

async fn is_revoked<S: Store>(a: &Authority<S>, mandate: &Id) -> bool {
    let kept = a.store().mandate(&scope(), mandate).await.unwrap().unwrap();
    let in_feed = a
        .store()
        .revocations_after(&scope(), 0)
        .await
        .unwrap()
        .iter()
        .any(|e| e.kind == Revocable::Mandate && &e.id == mandate);
    // The two are written together or not at all.
    assert_eq!(kept.revoked, in_feed);
    kept.revoked
}

async fn published(store: Failing) -> Authority<Failing> {
    let (a, _, _, _) = ready(store).await;
    a.publish_bundle(&id("ana"), &policy(1, vec![mandates_key().public()]), NOW)
        .await
        .unwrap();
    a
}

/// A mandate issued after the stop has looked for mandates and before the agent is marked
/// is revoked all the same: the stop looks again once nothing more can be issued.
#[tokio::test]
async fn a_mandate_issued_while_its_agent_is_being_stopped_is_revoked() {
    use std::sync::atomic::Ordering::SeqCst;
    for disable in [false, true] {
        let a = published(Failing::default()).await;
        let (ana, ben) = (id("ana"), id("ben"));
        let good = ask("refund-bot", "refund-bot:k1", "triage");
        let m = mandate_id(&a.issue_mandate(&ben, &good, NOW).await.unwrap()).await;
        // The stop's first look does not see it: it was issued a moment too late for that.
        a.store().late.store(UNTIL_AGENT_STOPPED, SeqCst);
        if disable {
            a.disable_agent(&ana, &id("refund-bot"), NOW).await.unwrap();
        } else {
            a.suspend_agent(&ana, &id("refund-bot"), NOW).await.unwrap();
        }
        assert!(is_revoked(&a, &m).await, "disable: {disable}");
        // And once the agent is marked, nothing more is issued to it.
        assert!(a.issue_mandate(&ben, &good, NOW).await.is_err());
        // The records, in order: the agent stopped, then the mandate revoked.
        let records = chain(&a).await;
        let last: Vec<AdminAction> = records.iter().rev().take(2).map(|r| r.action).collect();
        let stopped = if disable {
            AdminAction::AgentDisabled
        } else {
            AdminAction::AgentSuspended
        };
        assert_eq!(last, [AdminAction::Revoked, stopped]);
    }
}

/// A stop that was interrupted after the agent was marked is finished by asking again:
/// the answer is still that the agent is in that state, and its mandates are revoked.
#[tokio::test]
async fn a_stop_that_did_not_finish_is_finished_by_asking_again() {
    use std::sync::atomic::Ordering::SeqCst;
    let a = published(Failing::default()).await;
    let (ana, ben) = (id("ana"), id("ben"));
    let good = ask("refund-bot", "refund-bot:k1", "triage");
    let m = mandate_id(&a.issue_mandate(&ben, &good, NOW).await.unwrap()).await;
    // Neither look sees the mandate: as if the Authority stopped right after the mark.
    a.store().late.store(UNTIL_TOLD, SeqCst);
    a.suspend_agent(&ana, &id("refund-bot"), NOW).await.unwrap();
    a.store().late.store(0, SeqCst);
    assert!(!is_revoked(&a, &m).await);

    assert!(matches!(
        a.suspend_agent(&ana, &id("refund-bot"), NOW).await,
        Err(AuthorityError::Unknown("agent in that state"))
    ));
    assert!(is_revoked(&a, &m).await);
    // Asking a third time writes nothing more.
    let before = chain(&a).await.len();
    assert!(a.suspend_agent(&ana, &id("refund-bot"), NOW).await.is_err());
    assert_eq!(chain(&a).await.len(), before);
    // Only an admin can make the Authority do that work.
    assert!(matches!(
        a.suspend_agent(&ben, &id("refund-bot"), NOW).await,
        Err(AuthorityError::NotAllowed)
    ));
}

/// A person a template names can issue to an agent someone else owns. When that person
/// leaves, those mandates are revoked too; the agent, and what its own owner issued to it,
/// are left alone.
async fn leaving<S: Store>(store: S) {
    let (a, _, _, _) = ready(store).await;
    let (ana, ben, cy) = (id("ana"), id("ben"), id("cy"));
    a.publish_bundle(&ana, &policy(1, vec![mandates_key().public()]), NOW)
        .await
        .unwrap();
    let by_cy = a
        .issue_mandate(&cy, &ask("refund-bot", "refund-bot:k1", "shared"), NOW)
        .await
        .unwrap();
    let by_cy = mandate_id(&by_cy).await;
    let by_ben = a
        .issue_mandate(&ben, &ask("refund-bot", "refund-bot:k1", "triage"), NOW)
        .await
        .unwrap();
    let by_ben = mandate_id(&by_ben).await;
    let cys_own = a
        .issue_mandate(&cy, &ask("cy-bot", "cy-bot:k1", "triage"), NOW)
        .await
        .unwrap();
    let cys_own = mandate_id(&cys_own).await;
    let before = chain(&a).await.len();

    a.offboard_person(&ana, &cy, NOW).await.unwrap();
    assert!(is_revoked(&a, &by_cy).await);
    assert!(is_revoked(&a, &cys_own).await);
    assert!(!is_revoked(&a, &by_ben).await);
    let refund = a
        .store()
        .agent(&scope(), &id("refund-bot"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refund.status, Status::Active);
    // Two mandates revoked, one agent suspended, one person marked: four records, the
    // person's last.
    let records = chain(&a).await;
    assert_eq!(records.len(), before + 4);
    assert_eq!(
        records.last().unwrap().action,
        AdminAction::PersonOffboarded
    );
    // Gone, cy issues nothing, and nothing is issued to cy's agent.
    assert!(a
        .issue_mandate(&cy, &ask("refund-bot", "refund-bot:k1", "shared"), NOW)
        .await
        .is_err());
    // Asking again finds nothing left to do but say so once more.
    a.offboard_person(&ana, &cy, NOW).await.unwrap();
    assert_eq!(chain(&a).await.len(), before + 5);
}
both_stores!(
    a_leavers_mandates_for_other_peoples_agents_are_revoked,
    leaving
);

/// A mandate the leaver issued while the offboarding was looking is revoked all the same.
#[tokio::test]
async fn a_mandate_issued_while_its_issuer_is_leaving_is_revoked() {
    use std::sync::atomic::Ordering::SeqCst;
    let a = published(Failing::default()).await;
    let (ana, cy) = (id("ana"), id("cy"));
    let by_cy = a
        .issue_mandate(&cy, &ask("refund-bot", "refund-bot:k1", "shared"), NOW)
        .await
        .unwrap();
    let by_cy = mandate_id(&by_cy).await;
    a.store().late.store(UNTIL_PERSON_GONE, SeqCst);
    a.offboard_person(&ana, &cy, NOW).await.unwrap();
    assert!(is_revoked(&a, &by_cy).await);
    let records = chain(&a).await;
    let last: Vec<AdminAction> = records.iter().rev().take(2).map(|r| r.action).collect();
    assert_eq!(last, [AdminAction::Revoked, AdminAction::PersonOffboarded]);
}

// ----- G4d-1: a gate's renewal, and what it does with the answer (spec §7.6.3) -----------

mod renewal {
    use super::*;
    use gov_core::lease::Lease;
    use gov_core::policy::{open_bundle, HeldBundle};
    use gov_core::renew::{Renew, RenewRefused, Renewed, Standing};
    use gov_core::revocation::{RevocationFeed, Revocations};

    fn gate_key(name: &str) -> SigningKey {
        // A key each: the same public key is never enrolled twice.
        let seed = 30 + name.bytes().last().unwrap() % 10;
        key(&format!("{name}:k1"), seed)
    }

    async fn enrol_gate<S: Store>(a: &Authority<S>, name: &str) -> SigningKey {
        let k = gate_key(name);
        let code = a
            .offer_enrolment(&id("ana"), EnrolKind::Gate, &id(name), NOW)
            .await
            .unwrap();
        let asked = request(&k, EnrolKind::Gate, name, k.public(), &code);
        a.enrol(&code, &asked, NOW).await.unwrap();
        k
    }

    fn standing(gate: &str) -> Standing {
        Standing::new(scope(), id(gate), id("authority"), root(), None)
    }

    /// An Authority with people, agents, an enrolled gate and its first bundle.
    async fn serving<S: Store>(store: S) -> (Authority<S>, SigningKey) {
        let (a, _, _, _) = ready(store).await;
        let gate = enrol_gate(&a, "gate-1").await;
        a.publish_bundle(&id("ana"), &policy(1, vec![mandates_key().public()]), NOW)
            .await
            .unwrap();
        (a, gate)
    }

    /// What a gate holds, to compare before and after an answer it refuses.
    fn held(s: &Standing) -> (Option<Lease>, Option<HeldBundle>, u64, Option<i64>) {
        (
            s.lease().cloned(),
            s.bundle().map(|b| b.held()),
            s.revocations_through(),
            s.now(9_000_000),
        )
    }

    async fn round<S: Store>(
        a: &Authority<S>,
        s: &mut Standing,
        gate: &SigningKey,
        mono_ms: u64,
        now: i64,
    ) -> gov_core::renew::Taken {
        let (request, asked) = s.ask(gate, mono_ms).unwrap();
        let answer = a.renew(&request, now).await.unwrap();
        s.take(&answer, asked).unwrap()
    }

    /// One request brings the lease, the feed and, when the gate lacks it, the bundle; the
    /// gate's time, its revoked set and its bundle follow the Authority's.
    async fn renewing<S: Store>(store: S) {
        let (a, gate) = serving(store).await;
        let (ana, ben) = (id("ana"), id("ben"));
        let records = chain(&a).await.len();
        let mut s = standing("gate-1");
        assert!(s.now(0).is_none() && !s.fresh(0));

        // The first renewal brings the bundle with it.
        let (request, asked) = s.ask(&gate, 1_000).unwrap();
        let answer = a.renew(&request, NOW).await.unwrap();
        assert!(answer.bundle.is_some());
        let taken = s.take(&answer, asked).unwrap();
        assert!(taken.bundle_changed && taken.revoked.is_empty());
        assert_eq!(s.bundle().unwrap().bundle.number, 1);
        // The gate's time is the Authority's, counted on from when the request left.
        assert_eq!(s.now(1_000), Some(NOW));
        assert_eq!(s.now(6_999), Some(NOW + 5));
        // Complete, and fresh for as long as the bundle's own limit says: two minutes.
        assert!(s.fresh(1_000) && s.fresh(121_000));
        assert!(!s.fresh(121_001));
        // Still the gate's clock, however old the lease.
        assert_eq!(s.now(1_000 + 3_600_000), Some(NOW + 3600));

        // The next brings no bundle: the gate said which it holds.
        let (request, asked) = s.ask(&gate, 11_000).unwrap();
        let answer = a.renew(&request, NOW + 10).await.unwrap();
        assert!(answer.bundle.is_none());
        let taken = s.take(&answer, asked).unwrap();
        assert!(!taken.bundle_changed && taken.revoked.is_empty());
        assert!(s.fresh(131_000) && !s.fresh(131_001));

        // A mandate is revoked: the next renewal says so, once.
        let good = ask("refund-bot", "refund-bot:k1", "triage");
        let m = as_a_gate(
            &a.issue_mandate(&ben, &good, NOW).await.unwrap(),
            &RevocationSet::default(),
        )
        .unwrap()[0]
            .id
            .clone();
        assert!(!s.revoked().is_revoked(Revocable::Mandate, &m));
        a.revoke_mandate(&ben, &m, NOW + 12).await.unwrap();
        let taken = round(&a, &mut s, &gate, 21_000, NOW + 20).await;
        assert_eq!(taken.revoked.len(), 1);
        assert_eq!(
            (taken.revoked[0].kind, &taken.revoked[0].id),
            (Revocable::Mandate, &m)
        );
        assert!(s.revoked().is_revoked(Revocable::Mandate, &m));
        assert_eq!(
            (s.revocations_through(), s.lease().unwrap().revocation_seq),
            (1, 1)
        );
        let taken = round(&a, &mut s, &gate, 31_000, NOW + 30).await;
        assert!(taken.revoked.is_empty() && !taken.bundle_changed);

        // An agent is disabled, and a new bundle published: both arrive together.
        a.disable_agent(&ana, &id("helper-bot"), NOW + 31)
            .await
            .unwrap();
        a.publish_bundle(&ana, &policy(2, vec![mandates_key().public()]), NOW + 32)
            .await
            .unwrap();
        let taken = round(&a, &mut s, &gate, 41_000, NOW + 40).await;
        assert!(taken.bundle_changed);
        assert_eq!(s.bundle().unwrap().bundle.number, 2);
        assert!(s.revoked().is_revoked(Revocable::Agent, &id("helper-bot")));
        assert!(s.fresh(41_000));

        // A gate started again has no lease and no revoked set: it asks from nothing and
        // is told everything still worth telling.
        let mut again = standing("gate-1");
        let taken = round(&a, &mut again, &gate, 5, NOW + 50).await;
        assert!(taken.bundle_changed);
        assert_eq!(again.revocations_through(), s.revocations_through());
        assert!(again.revoked().is_revoked(Revocable::Mandate, &m));
        assert!(again
            .revoked()
            .is_revoked(Revocable::Agent, &id("helper-bot")));

        // A renewal writes nothing: every record since the start is one of the acts above.
        let acts = chain(&a).await.len() - records;
        assert_eq!(acts, 4, "issued, revoked, agent disabled, published");

        // A disabled gate gets no answer, so it has no fresh lease from then on (GR-167).
        a.disable_gate(&ana, &id("gate-1"), NOW + 60).await.unwrap();
        let (request, _) = s.ask(&gate, 71_000).unwrap();
        assert!(matches!(
            a.renew(&request, NOW + 70).await,
            Err(AuthorityError::Refused)
        ));
        assert!(s.fresh(41_000 + 120_000) && !s.fresh(41_000 + 120_001));
    }
    both_stores!(a_renewal_brings_the_lease_the_feed_and_the_bundle, renewing);

    /// Who the Authority answers, and who gets a bare refusal.
    #[tokio::test]
    async fn only_an_enrolled_gate_that_is_not_disabled_is_answered() {
        let (a, gate) = serving(MemoryStore::default()).await;
        let s = standing("gate-1");
        let (good, _) = s.ask(&gate, 0).unwrap();
        let refused = |r: Result<Renewed, AuthorityError>| {
            assert!(matches!(r, Err(AuthorityError::Refused)), "{:?}", r.err());
        };
        // A key nobody enrolled, under the gate's own key id and under another.
        let (forged, _) = s.ask(&key("gate-1:k1", 99), 0).unwrap();
        refused(a.renew(&forged, NOW).await);
        let (stranger, _) = s.ask(&key("gate-1:k9", 99), 0).unwrap();
        refused(a.renew(&stranger, NOW).await);
        // An agent's enrolled key is not a gate's.
        let (agent, _) = s.ask(&key("refund-bot:k1", 9), 0).unwrap();
        refused(a.renew(&agent, NOW).await);
        // A gate asking in another gate's name.
        a.register_gate(&id("ana"), &id("gate-2"), NOW)
            .await
            .unwrap();
        let (other, _) = standing("gate-2").ask(&gate, 0).unwrap();
        refused(a.renew(&other, NOW).await);
        // A request meant for another Authority.
        let elsewhere = Standing::new(scope(), id("gate-1"), id("authority-2"), root(), None);
        let (misdirected, _) = elsewhere.ask(&gate, 0).unwrap();
        refused(a.renew(&misdirected, NOW).await);
        // Something signed by the gate that is not a renewal at all.
        let not_a_renewal = request(&gate, EnrolKind::Gate, "gate-1", gate.public(), "code");
        refused(a.renew(&not_a_renewal, NOW).await);

        // The gate itself is answered, and not twice in one second.
        a.renew(&good, NOW).await.unwrap();
        let (second, _) = s.ask(&gate, 0).unwrap();
        assert!(matches!(
            a.renew(&second, NOW).await,
            Err(AuthorityError::TooSoon)
        ));
        // A refused request does not use the second up for the gate.
        refused(a.renew(&forged, NOW + 1).await);
        a.renew(&second, NOW + 1).await.unwrap();
        // Another gate's second is its own.
        let gate_2 = enrol_gate(&a, "gate-2").await;
        let (theirs, _) = standing("gate-2").ask(&gate_2, 0).unwrap();
        a.renew(&theirs, NOW + 1).await.unwrap();

        // With no bundle published there is nothing to lease from.
        let (bare, _, _, _) = ready(MemoryStore::default()).await;
        let gate = enrol_gate(&bare, "gate-1").await;
        let (request, _) = standing("gate-1").ask(&gate, 0).unwrap();
        assert!(matches!(
            bare.renew(&request, NOW).await,
            Err(AuthorityError::Unknown("published bundle"))
        ));
        // Nor with a bundle that does not list this Authority's leases key.
        let other = key("authority:leases-9", 78).public();
        bare.publish_bundle(
            &id("ana"),
            &policy_with(1, vec![mandates_key().public()], other),
            NOW,
        )
        .await
        .unwrap();
        assert!(matches!(
            bare.renew(&request, NOW).await,
            Err(AuthorityError::Unknown("leases key in the bundle"))
        ));
    }

    /// An agent and a gate never share a name, so an agent's key is never under a gate's.
    /// Were one ever written there all the same, it would still not be answered: the key
    /// must be one enrolled for a gate.
    #[tokio::test]
    async fn an_agents_key_under_a_gates_name_is_not_answered() {
        let (a, gate) = serving(MemoryStore::default()).await;
        // Written straight into the store, past the Authority's own checks.
        let planted = key("gate-1:k7", 88);
        let records = a.store().records_after(&scope(), 0, 1000).await.unwrap();
        let next = StoredRecord {
            seq: records.last().unwrap().seq + 1,
            ..records.last().unwrap().clone()
        };
        let as_agent = Effect::AddKey(EnrolledKey {
            kind: EnrolKind::Agent,
            name: id("gate-1"),
            key: planted.public(),
        });
        a.store()
            .commit(&scope(), &next, &[as_agent])
            .await
            .unwrap();

        let s = standing("gate-1");
        let (request, _) = s.ask(&planted, 0).unwrap();
        assert!(matches!(
            a.renew(&request, NOW).await,
            Err(AuthorityError::Refused)
        ));
        // The gate's own key is answered as before.
        let (request, _) = s.ask(&gate, 0).unwrap();
        a.renew(&request, NOW).await.unwrap();
    }

    fn lease_of(answer: &Renewed) -> Lease {
        let keys = KeySet::new([leases_key().public()]);
        open::<Lease>(&answer.lease, &keys, &scope()).unwrap().doc
    }
    fn feed_of(answer: &Renewed) -> RevocationFeed {
        let keys = KeySet::new([leases_key().public()]);
        open::<RevocationFeed>(&answer.feed, &keys, &scope())
            .unwrap()
            .doc
    }

    /// Every part of an answer is checked, and one that fails any check changes nothing.
    #[tokio::test]
    async fn a_gate_takes_the_whole_answer_or_none_of_it() {
        let (a, gate) = serving(MemoryStore::default()).await;
        let ben = id("ben");
        // Something is revoked, so that the feed has an entry to tamper with.
        let good = ask("refund-bot", "refund-bot:k1", "triage");
        for _ in 0..2 {
            let m = as_a_gate(
                &a.issue_mandate(&ben, &good, NOW).await.unwrap(),
                &RevocationSet::default(),
            )
            .unwrap()[0]
                .id
                .clone();
            a.revoke_mandate(&ben, &m, NOW).await.unwrap();
        }
        let mut s = standing("gate-1");
        round(&a, &mut s, &gate, 1_000, NOW + 1).await;
        let before = held(&s);
        assert_eq!(before.2, 2);

        // An honest answer to this request, to change one part of at a time.
        let mut now = NOW + 10;
        let mut attempt = 0u64;
        let refuse = |s: &mut Standing,
                      change: &dyn Fn(&mut Renewed),
                      why: RenewRefused,
                      answer: Renewed,
                      asked| {
            let mut answer = answer;
            change(&mut answer);
            assert_eq!(s.take(&answer, asked), Err(why), "{}", why.code());
            assert_eq!(
                held(s),
                before,
                "{} changed what the gate holds",
                why.code()
            );
        };
        macro_rules! refused {
            ($why:expr, $change:expr) => {{
                now += 1;
                attempt += 1;
                let (request, asked) = s.ask(&gate, 20_000 + attempt).unwrap();
                let answer = a.renew(&request, now).await.unwrap();
                refuse(&mut s, &$change, $why, answer, asked);
            }};
        }
        let resign_lease =
            |answer: &mut Renewed, change: &dyn Fn(&mut Lease), with: &SigningKey| {
                let mut lease = lease_of(answer);
                change(&mut lease);
                answer.lease = seal(&lease, with).unwrap();
            };
        let resign_feed =
            |answer: &mut Renewed, change: &dyn Fn(&mut RevocationFeed), with: &SigningKey| {
                let mut feed = feed_of(answer);
                change(&mut feed);
                answer.feed = seal(&feed, with).unwrap();
            };
        let leases = leases_key();
        let mandates = mandates_key();

        // --- the lease
        // Signed with a key the bundle lists for something else, or lists nowhere.
        refused!(RenewRefused::Lease, |r: &mut Renewed| resign_lease(
            r,
            &|_| {},
            &mandates
        ));
        refused!(RenewRefused::Lease, |r: &mut Renewed| resign_lease(
            r,
            &|_| {},
            &key("authority:leases-1", 99)
        ));
        // Another Authority's lease, under a key this bundle does list.
        refused!(RenewRefused::Lease, |r: &mut Renewed| resign_lease(
            r,
            &|l| l.issuer = id("authority-2"),
            &leases
        ));
        // It names a bundle the gate does not have in hand.
        refused!(RenewRefused::WrongBundle, |r: &mut Renewed| resign_lease(
            r,
            &|l| l.policy_version = id("2-0123456789abcdef"),
            &leases
        ));
        // For another gate, or answering a request this gate did not make.
        refused!(
            RenewRefused::NotThisRequest,
            |r: &mut Renewed| resign_lease(r, &|l| l.gate = id("gate-2"), &leases)
        );
        refused!(
            RenewRefused::NotThisRequest,
            |r: &mut Renewed| resign_lease(r, &|l| l.nonce = id("n-someone-elses"), &leases)
        );
        // Behind what the gate holds, in time or in revocations (GR-168).
        refused!(RenewRefused::AuthorityBehind, |r: &mut Renewed| {
            resign_lease(r, &|l| l.authority_time = NOW, &leases)
        });
        refused!(RenewRefused::AuthorityBehind, |r: &mut Renewed| {
            resign_lease(r, &|l| l.revocation_seq = 1, &leases)
        });

        // --- the feed
        refused!(RenewRefused::Feed, |r: &mut Renewed| resign_feed(
            r,
            &|_| {},
            &mandates
        ));
        refused!(RenewRefused::Feed, |r: &mut Renewed| resign_feed(
            r,
            &|f| f.issuer = id("authority-2"),
            &leases
        ));
        // It does not start where the gate has read to: something between would be missed.
        refused!(RenewRefused::Feed, |r: &mut Renewed| resign_feed(
            r,
            &|f| {
                f.after = 3;
                f.through = 3;
            },
            &leases
        ));
        // It stops short of the number the lease says the gate must hold.
        refused!(RenewRefused::Feed, |r: &mut Renewed| {
            resign_lease(r, &|l| l.revocation_seq = 3, &leases);
        });
        // It leaves a number out without saying so.
        refused!(RenewRefused::Feed, |r: &mut Renewed| resign_feed(
            r,
            &|f| f.through = 5,
            &leases
        ));

        // --- the bundle
        let offered = |bundle: Envelope| move |r: &mut Renewed| r.bundle = Some(bundle.clone());
        // Not signed by the root key.
        let mut unsigned = serde_json::to_value(policy(1, vec![mandates_key().public()])).unwrap();
        unsigned["kid"] = serde_json::json!("authority-root-9");
        refused!(
            RenewRefused::Bundle,
            offered(serde_json::from_value(unsigned).unwrap())
        );
        // Signed by the root, for another Authority.
        refused!(
            RenewRefused::Bundle,
            offered(signed_bundle(2, "authority-2", &[], &root_key()))
        );
        // Another bundle under the number the gate already holds.
        refused!(
            RenewRefused::BundleForked,
            offered(signed_bundle(1, "authority", &["extra"], &root_key()))
        );

        // After all of that the honest answer is still taken, and an answer taken once is
        // not taken again.
        let (request, asked) = s.ask(&gate, 30_000).unwrap();
        let answer = a.renew(&request, now + 1).await.unwrap();
        s.take(&answer, asked).unwrap();
        assert_ne!(held(&s), before);
        let (_, another) = s.ask(&gate, 31_000).unwrap();
        let after = held(&s);
        assert_eq!(s.take(&answer, another), Err(RenewRefused::NotThisRequest));
        assert_eq!(held(&s), after);
    }

    /// A gate is never taken back to an older bundle, and takes no lease before it has the
    /// bundle the lease names.
    #[tokio::test]
    async fn a_gate_does_not_go_back_to_an_older_bundle() {
        let (a, gate) = serving(MemoryStore::default()).await;
        let ana = id("ana");
        let first = a.latest_bundle().await.unwrap().unwrap().envelope;
        let mut s = standing("gate-1");
        round(&a, &mut s, &gate, 1_000, NOW + 1).await;
        a.publish_bundle(&ana, &policy(2, vec![mandates_key().public()]), NOW + 2)
            .await
            .unwrap();
        round(&a, &mut s, &gate, 2_000, NOW + 3).await;
        assert_eq!(s.bundle().unwrap().bundle.number, 2);
        let before = held(&s);

        // Bundle 1 again, with a lease for it: both were signed by the right keys, once.
        let (request, asked) = s.ask(&gate, 3_000).unwrap();
        let mut answer = a.renew(&request, NOW + 4).await.unwrap();
        let old = open_bundle(&first, &root(), &scope()).unwrap();
        let mut lease = lease_of(&answer);
        lease.policy_version = old.policy_version();
        answer.lease = seal(&lease, &leases_key()).unwrap();
        answer.bundle = Some(first.clone());
        assert_eq!(s.take(&answer, asked), Err(RenewRefused::BundleOlder));
        // The same lease without the bundle: it names one the gate does not hold.
        let (_, asked) = s.ask(&gate, 3_500).unwrap();
        answer.bundle = None;
        assert_eq!(s.take(&answer, asked), Err(RenewRefused::WrongBundle));
        assert_eq!(held(&s), before);

        // A gate with nothing, handed a lease and no bundle, takes neither.
        let mut empty = standing("gate-1");
        let (request, asked) = empty.ask(&gate, 10).unwrap();
        let mut answer = a.renew(&request, NOW + 5).await.unwrap();
        answer.bundle = None;
        assert_eq!(empty.take(&answer, asked), Err(RenewRefused::WrongBundle));
        assert!(empty.lease().is_none() && empty.bundle().is_none());

        // A gate started with a bundle file that is older than what the Authority
        // publishes is brought forward by its first renewal (GR-132)...
        let mut stale = Standing::new(scope(), id("gate-1"), id("authority"), root(), Some(old));
        assert!(!stale.fresh(0));
        let taken = round(&a, &mut stale, &gate, 20, NOW + 6).await;
        assert!(taken.bundle_changed && stale.fresh(20));
        assert_eq!(stale.bundle().unwrap().bundle.number, 2);
        // ...and one started with a newer bundle than its Authority publishes keeps its
        // own, and has no lease: something handed it that bundle, or the Authority was
        // put back.
        let third =
            open_bundle(&policy(3, vec![mandates_key().public()]), &root(), &scope()).unwrap();
        let mut ahead = Standing::new(scope(), id("gate-1"), id("authority"), root(), Some(third));
        let (request, asked) = ahead.ask(&gate, 30).unwrap();
        let answer = a.renew(&request, NOW + 7).await.unwrap();
        assert_eq!(ahead.take(&answer, asked), Err(RenewRefused::BundleOlder));
        assert_eq!(ahead.bundle().unwrap().bundle.number, 3);
        assert!(ahead.lease().is_none() && !ahead.fresh(30));
    }

    /// An Authority whose database was put back from an older copy answers from where it
    /// stands, and every gate that has heard more refuses it, under a reason of its own
    /// (review finding GR-168).
    #[tokio::test]
    async fn an_authority_that_has_gone_back_is_refused_as_behind() {
        let (ahead, gate) = serving(MemoryStore::default()).await;
        let ben = id("ben");
        let good = ask("refund-bot", "refund-bot:k1", "triage");
        for _ in 0..3 {
            let m = as_a_gate(
                &ahead.issue_mandate(&ben, &good, NOW).await.unwrap(),
                &RevocationSet::default(),
            )
            .unwrap()[0]
                .id
                .clone();
            ahead.revoke_mandate(&ben, &m, NOW).await.unwrap();
        }
        let mut s = standing("gate-1");
        round(&ahead, &mut s, &gate, 1_000, NOW + 1).await;
        assert_eq!(s.revocations_through(), 3);
        let before = held(&s);

        // The same installation as it was before those three revocations.
        let (behind, _) = serving(MemoryStore::default()).await;
        let (request, asked) = s.ask(&gate, 2_000).unwrap();
        // The gate names a number this Authority has never reached: it still answers.
        let answer = behind.renew(&request, NOW + 100).await.unwrap();
        assert_eq!(lease_of(&answer).revocation_seq, 0);
        assert_eq!(s.take(&answer, asked), Err(RenewRefused::AuthorityBehind));
        assert_eq!(held(&s), before);

        // And a clock set back, with nothing else wrong.
        let (request, asked) = s.ask(&gate, 3_000).unwrap();
        let answer = ahead.renew(&request, NOW - 3600).await.unwrap();
        assert_eq!(s.take(&answer, asked), Err(RenewRefused::AuthorityBehind));
        assert_eq!(held(&s), before);
        // Once it has caught up, the gate takes its leases again.
        round(&ahead, &mut s, &gate, 4_000, NOW + 200).await;
    }

    /// The renewal is a signed document of its own kind: nothing else the gate signs is
    /// taken for one, and it carries no time.
    #[test]
    fn a_renewal_says_what_the_gate_holds_and_no_time() {
        let gate = gate_key("gate-1");
        let s = standing("gate-1");
        let (request, asked) = s.ask(&gate, 77).unwrap();
        let keys = KeySet::new([gate.public()]);
        let renew = open::<Renew>(&request, &keys, &scope()).unwrap().doc;
        assert_eq!(
            (
                &renew.gate,
                &renew.authority,
                &renew.bundle,
                renew.revocations_through
            ),
            (&id("gate-1"), &id("authority"), &None, 0)
        );
        assert_eq!((&asked.nonce, asked.sent_mono_ms), (&renew.nonce, 77));
        // Two requests never share a nonce.
        let (again, _) = s.ask(&gate, 77).unwrap();
        assert_ne!(
            open::<Renew>(&again, &keys, &scope()).unwrap().doc.nonce,
            renew.nonce
        );
        // It is not a lease, a feed or a request to enrol, whoever signs it.
        assert!(open::<Lease>(&request, &keys, &scope()).is_err());
        assert!(open::<RevocationFeed>(&request, &keys, &scope()).is_err());
        assert!(open::<EnrolRequest>(&request, &keys, &scope()).is_err());
        let text = {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&request.payload)
                .unwrap();
            String::from_utf8(bytes).unwrap()
        };
        assert!(!text.contains("\"at\"") && !text.contains("time"));
    }
}
