//! A small map that forgets its oldest entries, for what the bot remembers in memory (which
//! bot is in a thread, events already handled, drafts waiting for a tap). A restart forgets
//! it all; nothing here is the record of anything.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

/// A map holding at most `cap` entries: inserting a new key past that drops the oldest.
pub struct Bounded<K, V> {
    cap: usize,
    map: HashMap<K, V>,
    order: VecDeque<K>,
}

impl<K: Eq + Hash + Clone, V> Bounded<K, V> {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key)
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.map.get_mut(key)
    }

    /// Insert or replace. A replaced key keeps its place in the eviction order.
    pub fn insert(&mut self, key: K, value: V) {
        if self.map.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let value = self.map.remove(key)?;
        self.order.retain(|k| k != key);
        Some(value)
    }

    /// `true` the first time `key` is seen (within the last `cap` keys), `false` after.
    pub fn first_time(&mut self, key: K) -> bool
    where
        V: Default,
    {
        if self.map.contains_key(&key) {
            return false;
        }
        self.insert(key, V::default());
        true
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest() {
        let mut m = Bounded::new(2);
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("a", 10);
        assert_eq!(m.len(), 2);
        m.insert("c", 3);
        assert_eq!(m.get(&"a"), None, "the oldest goes first");
        assert_eq!(m.get(&"b"), Some(&2));
        assert_eq!(m.remove(&"b"), Some(2));
        assert_eq!(m.len(), 1);

        let mut seen: Bounded<String, ()> = Bounded::new(10);
        assert!(seen.first_time("Ev1".into()));
        assert!(!seen.first_time("Ev1".into()));
    }
}
