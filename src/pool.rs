//! A small fixed-size worker pool over a slice of jobs.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// Runs `work` on every item with up to `jobs` threads and returns the
/// results in input order. Stops handing out new items after an error.
pub fn map<T: Sync, R: Send, E: Send>(
    items: &[T],
    jobs: usize,
    work: impl Fn(&T) -> Result<R, E> + Sync,
) -> Result<Vec<R>, E> {
    let next = AtomicUsize::new(0);
    let failed = std::sync::atomic::AtomicBool::new(false);
    let results: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    let error: Mutex<Option<E>> = Mutex::new(None);
    let workers = jobs.max(1).min(items.len().max(1));
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    if failed.load(Ordering::Relaxed) {
                        return;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= items.len() {
                        return;
                    }
                    match work(&items[i]) {
                        Ok(r) => results.lock().unwrap()[i] = Some(r),
                        Err(e) => {
                            failed.store(true, Ordering::Relaxed);
                            let mut slot = error.lock().unwrap();
                            if slot.is_none() {
                                *slot = Some(e);
                            }
                            return;
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = error.into_inner().unwrap() {
        return Err(e);
    }
    Ok(results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|r| r.expect("every item processed"))
        .collect())
}

pub fn default_jobs() -> usize {
    thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
}
