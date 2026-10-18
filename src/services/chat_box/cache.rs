//! A cache of data that is fetched in the background: the 7TV cosmetics of
//! the chatters, and the other channels of a shared chat.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use dashmap::{DashMap, DashSet};

/// How long a failed fetch is kept, before the next message fetches again
const FAILURE_TTL: Duration = Duration::from_secs(60);
/// A fetch that takes longer fails
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// What the cache has for a key
pub struct Lookup<T> {
    /// The value, also when it is not fresh
    pub value: Option<Arc<T>>,
    /// The caller must start a fetch: the value is not fresh, and no other
    /// fetch runs. The key is marked as being fetched.
    pub fetch: bool,
}

pub struct FetchCache<T> {
    /// For the logs, e.g. "the 7TV cosmetics"
    what: &'static str,
    ttl: Duration,
    /// Key → (fresh until, value); `None` when there is no data
    entries: DashMap<String, (Instant, Option<Arc<T>>)>,
    /// The keys that are being fetched
    fetching: DashSet<String>,
}

impl<T> FetchCache<T> {
    pub fn new(what: &'static str, ttl: Duration) -> Self {
        Self {
            what,
            ttl,
            entries: Default::default(),
            fetching: Default::default(),
        }
    }

    /// The value, also when it is not fresh.
    pub fn value(&self, key: &str) -> Option<Arc<T>> {
        self.entries.get(key).and_then(|entry| entry.1.clone())
    }

    /// The value; when it is not fresh, [`Lookup::fetch`] tells the caller
    /// to fetch it.
    pub fn lookup(&self, key: &str) -> Lookup<T> {
        let (fresh, value) = match self.entries.get(key) {
            Some(entry) => (Instant::now() < entry.0, entry.1.clone()),
            None => (false, None),
        };
        Lookup {
            value,
            fetch: !fresh && self.fetching.insert(key.to_owned()),
        }
    }

    /// Runs a fetch that [`lookup`](Self::lookup) asked for, and stores
    /// the result. A failure keeps the old value, for [`FAILURE_TTL`].
    pub async fn fetch(&self, key: &str, fetch: impl Future<Output = Result<Option<T>>>) {
        let result = tokio::time::timeout(FETCH_TIMEOUT, fetch)
            .await
            .unwrap_or_else(|_| Err(anyhow!("took too long")));
        let entry = match result {
            Ok(value) => (Instant::now() + self.ttl, value.map(Arc::new)),
            Err(error) => {
                tracing::warn!(?error, key, "chat box: failed to get {}", self.what);
                (Instant::now() + FAILURE_TTL, self.value(key))
            }
        };
        self.entries.insert(key.to_owned(), entry);
        self.fetching.remove(key);
    }

    /// Makes the value not fresh: the next lookup fetches it again.
    pub fn invalidate(&self, key: &str) {
        if let Some(mut entry) = self.entries.get_mut(key) {
            entry.0 = Instant::now();
        }
    }

    #[cfg(test)]
    pub fn is_fetching(&self) -> bool {
        !self.fetching.is_empty()
    }

    #[cfg(test)]
    pub fn insert(&self, key: &str, value: Option<T>) {
        self.entries.insert(
            key.to_owned(),
            (Instant::now() + self.ttl, value.map(Arc::new)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fetches_once_and_keeps_the_old_value_on_failure() {
        let cache = FetchCache::new("a number", Duration::from_secs(60));
        let first = cache.lookup("k");
        assert!(first.value.is_none() && first.fetch);
        // a fetch runs already
        assert!(!cache.lookup("k").fetch);
        cache.fetch("k", async { Ok(Some(1)) }).await;
        let fresh = cache.lookup("k");
        assert_eq!(fresh.value.as_deref(), Some(&1));
        assert!(!fresh.fetch);

        cache.invalidate("k");
        assert!(cache.lookup("k").fetch);
        cache.fetch("k", async { Err(anyhow!("down")) }).await;
        // the old value, fresh for a shorter time
        let failed = cache.lookup("k");
        assert_eq!(failed.value.as_deref(), Some(&1));
        assert!(!failed.fetch);
    }
}
