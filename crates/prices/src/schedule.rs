//! One background worker for a job that must never overlap itself, such as
//! the rate fetch: it runs at start, daily at a set time, and when asked.

use std::{future::Future, sync::Arc, time::Duration};

use chrono::{DateTime, NaiveTime, Utc};
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    /// The daily run, in UTC; `None` runs only at start and when asked.
    pub daily_at: Option<NaiveTime>,
    /// The wait after a failure, doubling with each further one, never past
    /// the next daily run.
    pub retry: Duration,
}

/// Asks the worker for a run.
#[derive(Debug, Clone)]
pub struct Handle {
    wake: Arc<Notify>,
}

impl Handle {
    /// A run already going finishes first; every request made meanwhile adds
    /// up to one more run, never a second one alongside it.
    pub fn request(&self) { self.wake.notify_one() }
}

/// Spawns the worker on the current tokio runtime; its first run starts at
/// once.
pub fn spawn<F, Fut>(schedule: Schedule, mut job: F) -> Handle
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let wake = Arc::new(Notify::new());
    wake.notify_one();
    let handle = Handle { wake: wake.clone() };
    tokio::spawn(async move {
        let mut failures = 0;
        loop {
            match wait(&schedule, failures, Utc::now()) {
                Some(wait) => tokio::select! {
                    () = wake.notified() => {}
                    () = tokio::time::sleep(wait) => {}
                },
                None => wake.notified().await,
            }
            // Run on a task of its own, so a panic fails the run, not the worker.
            failures = match tokio::spawn(job()).await {
                Ok(Ok(())) => 0,
                Ok(Err(e)) => {
                    log::warn!("background job failed: {e:#}");
                    failures + 1
                }
                Err(e) => {
                    log::warn!("background job panicked: {e}");
                    failures + 1
                }
            };
        }
    });
    handle
}

/// How long to sleep before the next unrequested run.
fn wait(schedule: &Schedule, failures: u32, now: DateTime<Utc>) -> Option<Duration> {
    let daily = schedule.daily_at.map(|at| until(at, now));
    match failures {
        0 => daily,
        n => {
            let retry = schedule.retry.saturating_mul(2u32.saturating_pow(n - 1));
            Some(daily.map_or(retry, |daily| retry.min(daily)))
        }
    }
}

/// The time from `now` to the next `at`, today's if still ahead.
fn until(at: NaiveTime, now: DateTime<Utc>) -> Duration {
    let today = now.date_naive().and_time(at).and_utc();
    let next = match today > now {
        true => today,
        false => today + chrono::Duration::days(1),
    };
    (next - now).to_std().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::{mpsc, Semaphore};

    use super::*;

    fn at(s: &str) -> DateTime<Utc> { s.parse().unwrap() }

    #[test]
    fn daily_runs_wait_for_the_set_time() {
        let schedule = Schedule {
            daily_at: Some(NaiveTime::from_hms_opt(22, 0, 0).unwrap()),
            retry: Duration::from_secs(300),
        };
        let hours = |h: u64| Some(Duration::from_secs(h * 3600));
        assert_eq!(wait(&schedule, 0, at("2024-01-01T20:00:00Z")), hours(2));
        assert_eq!(wait(&schedule, 0, at("2024-01-01T23:00:00Z")), hours(23));
        // Failures back off, doubling, but never past the daily run.
        let failed = |n| wait(&schedule, n, at("2024-01-01T20:00:00Z"));
        assert_eq!(failed(1), Some(schedule.retry));
        assert_eq!(failed(3), Some(schedule.retry * 4));
        assert_eq!(failed(10), hours(2));
    }

    /// A job that panics fails that run; the worker keeps serving requests.
    #[tokio::test(start_paused = true)]
    async fn a_panicking_run_does_not_stop_the_worker() {
        let (started, mut starts) = mpsc::unbounded_channel();
        async fn panics(started: mpsc::UnboundedSender<()>) -> anyhow::Result<()> {
            started.send(()).unwrap();
            panic!("a bug in the job")
        }
        let schedule = Schedule { daily_at: None, retry: Duration::from_secs(3600) };
        let handle = spawn(schedule, move || panics(started.clone()));
        starts.recv().await.expect("the first run starts, and panics");
        handle.request();
        starts.recv().await.expect("the worker still runs on request");
    }

    /// Requests made while a run is going coalesce into one more run, and the
    /// job never runs twice at once.
    #[tokio::test(start_paused = true)]
    async fn requests_during_a_run_queue_one_more_never_a_concurrent_one() {
        let (started, mut starts) = mpsc::unbounded_channel();
        let gate = Arc::new(Semaphore::new(0));
        let running = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let handle = spawn(Schedule { daily_at: None, retry: Duration::from_secs(60) }, {
            let (gate, running, most) = (gate.clone(), running.clone(), most.clone());
            move || {
                let (started, gate, running, most) =
                    (started.clone(), gate.clone(), running.clone(), most.clone());
                async move {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    started.send(()).unwrap();
                    gate.acquire().await.unwrap().forget();
                    running.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                }
            }
        });

        starts.recv().await.expect("the first run starts at once");
        for _ in 0..5 {
            handle.request();
        }
        gate.add_permits(1);
        starts.recv().await.expect("the requests queue one more run");
        gate.add_permits(1);
        let third = tokio::time::timeout(Duration::from_secs(3600), starts.recv()).await;
        assert!(third.is_err(), "five requests made one run, not five");
        assert_eq!(most.load(Ordering::SeqCst), 1);

        handle.request();
        starts.recv().await.expect("an idle worker runs on request");
        gate.add_permits(1);
    }
}
