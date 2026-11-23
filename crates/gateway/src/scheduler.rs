//! Who runs next on the one GPU (MOD-4). At most `max_in_flight` requests run at once, each
//! person at most `per_user` of them, and each client (gateway token) at most its own cap
//! (review finding N3: one token can no longer hold every slot). A waiting request is started
//! in priority order: someone waiting in Slack (interactive) before a routine (scheduled)
//! before a personal agent's background work, and first come, first served within a priority.
//!
//! Waiting is capped the same way (review finding R2): at most `max_queue` requests wait in
//! all, each client at most its own `client_queue` of them and each believed person at most
//! `per-user-queue` through each client, so one token can't fill the queue and turn everyone
//! else away. The person's cap counts per client (finding F1 of the review at 2544d01): before,
//! a team bot's token naming one PM (`bot-docs`, `user: any`) could fill that PM's places and
//! the PM's own calls through the runtime got an immediate `429`.
//!
//! Running, a person's slots are shared out by client (U1, review at c78b5fb): each client
//! holds at most its `person_share` of them, and the clients that may name anyone (the
//! runtime, the team bots; [`Owner::with_shared_cap`]) at most `shared_cap` of them
//! **together**, so with `per-user` 2 and a PM's own bot configured, one slot is only ever
//! taken by that bot. Before, the share was per client only, so two such clients (`bot-docs`
//! and `runtime`) could hold both of a PM's slots and the PM's own bot waited.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::config::Limits;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Personal,
    Scheduled,
    Interactive,
}

impl Priority {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "interactive" => Some(Self::Interactive),
            "scheduled" => Some(Self::Scheduled),
            "personal" => Some(Self::Personal),
            _ => None,
        }
    }
}

/// The request would have to wait and may not: the caller should try again shortly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    /// The whole queue is full (`max_queue`).
    Full,
    /// This client already has its `client_queue` requests waiting.
    Client,
    /// This person already has `per-user-queue` requests waiting through this client.
    Person,
}

/// Whose a request is, for the caps: the person (when the gateway believes one; capped at
/// `per_user` running and `per-user-queue` waiting), and the client that sent it (capped at
/// `client_cap` running and `client_queue` waiting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub person: Option<String>,
    pub client: String,
    pub client_cap: usize,
    pub client_queue: usize,
    /// The most of this person's running slots this client may hold (G3, review at
    /// 6bb806f): `ClientPolicy::per_user_share`.
    pub person_share: usize,
    /// For a client that may name anyone: the most of this person's running slots that all
    /// such clients together may hold (U1): `ClientPolicy::shared_person_cap`. `None` for a
    /// PM's own bot's client and for a call that names no one.
    pub shared_cap: Option<usize>,
}

impl Owner {
    /// A request of `client`'s, for `person` when there is one, with room to wait for twice
    /// `client_cap` (see [`Owner::with_queue`]).
    pub fn new(person: Option<&str>, client: &str, client_cap: usize) -> Self {
        Self {
            person: person.map(str::to_string),
            client: client.to_string(),
            client_cap,
            client_queue: client_cap.saturating_mul(2),
            person_share: usize::MAX,
            shared_cap: None,
        }
    }

    /// The same, from a client that may name anyone: such clients together hold at most
    /// `cap` of the person's running slots (U1). `None` leaves the call out of that count.
    pub fn with_shared_cap(mut self, cap: Option<usize>) -> Self {
        self.shared_cap = cap;
        self
    }

    /// Counted among the calls of clients that may name anyone, for its person.
    fn shared(&self) -> bool {
        self.person.is_some() && self.shared_cap.is_some()
    }

    /// The same, with this client allowed at most `share` of the person's running slots.
    pub fn with_person_share(mut self, share: usize) -> Self {
        self.person_share = share;
        self
    }

    /// The same, with `client_queue` requests of this client allowed to wait.
    pub fn with_queue(mut self, client_queue: usize) -> Self {
        self.client_queue = client_queue;
        self
    }
}

struct Waiter {
    id: u64,
    priority: Priority,
    owner: Owner,
    tx: oneshot::Sender<()>,
}

#[derive(Default)]
struct State {
    in_flight: usize,
    per_user: HashMap<String, usize>,
    per_client: HashMap<String, usize>,
    /// Running calls by (client, person): each client's share of a person's slots (G3).
    per_client_person: HashMap<(String, String), usize>,
    /// Running calls by person, of clients that may name anyone, together (U1).
    per_user_shared: HashMap<String, usize>,
    waiting: Vec<Waiter>,
    /// Started but not yet picked up by their caller.
    granted: HashSet<u64>,
    next_id: u64,
}

