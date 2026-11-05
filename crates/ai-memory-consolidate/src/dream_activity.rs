//! Activity snapshots and cancellation shared by DREAM and live work.

use ai_memory_llm::{Embedder, LlmResult};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A single, consistent view of client activity and ongoing work.
#[derive(Debug, Clone, Copy)]
pub struct ActivitySnapshot {
    /// Last activity in microseconds since epoch.
    pub last_activity_us: i64,
    /// Requests or relevant background jobs that have not finished yet.
    pub in_flight: usize,
    generation: u64,
}

/// Shared activity state. A generation detects activity even under clock skew.
#[derive(Debug, Clone)]
pub struct ActivityClock(Arc<Mutex<ActivitySnapshot>>);

impl ActivityClock {
    /// Seed an activity clock so a freshly started server is not idle.
    #[must_use]
    pub fn new(now_us: i64) -> Self {
        Self(Arc::new(Mutex::new(ActivitySnapshot {
            last_activity_us: now_us,
            in_flight: 0,
            generation: 0,
        })))
    }

    /// Record activity without rewinding the wall clock.
    pub fn mark(&self, now_us: i64) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.last_activity_us = state.last_activity_us.max(now_us);
        state.generation = state.generation.wrapping_add(1);
    }

    /// Hold an activity reservation until the request or job finishes.
    #[must_use]
    pub fn begin(&self, now_us: i64) -> ActivityGuard {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.last_activity_us = state.last_activity_us.max(now_us);
        state.generation = state.generation.wrapping_add(1);
        state.in_flight = state.in_flight.saturating_add(1);
        ActivityGuard(self.clone())
    }

    /// Take one consistent snapshot for both idle and cancel decisions.
    #[must_use]
    pub fn snapshot(&self) -> ActivitySnapshot {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The most recent activity instant seen.
    #[must_use]
    pub fn last_activity_us(&self) -> i64 {
        self.snapshot().last_activity_us
    }

    fn changed_since(&self, snapshot: ActivitySnapshot) -> bool {
        let now = self.snapshot();
        now.generation != snapshot.generation || now.in_flight > 0 || snapshot.in_flight > 0
    }
}

impl Default for ActivityClock {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Reservation for an admitted request or relevant job.
pub struct ActivityGuard(ActivityClock);

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap_or_else(|e| e.into_inner());
        state.in_flight = state.in_flight.saturating_sub(1);
        state.last_activity_us = state
            .last_activity_us
            .max(jiff::Timestamp::now().as_microsecond());
        state.generation = state.generation.wrapping_add(1);
    }
}

/// Explicit cancellation optionally bound to a live activity snapshot.
#[derive(Clone, Default)]
pub struct DreamCancel {
    cancelled: Arc<AtomicBool>,
    activity: Option<(ActivityClock, ActivitySnapshot)>,
}

impl DreamCancel {
    /// A fresh token for a standalone pass.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a token to the same snapshot that admitted the run.
    #[must_use]
    pub fn for_activity(clock: ActivityClock, snapshot: ActivitySnapshot) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            activity: Some((clock, snapshot)),
        }
    }

    /// Request cancellation before another request or write is admitted.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Check activity synchronously, without a delayed watcher task.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        if self.cancelled.load(Ordering::Relaxed) {
            return true;
        }
        if self
            .activity
            .as_ref()
            .is_some_and(|(clock, snapshot)| clock.changed_since(*snapshot))
        {
            self.cancel();
            return true;
        }
        false
    }
}

/// Observe actual embedding inference, including filesystem watcher work.
/// No-op reconciliation reads do not continuously reset the idle window.
pub struct ActivityEmbedder {
    inner: Arc<dyn Embedder>,
    clock: ActivityClock,
}

impl ActivityEmbedder {
    /// Wrap the running embedder without changing its embedding coordinate.
    #[must_use]
    pub fn new(inner: Arc<dyn Embedder>, clock: ActivityClock) -> Self {
        Self { inner, clock }
    }
}

