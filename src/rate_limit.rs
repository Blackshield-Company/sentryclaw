use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Simple in-memory rate limiter for webhook endpoints.
/// Limits requests per IP address within a time window.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    max_requests: u32,
    window: Duration,
    store: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
    /// Shared guard for the background cleanup task. The task is aborted when
    /// the last clone of this `RateLimiter` is dropped, or on [`RateLimiter::shutdown`].
    cleanup_task: Option<Arc<CleanupTask>>,
}

/// Owns the cleanup task's `JoinHandle` and aborts it on drop.
#[derive(Debug)]
struct CleanupTask(tokio::task::JoinHandle<()>);

impl CleanupTask {
    fn abort(&self) {
        self.0.abort();
    }

    fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

impl Drop for CleanupTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl RateLimiter {
    pub fn new(max_requests: u32, window_seconds: u64) -> Self {
        let window = Duration::from_secs(window_seconds);
        let store: Arc<Mutex<HashMap<String, Vec<Instant>>>> = Arc::new(Mutex::new(HashMap::new()));
        // The task only holds a weak reference so it never keeps the store alive.
        let store_weak = Arc::downgrade(&store);
        let cleanup_window = window;

        // Only spawn cleanup task if we're inside a Tokio runtime
        let cleanup_handle: Option<tokio::task::JoinHandle<()>> =
            if tokio::runtime::Handle::try_current().is_ok() {
                Some(tokio::spawn(async move {
                    let mut interval =
                        tokio::time::interval(cleanup_window.max(Duration::from_secs(60)));
                    loop {
                        interval.tick().await;
                        let now = Instant::now();
                        let Some(store) = store_weak.upgrade() else {
                            break;
                        };
                        let mut store = store.lock().await;
                        store.retain(|_, timestamps| {
                            timestamps.retain(|t| now.duration_since(*t) < cleanup_window);
                            !timestamps.is_empty()
                        });
                    }
                }))
            } else {
                None
            };

        Self {
            max_requests,
            window,
            store,
            cleanup_task: cleanup_handle.map(|h| Arc::new(CleanupTask(h))),
        }
    }

    /// Check if a request from the given key is allowed.
    /// Returns true if the request is allowed, false if rate limited.
    pub async fn is_allowed(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut store = self.store.lock().await;

        let timestamps = store.entry(key.to_string()).or_default();

        // Remove timestamps outside the window
        timestamps.retain(|t| now.duration_since(*t) < self.window);

        if timestamps.len() >= self.max_requests as usize {
            false
        } else {
            timestamps.push(now);
            true
        }
    }

    /// Get current request count for a key.
    pub async fn count(&self, key: &str) -> usize {
        let now = Instant::now();
        let mut store = self.store.lock().await;

        if let Some(timestamps) = store.get_mut(key) {
            timestamps.retain(|t| now.duration_since(*t) < self.window);
            timestamps.len()
        } else {
            0
        }
    }

    /// Clean up expired entries to prevent unbounded growth.
    pub async fn cleanup(&self) {
        let now = Instant::now();
        let mut store = self.store.lock().await;
        store.retain(|_, timestamps| {
            timestamps.retain(|t| now.duration_since(*t) < self.window);
            !timestamps.is_empty()
        });
    }

    /// Stop the background cleanup task (affects all clones of this limiter).
    /// Rate limiting keeps working; expired entries are still pruned lazily
    /// per key and via [`RateLimiter::cleanup`].
    pub fn shutdown(&self) {
        if let Some(task) = &self.cleanup_task {
            task.abort();
        }
    }

    /// Whether the background cleanup task is running.
    pub fn cleanup_task_running(&self) -> bool {
        self.cleanup_task.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(60, 60) // 60 requests per minute
    }
}

/// Extract client IP from request info.
pub fn extract_client_key(addr: Option<SocketAddr>) -> String {
    addr.map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_rate_limiter_allows_within_limit() {
        let limiter = RateLimiter::new(3, 60);

        assert!(limiter.is_allowed("client1").await);
        assert!(limiter.is_allowed("client1").await);
        assert!(limiter.is_allowed("client1").await);
    }

    #[tokio::test]
    async fn test_rate_limiter_blocks_over_limit() {
        let limiter = RateLimiter::new(2, 60);

        assert!(limiter.is_allowed("client1").await);
        assert!(limiter.is_allowed("client1").await);
        assert!(!limiter.is_allowed("client1").await);
    }

    #[tokio::test]
    async fn test_rate_limiter_independent_keys() {
        let limiter = RateLimiter::new(1, 60);

        assert!(limiter.is_allowed("client1").await);
        assert!(limiter.is_allowed("client2").await);
        assert!(!limiter.is_allowed("client1").await);
    }

    #[tokio::test]
    async fn test_rate_limiter_window_expires() {
        let limiter = RateLimiter::new(1, 1);

        assert!(limiter.is_allowed("client1").await);
        assert!(!limiter.is_allowed("client1").await);

        // Wait for window to expire
        sleep(Duration::from_secs(2)).await;
        assert!(limiter.is_allowed("client1").await);
    }

    #[tokio::test]
    async fn test_cleanup() {
        let limiter = RateLimiter::new(10, 1);
        limiter.is_allowed("client1").await;

        sleep(Duration::from_secs(2)).await;
        limiter.cleanup().await;

        assert_eq!(limiter.count("client1").await, 0);
    }

    #[tokio::test]
    async fn test_shutdown_stops_cleanup_task() {
        let limiter = RateLimiter::new(10, 60);
        assert!(limiter.cleanup_task_running());
        limiter.shutdown();
        tokio::task::yield_now().await;
        assert!(!limiter.cleanup_task_running());
        // Limiter still functions after shutdown.
        assert!(limiter.is_allowed("client1").await);
    }

    #[tokio::test]
    async fn test_drop_aborts_cleanup_task_only_after_last_clone() {
        let limiter = RateLimiter::new(10, 60);
        let handle = limiter.cleanup_task.as_ref().unwrap().clone();
        let clone = limiter.clone();
        drop(limiter);
        tokio::task::yield_now().await;
        assert!(
            !handle.is_finished(),
            "task must survive while a clone exists"
        );
        drop(clone);
        // `handle` is the last strong ref; dropping it aborts the task.
        let weak = Arc::downgrade(&handle);
        drop(handle);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn test_no_task_outside_runtime() {
        let limiter = RateLimiter::new(1, 1);
        assert!(!limiter.cleanup_task_running());
        limiter.shutdown();
    }
}
