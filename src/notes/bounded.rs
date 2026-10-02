//! A small capacity-bounded map that evicts the least recently used entry.
//!
//! No LRU crate is a dependency, and the caches that use this hold at most a
//! few hundred entries, so eviction is a linear scan for the oldest tick.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;

#[derive(Debug, Clone)]
pub(crate) struct BoundedMap<K, V> {
    entries: HashMap<K, (u64, V)>,
    capacity: usize,
    tick: u64,
}

// A shared cache utility (server stores and the client dock): not every
// method has a caller in every build.
#[allow(dead_code)]
impl<K: Eq + Hash + Clone, V> BoundedMap<K, V> {
    /// A map holding at most `capacity` entries (at least one).
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity: capacity.max(1),
            tick: 0,
        }
    }

    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.wrapping_add(1);
        self.tick
    }

    /// The value for `key`, marking it most recently used.
    pub(crate) fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.get_mut(key).map(|value| &*value)
    }

    /// The value for `key`, mutably, marking it most recently used.
    pub(crate) fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let tick = self.next_tick();
        self.entries.get_mut(key).map(|(used, value)| {
            *used = tick;
            value
        })
    }

    /// The value for `key` without touching its recency.
    pub(crate) fn peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.entries.get(key).map(|(_, value)| value)
    }

    pub(crate) fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.entries.contains_key(key)
    }

    /// Insert or replace `key`, evicting the least recently used entry when a
    /// new key would exceed the capacity. Returns the replaced value.
    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        let tick = self.next_tick();
        if let Some(slot) = self.entries.get_mut(&key) {
            slot.0 = tick;
            return Some(std::mem::replace(&mut slot.1, value));
        }
        if self.entries.len() >= self.capacity {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key, (tick, value));
        None
    }

    pub(crate) fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.entries.remove(key).map(|(_, value)| value)
    }

    /// Drop every entry the predicate rejects.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        self.entries.retain(|key, (_, value)| keep(key, value));
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }
}

impl<K: Eq + Hash + Clone, V> Default for BoundedMap<K, V> {
    /// A map of 64 entries.
    fn default() -> Self {
        Self::new(64)
    }
}
