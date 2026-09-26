//! Bounded concurrency for independent model calls inside one job.
//!
//! A job often makes several calls that do not depend on each other (codes
//! per study, certainty per theme, reconciling appraisal domains). Running
//! them one after another made synthesis and appraisal slow. `bounded` runs
//! up to `limit` of them at once on the current task and returns the outputs
//! in input order. The per-provider semaphore still caps what reaches a
//! provider, so this only removes idle waiting.

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

/// Default number of calls one job keeps in flight.
pub const LIMIT: usize = 6;

/// Runs `f` over `items` with at most `limit` futures in flight and returns
/// the outputs in the order of `items`.
pub async fn bounded<T, F, Fut>(items: Vec<T>, limit: usize, f: F) -> Vec<Fut::Output>
where
    F: Fn(T) -> Fut,
    Fut: Future,
{
    let limit = limit.max(1);
    let total = items.len();
    let mut pending = items.into_iter().enumerate();
    let mut running: Vec<(usize, Pin<Box<Fut>>)> = Vec::new();
    let mut out: Vec<Option<Fut::Output>> = (0..total).map(|_| None).collect();
    loop {
        while running.len() < limit {
            match pending.next() {
                Some((i, item)) => running.push((i, Box::pin(f(item)))),
                None => break,
            }
        }
        if running.is_empty() {
            break;
        }
        let (i, value) = std::future::poll_fn(|cx| {
            for k in 0..running.len() {
                if let Poll::Ready(v) = running[k].1.as_mut().poll(cx) {
                    let (i, _) = running.swap_remove(k);
                    return Poll::Ready((i, v));
                }
            }
            Poll::Pending
        })
        .await;
        out[i] = Some(value);
    }
    out.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn keeps_input_order_and_the_limit() {
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let items: Vec<u64> = (0..10).collect();
        let out = bounded(items, 3, |n| {
            let live = live.clone();
            let peak = peak.clone();
            async move {
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // Later items finish first, so order must be restored.
                tokio::time::sleep(Duration::from_millis(20 - n)).await;
                live.fetch_sub(1, Ordering::SeqCst);
                n * 2
            }
        })
        .await;
        assert_eq!(out, (0..10).map(|n| n * 2).collect::<Vec<_>>());
        assert_eq!(peak.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn runs_in_parallel_not_in_sequence() {
        let start = std::time::Instant::now();
        let out = bounded((0..6).collect::<Vec<u32>>(), 6, |n| async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            n
        })
        .await;
        assert_eq!(out.len(), 6);
        assert!(start.elapsed() < Duration::from_millis(400));
    }

    #[tokio::test]
    async fn empty_input_returns_nothing() {
        let out: Vec<u8> = bounded(Vec::<u8>::new(), 4, |n| async move { n }).await;
        assert!(out.is_empty());
    }
}
