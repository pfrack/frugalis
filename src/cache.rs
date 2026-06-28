use std::sync::atomic::{AtomicU64, Ordering};

/// A cached upstream response entry, stored keyed by request body SHA-256.
#[derive(Clone, Debug)]
pub struct CachedEntry {
    pub body: String,
    #[allow(dead_code)]
    pub content_type: String,
    pub status: u16,
}

/// Snapshot of cache statistics for the dashboard.
#[derive(Debug)]
pub struct CacheStats {
    pub hit_count: u64,
    pub miss_count: u64,
    pub entry_count: u64,
    pub max_entries: u64,
    pub ttl_secs: u64,
}

/// Thread-safe response cache backed by `moka::sync::Cache`.
///
/// Tracks hit/miss counts via lock-free atomics. TTL and capacity eviction
/// are handled by moka's internal housekeeping thread.
pub struct ResponseCache {
    cache: moka::sync::Cache<String, CachedEntry>,
    hits: AtomicU64,
    misses: AtomicU64,
    max_entries: u64,
    ttl_secs: u64,
}

impl ResponseCache {
    pub fn new(ttl_secs: u64, max_entries: u64) -> Self {
        let cache = moka::sync::Cache::builder()
            .time_to_live(std::time::Duration::from_secs(ttl_secs))
            .max_capacity(max_entries)
            .build();
        Self {
            cache,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            max_entries,
            ttl_secs,
        }
    }

    /// Look up a cached entry by its SHA-256 hex key.
    /// Increments `hits` atomically on success or `misses` on lookup miss.
    pub fn get(&self, key: &str) -> Option<CachedEntry> {
        if let Some(entry) = self.cache.get(key) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            Some(entry)
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// Store an entry in the cache. moka handles TTL and capacity eviction automatically.
    pub fn put(&self, key: String, entry: CachedEntry) {
        self.cache.insert(key, entry);
    }

    /// Return a snapshot of current cache statistics.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hit_count: self.hits.load(Ordering::Relaxed),
            miss_count: self.misses.load(Ordering::Relaxed),
            entry_count: self.cache.entry_count(),
            max_entries: self.max_entries,
            ttl_secs: self.ttl_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_get_put() {
        let cache = ResponseCache::new(60, 10);
        let entry = CachedEntry {
            body: "test body".to_string(),
            content_type: "application/json".to_string(),
            status: 200,
        };
        cache.put("key1".to_string(), entry.clone());
        let retrieved = cache.get("key1");
        assert!(retrieved.is_some());
        let r = retrieved.unwrap();
        assert_eq!(r.body, "test body");
        assert_eq!(r.content_type, "application/json");
        assert_eq!(r.status, 200);
    }

    #[test]
    fn test_cache_hit_miss_counters() {
        let cache = ResponseCache::new(60, 10);
        let entry = CachedEntry {
            body: "body".to_string(),
            content_type: "application/json".to_string(),
            status: 200,
        };
        cache.put("hit".to_string(), entry);

        let _ = cache.get("hit");
        let _ = cache.get("miss");

        let stats = cache.stats();
        assert_eq!(stats.hit_count, 1);
        assert_eq!(stats.miss_count, 1);
    }

    #[test]
    fn test_cache_miss_returns_none() {
        let cache = ResponseCache::new(60, 10);
        assert!(cache.get("nonexistent").is_none());
        let stats = cache.stats();
        assert_eq!(stats.miss_count, 1);
    }

    #[test]
    fn test_cache_stats() {
        let cache = ResponseCache::new(120, 50);
        let entry = CachedEntry {
            body: "b".to_string(),
            content_type: "application/json".to_string(),
            status: 200,
        };
        cache.put("a".to_string(), entry);
        let _ = cache.get("a");
        let _ = cache.get("a");
        let _ = cache.get("b");

        let stats = cache.stats();
        assert_eq!(stats.hit_count, 2);
        assert_eq!(stats.miss_count, 1);
        // moka's entry_count() is approximate; at most one entry was inserted
        assert!(stats.entry_count <= 1, "entry_count={} should be <= 1", stats.entry_count);
        assert_eq!(stats.max_entries, 50);
        assert_eq!(stats.ttl_secs, 120);
    }

    #[test]
    fn test_cache_max_capacity() {
        let cache = ResponseCache::new(60, 2);
        for i in 0..4 {
            cache.put(
                format!("key{i}"),
                CachedEntry {
                    body: format!("body{i}"),
                    content_type: "application/json".to_string(),
                    status: 200,
                },
            );
        }
        // moka evicts oldest entries; at most max_capacity remain
        assert!(
            cache.stats().entry_count <= 2,
            "cache should evict entries beyond max_capacity"
        );
    }
}