impl Embedder for ActivityEmbedder {
    fn provider(&self) -> &'static str {
        self.inner.provider()
    }
    fn model(&self) -> &str {
        self.inner.model()
    }
    fn model_identity(&self) -> String {
        self.inner.model_identity()
    }
    fn dim(&self) -> u32 {
        self.inner.dim()
    }

    fn embed<'life0, 'life1, 'async_trait>(
        &'life0 self,
        text: &'life1 str,
    ) -> Pin<Box<dyn Future<Output = LlmResult<Vec<f32>>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let _active = self.clock.begin(jiff::Timestamp::now().as_microsecond());
            self.inner.embed(text).await
        })
    }

    fn embed_document<'life0, 'life1, 'async_trait>(
        &'life0 self,
        text: &'life1 str,
    ) -> Pin<Box<dyn Future<Output = LlmResult<Vec<f32>>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let _active = self.clock.begin(jiff::Timestamp::now().as_microsecond());
            self.inner.embed_document(text).await
        })
    }

    fn embed_query<'life0, 'life1, 'async_trait>(
        &'life0 self,
        text: &'life1 str,
    ) -> Pin<Box<dyn Future<Output = LlmResult<Vec<f32>>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let _active = self.clock.begin(jiff::Timestamp::now().as_microsecond());
            self.inner.embed_query(text).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_cancels_even_when_timestamp_does_not_advance() {
        let clock = ActivityClock::new(100);
        let cancel = DreamCancel::for_activity(clock.clone(), clock.snapshot());
        assert!(!cancel.is_cancelled());
        clock.mark(50);
        assert!(cancel.is_cancelled());
        assert_eq!(clock.last_activity_us(), 100);
    }

    #[test]
    fn in_flight_work_refuses_a_run_and_drop_starts_idle_window() {
        let clock = ActivityClock::new(0);
        let request = clock.begin(100);
        let snapshot = clock.snapshot();
        assert_eq!(snapshot.in_flight, 1);
        assert!(DreamCancel::for_activity(clock.clone(), snapshot).is_cancelled());
        drop(request);
        let after = clock.snapshot();
        assert_eq!(after.in_flight, 0);
        assert!(after.last_activity_us >= snapshot.last_activity_us);
        assert!(!DreamCancel::for_activity(clock, after).is_cancelled());
    }

    #[tokio::test]
    async fn actual_embedding_inference_is_live_work_and_preserves_coordinates() {
        let clock = ActivityClock::default();
        let cancel = DreamCancel::for_activity(clock.clone(), clock.snapshot());
        let embedder = ActivityEmbedder::new(
            Arc::new(ai_memory_llm::SyntheticEmbedder::new(64)),
            clock.clone(),
        );
        assert_eq!(embedder.provider(), "synthetic");
        assert_eq!(embedder.model_identity(), "bag-of-words-v1");
        assert_eq!(embedder.dim(), 64);
        embedder
            .embed_document("synthetic watcher body")
            .await
            .unwrap();
        assert!(
            cancel.is_cancelled(),
            "embedding work must cancel the old DREAM snapshot"
        );
        assert_eq!(clock.snapshot().in_flight, 0);
    }

    struct PendingEmbedder {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl Embedder for PendingEmbedder {
        fn provider(&self) -> &'static str {
            "pending"
        }
        fn model(&self) -> &str {
            "fixture-model"
        }
        fn dim(&self) -> u32 {
            1
        }
        fn embed<'life0, 'life1, 'async_trait>(
            &'life0 self,
            _text: &'life1 str,
        ) -> Pin<Box<dyn Future<Output = LlmResult<Vec<f32>>> + Send + 'async_trait>>
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(vec![1.0])
            })
        }
    }

    #[tokio::test]
    async fn embedding_reservation_lasts_until_inference_completes() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let clock = ActivityClock::default();
        let cancel = DreamCancel::for_activity(clock.clone(), clock.snapshot());
        let embedder = ActivityEmbedder::new(
            Arc::new(PendingEmbedder {
                entered: entered.clone(),
                release: release.clone(),
            }),
            clock.clone(),
        );
        let task =
            tokio::spawn(async move { embedder.embed_document("synthetic watcher input").await });
        entered.notified().await;
        assert_eq!(clock.snapshot().in_flight, 1);
        assert!(cancel.is_cancelled());
        release.notify_one();
        assert_eq!(task.await.unwrap().unwrap(), vec![1.0]);
        assert_eq!(clock.snapshot().in_flight, 0);
    }
}