pub struct Scheduler {
    limits: Limits,
    state: Mutex<State>,
}

/// A running slot. Dropping it frees the slot for the next waiter.
pub struct Permit {
    sched: Arc<Scheduler>,
    owner: Owner,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.sched.release(&self.owner);
    }
}

/// Cleans up a request that stops waiting (its caller went away, or its wait timed out): it
/// leaves the queue, or gives back a slot it was granted but never used.
struct Waiting {
    sched: Arc<Scheduler>,
    id: u64,
    owner: Owner,
    done: bool,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut st = self.sched.state.lock().expect("scheduler lock");
        if let Some(pos) = st.waiting.iter().position(|w| w.id == self.id) {
            st.waiting.remove(pos);
        } else if st.granted.remove(&self.id) {
            drop(st);
            self.sched.release(&self.owner);
        }
    }
}

fn decrement<K, Q>(map: &mut HashMap<K, usize>, key: &Q)
where
    K: std::borrow::Borrow<Q> + std::hash::Hash + Eq,
    Q: std::hash::Hash + Eq + ?Sized,
{
    if let Some(n) = map.get_mut(key) {
        *n = n.saturating_sub(1);
        if *n == 0 {
            map.remove(key);
        }
    }
}

impl Scheduler {
    pub fn new(limits: Limits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            state: Mutex::new(State::default()),
        })
    }

    /// Wait for a slot. Refuses at once with [`Busy`] when the queue is full. Dropping the
    /// future (a caller that hangs up, or a deadline) gives its place back.
    pub async fn acquire(
        self: &Arc<Self>,
        owner: &Owner,
        priority: Priority,
    ) -> Result<Permit, Busy> {
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut st = self.state.lock().expect("scheduler lock");
            st.next_id += 1;
            let id = st.next_id;
            st.waiting.push(Waiter {
                id,
                priority,
                owner: owner.clone(),
                tx,
            });
            self.dispatch(&mut st);
            // Refused only if it would have to wait and there's no room to: the queue is full
            // (`503`), or its client or its person already has as many waiting as it may
            // (`429`; they count this one). A request that can start now always starts.
            if let Some(pos) = st.waiting.iter().position(|w| w.id == id) {
                let client_waiting = st
                    .waiting
                    .iter()
                    .filter(|w| w.owner.client == owner.client)
                    .count();
                // Per client and person (F1): another client's calls for this person never
                // use this client's places for them, nor the other way round.
                let person_waiting = owner.person.as_ref().map_or(0, |p| {
                    st.waiting
                        .iter()
                        .filter(|w| {
                            w.owner.client == owner.client && w.owner.person.as_ref() == Some(p)
                        })
                        .count()
                });
                let refuse = if st.waiting.len() > self.limits.max_queue {
                    Some(Busy::Full)
                } else if client_waiting > owner.client_queue {
                    Some(Busy::Client)
                } else if person_waiting > self.limits.person_queue() {
                    Some(Busy::Person)
                } else {
                    None
                };
                if let Some(why) = refuse {
                    st.waiting.remove(pos);
                    return Err(why);
                }
            }
            id
        };
        let mut waiting = Waiting {
            sched: self.clone(),
            id,
            owner: owner.clone(),
            done: false,
        };
        // The sender is only dropped after a grant, so an error can't happen here.
        let _ = rx.await;
        self.state
            .lock()
            .expect("scheduler lock")
            .granted
            .remove(&id);
        waiting.done = true;
        Ok(Permit {
            sched: self.clone(),
            owner: owner.clone(),
        })
    }

    fn release(&self, owner: &Owner) {
        let mut st = self.state.lock().expect("scheduler lock");
        st.in_flight = st.in_flight.saturating_sub(1);
        if let Some(p) = &owner.person {
            decrement(&mut st.per_user, p);
            decrement(
                &mut st.per_client_person,
                &(owner.client.clone(), p.clone()),
            );
            if owner.shared() {
                decrement(&mut st.per_user_shared, p);
            }
        }
        decrement(&mut st.per_client, &owner.client);
        self.dispatch(&mut st);
    }

    /// Start waiters while there is room: the highest priority whose person and client are
    /// both under their caps, whose client is under its share of that person's slots (G3),
    /// and, for a client that may name anyone, whose person's calls from all such clients
    /// are under their shared cap (U1); oldest first.
    fn dispatch(&self, st: &mut State) {
        while st.in_flight < self.limits.max_in_flight {
            let pick = st
                .waiting
                .iter()
                .enumerate()
                .filter(|(_, w)| {
                    let person_ok = w.owner.person.as_ref().is_none_or(|p| {
                        st.per_user.get(p).copied().unwrap_or(0) < self.limits.per_user
                            && st
                                .per_client_person
                                .get(&(w.owner.client.clone(), p.clone()))
                                .copied()
                                .unwrap_or(0)
                                < w.owner.person_share
                            && w.owner.shared_cap.is_none_or(|cap| {
                                st.per_user_shared.get(p).copied().unwrap_or(0) < cap
                            })
                    });
                    let client_ok = st.per_client.get(&w.owner.client).copied().unwrap_or(0)
                        < w.owner.client_cap;
                    person_ok && client_ok
                })
                .max_by_key(|(_, w)| (w.priority, std::cmp::Reverse(w.id)))
                .map(|(i, _)| i);
            let Some(i) = pick else { break };
            let w = st.waiting.remove(i);
            if w.tx.send(()).is_ok() {
                st.in_flight += 1;
                if let Some(p) = w.owner.person.as_ref().filter(|_| w.owner.shared()) {
                    *st.per_user_shared.entry(p.clone()).or_default() += 1;
                }
                if let Some(p) = w.owner.person {
                    *st.per_client_person
                        .entry((w.owner.client.clone(), p.clone()))
                        .or_default() += 1;
                    *st.per_user.entry(p).or_default() += 1;
                }
                *st.per_client.entry(w.owner.client).or_default() += 1;
                st.granted.insert(w.id);
            }
        }
    }

    /// `(running, waiting)`, for readiness and tests.
    pub fn snapshot(&self) -> (usize, usize) {
        let st = self.state.lock().expect("scheduler lock");
        (st.in_flight, st.waiting.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sched(max_in_flight: usize, per_user: usize, max_queue: usize) -> Arc<Scheduler> {
        Scheduler::new(Limits {
            max_in_flight,
            per_user,
            max_queue,
            ..Limits::default()
        })
    }

    /// A person's request through a client with room for everything.
    fn who(person: &str) -> Owner {
        Owner::new(Some(person), "runtime", 64)
    }

    #[tokio::test]
    async fn higher_priority_waiters_go_first() {
        let s = sched(1, 4, 8);
        let first = s.acquire(&who("ana"), Priority::Personal).await.unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for (name, p) in [
            ("bo", Priority::Personal),
            ("cy", Priority::Scheduled),
            ("di", Priority::Interactive),
        ] {
            let s = s.clone();
            let order = order.clone();
            handles.push(tokio::spawn(async move {
                let _permit = s.acquire(&who(name), p).await.unwrap();
                order.lock().unwrap().push(name);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(s.snapshot(), (1, 3));
        drop(first);
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec!["di", "cy", "bo"]);
        assert_eq!(s.snapshot(), (0, 0));
    }

    #[tokio::test]
    async fn one_person_cant_take_every_slot() {
        let s = sched(2, 1, 8);
        let a1 = s.acquire(&who("ana"), Priority::Interactive).await.unwrap();
        // Ana's second request waits even though a slot is free; Bo's starts.
        let s2 = s.clone();
        let ana2 =
            tokio::spawn(async move { s2.acquire(&who("ana"), Priority::Interactive).await });
        tokio::time::sleep(Duration::from_millis(5)).await;
        let b1 = s.acquire(&who("bo"), Priority::Personal).await.unwrap();
        assert_eq!(s.snapshot(), (2, 1));
        drop(a1);
        let a2 = ana2.await.unwrap().unwrap();
        assert_eq!(s.snapshot(), (2, 0));
        drop((a2, b1));
        assert_eq!(s.snapshot(), (0, 0));
    }

    #[tokio::test]
    async fn one_client_cant_take_every_slot_whoever_it_names() {
        // Four slots, two per person, but the `evals` token may hold one, however many
        // people its calls name; another client's calls still start.
        let s = sched(4, 2, 8);
        let evals = |p: &str| Owner::new(Some(p), "evals", 1);
        let e1 = s
            .acquire(&evals("ana"), Priority::Interactive)
            .await
            .unwrap();
        let s2 = s.clone();
        let e2 = tokio::spawn(async move {
            s2.acquire(&Owner::new(Some("bo"), "evals", 1), Priority::Interactive)
                .await
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(s.snapshot(), (1, 1), "the evals client is at its cap");
        let r1 = s.acquire(&who("cy"), Priority::Personal).await.unwrap();
        assert_eq!(s.snapshot(), (2, 1));
        drop(e1);
        let e2 = e2.await.unwrap().unwrap();
        assert_eq!(s.snapshot(), (2, 0));
        drop((e2, r1));
        assert_eq!(s.snapshot(), (0, 0));
        // Without a person, a client's calls are bounded by its own cap alone.
        let s = sched(4, 1, 8);
        let a = s
            .acquire(&Owner::new(None, "worker", 3), Priority::Personal)
            .await
            .unwrap();
        let b = s
            .acquire(&Owner::new(None, "worker", 3), Priority::Personal)
            .await
            .unwrap();
        let c = s
            .acquire(&Owner::new(None, "worker", 3), Priority::Personal)
            .await
            .unwrap();
        assert_eq!(s.snapshot(), (3, 0));
        drop((a, b, c));
        assert_eq!(s.snapshot(), (0, 0));
    }

    #[tokio::test]
    async fn a_full_queue_is_refused_but_a_free_slot_is_used() {
        let s = sched(2, 1, 1);
        let _held = s.acquire(&who("ana"), Priority::Interactive).await.unwrap();
        // Ana's second request waits (she's at her cap) and fills the queue.
        let s2 = s.clone();
        let _waiter =
            tokio::spawn(async move { s2.acquire(&who("ana"), Priority::Interactive).await });
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(s.snapshot(), (1, 1));
        // Bo can start at once, so a full queue doesn't stop him.
        let _bo = s.acquire(&who("bo"), Priority::Interactive).await.unwrap();
        // Cy would have to wait, and there's no room to: refused, and nothing is left behind.
        assert!(matches!(
            s.acquire(&who("cy"), Priority::Interactive).await,
            Err(Busy::Full)
        ));
        assert_eq!(s.snapshot(), (2, 1));
    }

    /// Waits until the scheduler holds `want` (running, waiting).
    async fn settle(s: &Arc<Scheduler>, want: (usize, usize)) {
        for _ in 0..200 {
            if s.snapshot() == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the scheduler never reached {want:?}: {:?}", s.snapshot());
    }

    #[tokio::test]
    async fn one_client_cant_fill_the_queue_and_turn_others_away() {
        // R2: one slot, room for 8 to wait, but each client may have only 2 waiting.
        let s = sched(1, 8, 8);
        let busy = Owner::new(None, "worker", 1).with_queue(2);
        let held = s.acquire(&busy, Priority::Interactive).await.unwrap();
        let mut waiters = Vec::new();
        for _ in 0..2 {
            let (s2, o) = (s.clone(), busy.clone());
            waiters.push(tokio::spawn(async move {
                s2.acquire(&o, Priority::Interactive).await.map(drop)
            }));
        }
        settle(&s, (1, 2)).await;
        // Its third would wait too: refused, as the client's own excess.
        assert_eq!(
            s.acquire(&busy, Priority::Interactive).await.err(),
            Some(Busy::Client)
        );
        assert_eq!(s.snapshot(), (1, 2), "nothing is left behind");
        // Another client still gets its places in the queue.
        let other = Owner::new(None, "runtime", 1).with_queue(2);
        for _ in 0..2 {
            let (s2, o) = (s.clone(), other.clone());
            waiters.push(tokio::spawn(async move {
                s2.acquire(&o, Priority::Personal).await.map(drop)
            }));
        }
        settle(&s, (1, 4)).await;
        drop(held);
        for w in waiters {
            assert_eq!(w.await.unwrap(), Ok(()));
        }
        assert_eq!(s.snapshot(), (0, 0));
    }

    #[tokio::test]
    async fn one_person_cant_fill_the_queue_through_a_trusted_client() {
        // A trusted client may name many people; each may have `per-user-queue` waiting
        // (default twice `per-user`: here 2) through that client.
        let s = sched(1, 1, 16);
        assert_eq!(s.limits.person_queue(), 2);
        let ana = Owner::new(Some("ana"), "runtime", 8);
        let held = s.acquire(&ana, Priority::Interactive).await.unwrap();
        let mut waiters = Vec::new();
        for _ in 0..2 {
            let (s2, o) = (s.clone(), Owner::new(Some("ana"), "bot-docs", 8));
            waiters.push(tokio::spawn(async move {
                s2.acquire(&o, Priority::Interactive).await.map(drop)
            }));
        }
        settle(&s, (1, 2)).await;
        assert_eq!(
            s.acquire(
                &Owner::new(Some("ana"), "bot-docs", 8),
                Priority::Interactive
            )
            .await
            .err(),
            Some(Busy::Person)
        );
        // Bo, through the same client, still waits.
        let (s2, bo) = (s.clone(), Owner::new(Some("bo"), "bot-docs", 8));
        waiters.push(tokio::spawn(async move {
            s2.acquire(&bo, Priority::Interactive).await.map(drop)
        }));
        settle(&s, (1, 3)).await;
        drop(held);
        for w in waiters {
            assert_eq!(w.await.unwrap(), Ok(()));
        }
        // `per-user-queue` is configurable.
        let s = Scheduler::new(Limits {
            per_user_queue: Some(5),
            ..Limits::default()
        });
        assert_eq!(s.limits.person_queue(), 5);
    }

    #[tokio::test]
    async fn one_client_cant_take_all_of_a_persons_places() {
        // F1 (review at 2544d01): `bot-docs` (it may name anyone) fills every place Ana has
        // through it, as fast as it likes; Ana's own calls through the runtime still get
        // theirs, and wait rather than being refused.
        let s = sched(1, 1, 16);
        let person_queue = s.limits.person_queue();
        let held = s
            .acquire(&Owner::new(None, "other", 8), Priority::Interactive)
            .await
            .unwrap();
        let docs = Owner::new(Some("ana"), "bot-docs", 8);
        let mut waiters = Vec::new();
        for _ in 0..person_queue {
            let (s2, o) = (s.clone(), docs.clone());
            waiters.push(tokio::spawn(async move {
                s2.acquire(&o, Priority::Interactive).await.map(drop)
            }));
        }
        settle(&s, (1, person_queue)).await;
        for _ in 0..3 {
            assert_eq!(
                s.acquire(&docs, Priority::Interactive).await.err(),
                Some(Busy::Person),
                "bot-docs has used its places for Ana"
            );
        }
        let runtime = Owner::new(Some("ana"), "runtime", 8);
        for _ in 0..person_queue {
            let (s2, o) = (s.clone(), runtime.clone());
            waiters.push(tokio::spawn(async move {
                s2.acquire(&o, Priority::Interactive).await.map(drop)
            }));
        }
        settle(&s, (1, 2 * person_queue)).await;
        // The runtime's places for Ana are bounded the same way.
        assert_eq!(
            s.acquire(&runtime, Priority::Interactive).await.err(),
            Some(Busy::Person)
        );
        drop(held);
        for w in waiters {
            assert_eq!(w.await.unwrap(), Ok(()));
        }
        assert_eq!(s.snapshot(), (0, 0));
    }

    #[tokio::test]
    async fn a_client_holds_at_most_its_share_of_a_persons_slots() {
        // G3 (review at 6bb806f): Ana has two slots; `bot-docs` may hold one of them, however
        // early its calls came, so her own bot's call starts while bot-docs' second waits.
        let s = sched(4, 2, 16);
        let docs = Owner::new(Some("ana"), "bot-docs", 8).with_person_share(1);
        let d1 = s.acquire(&docs, Priority::Interactive).await.unwrap();
        let (s2, o) = (s.clone(), docs.clone());
        let d2 = tokio::spawn(async move { s2.acquire(&o, Priority::Interactive).await });
        settle(&s, (1, 1)).await;
        // Bo, through the same client, isn't held up by Ana's share.
        let bo = s
            .acquire(
                &Owner::new(Some("bo"), "bot-docs", 8).with_person_share(1),
                Priority::Interactive,
            )
            .await
            .unwrap();
        let own = Owner::new(Some("ana"), "bot-pm-ana", 8).with_person_share(2);
        let o1 = s.acquire(&own, Priority::Personal).await.unwrap();
        settle(&s, (3, 1)).await;
        // Ana is at her two now: bot-docs' second waits for her, not for its share.
        drop(d1);
        let d2 = d2.await.unwrap().unwrap();
        settle(&s, (3, 0)).await;
        drop((d2, bo, o1));
        assert_eq!(s.snapshot(), (0, 0));
    }

    #[tokio::test]
    async fn clients_that_may_name_anyone_together_leave_a_pm_their_own_bots_slot() {
        // U1 (review at c78b5fb): Ana has two slots. `runtime` and `bot-docs` may each hold
        // one (their share), and together hold at most one (the shared cap, `per-user - 1`
        // with a PM's own bot configured). Before, each client's share was counted alone, so
        // the two of them filled both slots and her own bot waited behind them.
        let s = sched(4, 2, 16);
        let any = |client: &str| {
            Owner::new(Some("ana"), client, 8)
                .with_person_share(1)
                .with_shared_cap(Some(1))
        };
        let r1 = s
            .acquire(&any("runtime"), Priority::Interactive)
            .await
            .unwrap();
        // Each waiter keeps its permit (in its task's output) until it is awaited.
        let mut waiting = Vec::new();
        for client in ["bot-docs", "runtime", "bot-docs"] {
            let (s2, o) = (s.clone(), any(client));
            waiting.push(tokio::spawn(async move {
                s2.acquire(&o, Priority::Interactive).await
            }));
            settle(&s, (1, waiting.len())).await;
        }
        // The saturated shared cap holds no one else up: Bo, through the same clients.
        let bo = s
            .acquire(
                &Owner::new(Some("bo"), "bot-docs", 8)
                    .with_person_share(1)
                    .with_shared_cap(Some(1)),
                Priority::Personal,
            )
            .await
            .unwrap();
        // Her own bot's call starts at once, at a lower priority than those waiting.
        let own = Owner::new(Some("ana"), "bot-pm-ana", 8).with_person_share(2);
        let o1 = tokio::time::timeout(
            Duration::from_millis(200),
            s.acquire(&own, Priority::Personal),
        )
        .await
        .expect("her own bot's call starts without waiting")
        .unwrap();
        assert_eq!(s.snapshot(), (3, 3));
        // She is at her two: her own bot's second call waits like everyone else's.
        let (s2, o) = (s.clone(), own.clone());
        let o2 = tokio::spawn(async move { s2.acquire(&o, Priority::Personal).await });
        settle(&s, (3, 4)).await;
        // Freeing the runtime's slot lets one shared call in (the oldest of the highest
        // priority, bot-docs'), never two: the shared cap is still 1.
        drop(r1);
        assert_eq!(s.snapshot(), (3, 3));
        // Freeing Bo's and her own bot's slots: her own bot's second call starts, and the
        // shared calls still wait for the one shared slot.
        drop((bo, o1));
        assert_eq!(s.snapshot(), (2, 2));
        let o2 = o2.await.unwrap().unwrap();
        for w in waiting {
            // Each in turn, as the one before gives the shared slot back.
            drop(w.await.unwrap().unwrap());
        }
        assert_eq!(s.snapshot(), (1, 0));
        drop(o2);
        assert_eq!(s.snapshot(), (0, 0));
    }

    #[tokio::test]
    async fn a_caller_that_goes_away_frees_its_place() {
        let s = sched(1, 1, 8);
        let held = s.acquire(&who("ana"), Priority::Interactive).await.unwrap();
        // A waiter whose caller disconnects leaves the queue.
        let s2 = s.clone();
        let gone = tokio::spawn(async move { s2.acquire(&who("bo"), Priority::Interactive).await });
        tokio::time::sleep(Duration::from_millis(5)).await;
        gone.abort();
        let _ = gone.await;
        assert_eq!(s.snapshot(), (1, 0));
        // So does one whose wait times out.
        let timed_out = tokio::time::timeout(
            Duration::from_millis(20),
            s.acquire(&who("cy"), Priority::Interactive),
        )
        .await;
        assert!(timed_out.is_err());
        assert_eq!(s.snapshot(), (1, 0));
        drop(held);
        assert_eq!(s.snapshot(), (0, 0));
        // The slot is still usable.
        let again = tokio::time::timeout(
            Duration::from_secs(1),
            s.acquire(&who("cy"), Priority::Personal),
        )
        .await
        .unwrap()
        .unwrap();
        drop(again);
        assert_eq!(s.snapshot(), (0, 0));
    }
}
