//! Who runs next on the one GPU (MOD-4). At most `max_in_flight` requests run at once, each
//! person at most `per_user` of them, and each client (gateway token) at most its own cap
//! (review finding N3: one token can no longer hold every slot). A waiting request is started
//! in priority order: someone waiting in Slack (interactive) before a routine (scheduled)
//! before a personal agent's background work, and first come, first served within a priority.

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

/// The queue is full: the caller should try again shortly.
#[derive(Debug, PartialEq, Eq)]
pub struct Busy;

/// Whose a request is, for the caps: the person (when the gateway believes one; capped at
/// `per_user`), and the client that sent it (capped at `client_cap`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub person: Option<String>,
    pub client: String,
    pub client_cap: usize,
}

impl Owner {
    /// A request of `client`'s, for `person` when there is one.
    pub fn new(person: Option<&str>, client: &str, client_cap: usize) -> Self {
        Self {
            person: person.map(str::to_string),
            client: client.to_string(),
            client_cap,
        }
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

fn decrement(map: &mut HashMap<String, usize>, key: &str) {
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
            // Refused only if it would have to wait and the queue is already full; a request
            // that can start now always starts.
            if st.waiting.len() > self.limits.max_queue {
                if let Some(pos) = st.waiting.iter().position(|w| w.id == id) {
                    st.waiting.remove(pos);
                    return Err(Busy);
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
        }
        decrement(&mut st.per_client, &owner.client);
        self.dispatch(&mut st);
    }

    /// Start waiters while there is room: the highest priority whose person and client are
    /// both under their caps, oldest first.
    fn dispatch(&self, st: &mut State) {
        while st.in_flight < self.limits.max_in_flight {
            let pick = st
                .waiting
                .iter()
                .enumerate()
                .filter(|(_, w)| {
                    let person_ok = w.owner.person.as_ref().is_none_or(|p| {
                        st.per_user.get(p).copied().unwrap_or(0) < self.limits.per_user
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
                if let Some(p) = w.owner.person {
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
            Err(Busy)
        ));
        assert_eq!(s.snapshot(), (2, 1));
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
