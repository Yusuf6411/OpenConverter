//! Progress and cooperative cancellation (MASTER_PROMPT §9.7).
use crate::error::{Error, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cloneable cancellation flag shared between a job and its controller.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Passed to every long loop in every codec.
pub trait Progress: Send + Sync {
    /// Report `done` of `total` units for the named stage. Must be cheap.
    fn report(&self, done: u64, total: u64, stage: &str);
    /// Returns `Err(Cancelled)` once cancellation was requested.
    fn check_cancelled(&self) -> Result<()>;
}

/// A progress sink that ignores reports and never cancels.
pub struct NoProgress;

impl Progress for NoProgress {
    fn report(&self, _done: u64, _total: u64, _stage: &str) {}
    fn check_cancelled(&self) -> Result<()> {
        Ok(())
    }
}

/// Progress that honours a [`CancelToken`] and forwards reports to a callback.
pub struct TokenProgress<F: Fn(u64, u64, &str) + Send + Sync> {
    token: CancelToken,
    callback: F,
}

impl<F: Fn(u64, u64, &str) + Send + Sync> TokenProgress<F> {
    pub fn new(token: CancelToken, callback: F) -> Self {
        Self { token, callback }
    }
}

impl<F: Fn(u64, u64, &str) + Send + Sync> Progress for TokenProgress<F> {
    fn report(&self, done: u64, total: u64, stage: &str) {
        (self.callback)(done, total, stage);
    }
    fn check_cancelled(&self) -> Result<()> {
        if self.token.is_cancelled() {
            Err(Error::cancelled())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn cancel_is_observed() {
        let t = CancelToken::new();
        let p = TokenProgress::new(t.clone(), |_, _, _| {});
        assert!(p.check_cancelled().is_ok());
        t.cancel();
        assert_eq!(
            p.check_cancelled().err().map(|e| e.code),
            Some(ErrorCode::Cancelled)
        );
    }

    #[test]
    fn reports_reach_callback() {
        let seen = Arc::new(AtomicU64::new(0));
        let s2 = seen.clone();
        let p = TokenProgress::new(CancelToken::new(), move |d, _, _| {
            s2.store(d, Ordering::Relaxed)
        });
        p.report(7, 10, "decode");
        assert_eq!(seen.load(Ordering::Relaxed), 7);
    }
}
