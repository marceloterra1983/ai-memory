//! Short-lived query vectors only: never page results or document embeddings.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

const CAPACITY: usize = 128;
const MAX_DIM: usize = 8192;
const TTL: Duration = Duration::from_secs(60);

struct Entry {
    key: [u8; 32],
    inserted: Instant,
    vector: Vec<f32>,
}

/// Owned by one immutable embedder configuration; no text or disk storage.
#[derive(Default)]
pub(crate) struct QueryEmbeddingCache {
    entries: Mutex<VecDeque<Entry>>,
}

impl QueryEmbeddingCache {
    pub(crate) fn get(&self, input: &str) -> Option<Vec<f32>> {
        self.get_at(input, Instant::now())
    }

    fn get_at(&self, input: &str, now: Instant) -> Option<Vec<f32>> {
        let key: [u8; 32] = Sha256::digest(input.as_bytes()).into();
        let mut entries = self.entries.lock().ok()?;
        entries.retain(|entry| now.duration_since(entry.inserted) < TTL);
        entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.vector.clone())
    }

    pub(crate) fn insert(&self, input: &str, vector: &[f32], dim: u32) {
        self.insert_at(input, vector, dim, Instant::now());
    }

    fn insert_at(&self, input: &str, vector: &[f32], dim: u32, now: Instant) {
        // Bound retained vector bytes even for custom dimensions. A poisoned
        // lock or invalid vector simply keeps the ordinary inference path.
        if vector.is_empty() || vector.len() != dim as usize || vector.len() > MAX_DIM {
            return;
        }
        let norm_squared = vector.iter().map(|value| value * value).sum::<f32>();
        if !norm_squared.is_finite() || norm_squared <= 0.0 {
            return;
        }
        let key: [u8; 32] = Sha256::digest(input.as_bytes()).into();
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        entries.retain(|entry| now.duration_since(entry.inserted) < TTL && entry.key != key);
        if entries.len() >= CAPACITY {
            entries.pop_front();
        }
        entries.push_back(Entry {
            key,
            inserted: now,
            vector: vector.to_vec(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_cache_expiry_is_absolute_and_hits_do_not_extend_it() {
        let cache = QueryEmbeddingCache::default();
        let now = Instant::now();
        cache.insert_at("private synthetic query", &[1.0, 0.0], 2, now);
        assert_eq!(
            cache.get_at("private synthetic query", now + TTL / 2),
            Some(vec![1.0, 0.0])
        );
        assert!(cache.get_at("private synthetic query", now + TTL).is_none());
        assert!(cache.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn query_cache_evicts_oldest() {
        let cache = QueryEmbeddingCache::default();
        cache.insert("private synthetic query", &[1.0, 0.0], 2);
        for i in 0..CAPACITY {
            cache.insert(&format!("different {i}"), &[0.0, 1.0], 2);
        }
        assert!(cache.get("private synthetic query").is_none());
        assert_eq!(cache.get("different 127"), Some(vec![0.0, 1.0]));
        assert_eq!(cache.entries.lock().unwrap().len(), CAPACITY);
    }

    #[test]
    fn query_cache_invalid_or_oversized_vectors_are_not_retained() {
        let cache = QueryEmbeddingCache::default();
        for (vector, dim) in [
            (vec![], 0),
            (vec![1.0], 2),
            (vec![0.0, 0.0], 2),
            (vec![f32::MIN_POSITIVE, f32::MIN_POSITIVE], 2),
            (vec![f32::NAN, 1.0], 2),
            (vec![f32::INFINITY, 1.0], 2),
            (vec![1.0; MAX_DIM + 1], (MAX_DIM + 1) as u32),
        ] {
            cache.insert("invalid", &vector, dim);
            assert!(cache.get("invalid").is_none());
        }
        cache.insert("valid", &[1.0, 0.0], 2);
        assert_eq!(cache.get("valid"), Some(vec![1.0, 0.0]));
        cache.insert("max", &vec![1.0; MAX_DIM], MAX_DIM as u32);
        assert_eq!(cache.get("max").unwrap().len(), MAX_DIM);
    }
}
