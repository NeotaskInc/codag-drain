use std::collections::HashMap;
use std::num::NonZeroUsize;

use lru::LruCache;

use crate::cluster::LogCluster;

/// Backing store for log clusters.
///
/// - `Unlimited` — plain `HashMap`, no eviction.
/// - `Limited`   — `LruCache` that evicts the least-recently-used cluster
///                 once `max_clusters` is reached.
pub enum ClusterStorage {
    Unlimited(HashMap<usize, LogCluster>),
    Limited(LruCache<usize, LogCluster>),
}

impl ClusterStorage {
    pub fn new(max_clusters: Option<usize>) -> Self {
        match max_clusters {
            None => ClusterStorage::Unlimited(HashMap::new()),
            Some(cap) => {
                ClusterStorage::Limited(LruCache::new(NonZeroUsize::new(cap).unwrap()))
            }
        }
    }

    /// Insert or update a cluster (touches LRU).
    pub fn insert(&mut self, id: usize, cluster: LogCluster) {
        match self {
            ClusterStorage::Unlimited(map) => {
                map.insert(id, cluster);
            }
            ClusterStorage::Limited(cache) => {
                cache.put(id, cluster);
            }
        }
    }

    /// Peek at a cluster **without** promoting it in the LRU.
    pub fn peek(&self, id: &usize) -> Option<&LogCluster> {
        match self {
            ClusterStorage::Unlimited(map) => map.get(id),
            ClusterStorage::Limited(cache) => cache.peek(id),
        }
    }

    /// Get a mutable reference, **promoting** the entry in the LRU.
    pub fn get_mut(&mut self, id: &usize) -> Option<&mut LogCluster> {
        match self {
            ClusterStorage::Unlimited(map) => map.get_mut(id),
            ClusterStorage::Limited(cache) => cache.get_mut(id),
        }
    }

    /// Mark a cluster as recently used (LRU promotion only).
    pub fn touch(&mut self, id: &usize) {
        match self {
            ClusterStorage::Unlimited(_) => {}
            ClusterStorage::Limited(cache) => {
                let _ = cache.get(id);
            }
        }
    }

    pub fn contains(&self, id: &usize) -> bool {
        match self {
            ClusterStorage::Unlimited(map) => map.contains_key(id),
            ClusterStorage::Limited(cache) => cache.peek(id).is_some(),
        }
    }

    pub fn values(&self) -> Vec<&LogCluster> {
        match self {
            ClusterStorage::Unlimited(map) => map.values().collect(),
            ClusterStorage::Limited(cache) => cache.iter().map(|(_, v)| v).collect(),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            ClusterStorage::Unlimited(map) => map.len(),
            ClusterStorage::Limited(cache) => cache.len(),
        }
    }
}
