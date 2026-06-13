//! Generic restart supervisor: run an async unit forever, restarting on any
//! completion with capped exponential backoff (reset after a stable run), until
//! a watch-based shutdown signal. See research/supervisor-design.md.
use std::future::Future;
use tokio::sync::watch;
use tokio::time::{Duration, Instant};

/// Backoff/restart policy for a supervised unit.
#[derive(Debug, Clone, Copy)]
pub struct RestartPolicy {
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub stable_after: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            stable_after: Duration::from_secs(10),
        }
    }
}

/// Outcome of a supervise loop, available after shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuperviseReport {
    /// Times the unit was restarted after its first run.
    pub restarts: u32,
}

/// Next backoff: double, capped at `max`. Saturating so a pathological policy
/// (e.g. `max_backoff = Duration::MAX`) can never panic on overflow.
pub fn next_backoff(current: Duration, max: Duration) -> Duration {
    current.saturating_mul(2).min(max)
}

/// Resolve once `rx` is (or becomes) `true`, or the sender drops. Shared with
/// the daemon, which awaits the same shutdown signal.
pub(crate) async fn wait_for_shutdown(rx: &mut watch::Receiver<bool>) {
    loop {
        // borrow_and_update marks the current value seen, so a later `changed()`
        // waits for the NEXT change (idiomatic; avoids a spurious extra iteration
        // if the channel is ever re-signalled).
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            return; // sender dropped -> treat as shutdown
        }
    }
}

/// Run `factory()` forever, restarting on ANY completion (Ok or Err — both mean
/// the unit stopped unexpectedly; intentional stop comes via `shutdown`). Uses
/// capped exponential backoff, reset whenever a run lasted >= `stable_after`.
pub async fn supervise<F, Fut>(
    _name: &str,
    factory: F,
    policy: RestartPolicy,
    mut shutdown: watch::Receiver<bool>,
) -> SuperviseReport
where
    // Send + 'static so the daemon can `tokio::spawn(supervise(...))`; surfacing
    // the bound here gives a clear error rather than an inference failure at the
    // eventual spawn call site.
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = std::io::Result<()>> + Send,
{
    let mut restarts: u32 = 0;
    let mut backoff = policy.initial_backoff;

    loop {
        if *shutdown.borrow() {
            break;
        }

        let started = Instant::now();
        // Run the unit; cancel (drop) it if shutdown fires first.
        let stop = tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => true,
            _ = factory() => false,
        };
        if stop {
            break;
        }

        // Unit ended unexpectedly. Reset backoff if it had been stable.
        if started.elapsed() >= policy.stable_after {
            backoff = policy.initial_backoff;
        }

        // Wait the backoff, but cut it short on shutdown.
        let stop = tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => true,
            _ = tokio::time::sleep(backoff) => false,
        };
        if stop {
            break;
        }

        restarts += 1;
        backoff = next_backoff(backoff, policy.max_backoff);
    }

    SuperviseReport { restarts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::watch;
    use tokio::time::{Duration, Instant};

    fn fast_policy() -> RestartPolicy {
        RestartPolicy {
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            stable_after: Duration::from_secs(1),
        }
    }

    #[test]
    fn next_backoff_doubles_and_caps() {
        let max = Duration::from_secs(5);
        assert_eq!(
            next_backoff(Duration::from_millis(100), max),
            Duration::from_millis(200)
        );
        assert_eq!(
            next_backoff(Duration::from_secs(3), max),
            Duration::from_secs(5)
        ); // 6 capped to 5
        assert_eq!(
            next_backoff(Duration::from_secs(5), max),
            Duration::from_secs(5)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_before_first_run_does_not_invoke_factory() {
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let report = supervise(
            "unit",
            move || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            fast_policy(),
            rx,
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.restarts, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn restarts_until_shutdown() {
        let (tx, rx) = watch::channel(false);
        let tx = Arc::new(tx);
        let calls = Arc::new(AtomicUsize::new(0));
        let (c, t) = (calls.clone(), tx.clone());
        let report = supervise(
            "unit",
            move || {
                let (c, t) = (c.clone(), t.clone());
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                    if n == 3 {
                        let _ = t.send(true); // ask to stop after the 3rd run
                    }
                    Ok(())
                }
            },
            fast_policy(),
            rx,
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 3); // factory ran exactly 3 times
        assert_eq!(report.restarts, 2); // restarted after runs 1 and 2; stopped instead of restarting after 3
    }

    #[tokio::test(start_paused = true)]
    async fn backoff_grows_then_resets_after_stable_run() {
        // Record (start, end) instants per run; assert the GAP (end_n -> start_{n+1})
        // follows the backoff schedule and resets after a >= stable_after run.
        let (tx, rx) = watch::channel(false);
        let tx = Arc::new(tx);
        let times: Arc<Mutex<Vec<(Instant, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let (tm, c, t) = (times.clone(), calls.clone(), tx.clone());
        supervise(
            "unit",
            move || {
                let (tm, c, t) = (tm.clone(), c.clone(), t.clone());
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                    let start = Instant::now();
                    // run 3 stays up past stable_after (1s); others exit immediately
                    if n == 3 {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    let end = Instant::now();
                    tm.lock().unwrap().push((start, end));
                    if n == 4 {
                        let _ = t.send(true);
                    }
                    Ok(())
                }
            },
            fast_policy(),
            rx,
        )
        .await;

        let times = times.lock().unwrap();
        assert_eq!(times.len(), 4);
        let gap = |i: usize| times[i + 1].0.duration_since(times[i].1);
        assert_eq!(gap(0), Duration::from_millis(100)); // initial backoff
        assert_eq!(gap(1), Duration::from_millis(200)); // doubled
        assert_eq!(gap(2), Duration::from_millis(100)); // reset: run 3 was stable (2s >= 1s)
    }

    #[tokio::test(start_paused = true)]
    async fn external_shutdown_during_run_stops_cleanly() {
        // A unit that is mid-run when an EXTERNAL sender signals shutdown stops
        // without a further restart. Also exercises the Send + 'static bounds via
        // tokio::spawn.
        let (tx, rx) = watch::channel(false);
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let handle = tokio::spawn(supervise(
            "unit",
            move || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    Ok(())
                }
            },
            fast_policy(),
            rx,
        ));
        tokio::task::yield_now().await; // let the unit start and reach its sleep
        tx.send(true).unwrap();
        let report = handle.await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1); // started once
        assert_eq!(report.restarts, 0); // cancelled mid-run, never restarted
    }
}
