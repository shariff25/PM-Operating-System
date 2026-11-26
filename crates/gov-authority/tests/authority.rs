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
}
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
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::Other("the disk is full".into()));
        }
        self.inner.commit(scope, record, effects).await
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
