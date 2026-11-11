//! Worker threads that share a period's independent track processing with the audio thread.
//!
//! The audio thread publishes a job, wakes the workers (a futex wake: no locks, no
//! allocation), takes items itself and spins until every item is done. Workers only help:
//! if one is late the audio thread simply does more of the work.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::OnceLock;
use std::thread::Thread;

/// Starts a thread running the given function; false if it couldn't.
pub type Spawn<'a> = &'a dyn Fn(Box<dyn FnOnce() + Send>) -> bool;

#[derive(Clone, Copy)]
struct Job {
    run: unsafe fn(*mut (), usize),
    data: *mut (),
}

struct Shared {
    generation: AtomicU64,
    next: AtomicUsize,
    done: AtomicUsize,
    total: AtomicUsize,
    job: UnsafeCell<Job>,
    stop: AtomicBool,
    /// Workers running at real-time priority and taking part.
    ready: Arc<AtomicUsize>,
    /// Each worker's handle, filled in as it starts, for waking it.
    threads: Box<[OnceLock<Thread>]>,
    /// Workers still running, so dropping the pool can wait for them.
    alive: AtomicUsize,
}

// `job` is written only by the audio thread, before `next` is released for a new job.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

pub struct Pool {
    shared: Arc<Shared>,
}

unsafe fn nothing(_: *mut (), _: usize) {}

impl Pool {
    /// Starts `count` workers through `spawn`. With `require_rt`, a worker that finds itself
    /// without real-time scheduling leaves at once, since one the scheduler can preempt
    /// would cause dropouts.
    pub fn new(count: usize, spawn: Spawn, require_rt: bool, ready: Arc<AtomicUsize>) -> Pool {
        let shared = Arc::new(Shared {
            generation: AtomicU64::new(0),
            next: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            job: UnsafeCell::new(Job { run: nothing, data: std::ptr::null_mut() }),
            stop: AtomicBool::new(false),
            ready,
            threads: (0..count).map(|_| OnceLock::new()).collect(),
            alive: AtomicUsize::new(0),
        });
        for i in 0..count {
            let s = shared.clone();
            s.alive.fetch_add(1, Relaxed);
            let started = spawn(Box::new(move || {
                if !require_rt || became_realtime() {
                    let _ = s.threads[i].set(std::thread::current());
                    s.ready.fetch_add(1, Relaxed);
                    let mut seen = 0;
                    while !s.stop.load(Acquire) {
                        let g = s.generation.load(Acquire);
                        if g == seen {
                            std::thread::park();
                            continue;
                        }
                        seen = g;
                        work(&s);
                    }
                }
                s.alive.fetch_sub(1, Release);
            }));
            if !started {
                shared.alive.fetch_sub(1, Relaxed);
            }
        }
        Pool { shared }
    }

    pub fn workers(&self) -> usize {
        self.shared.ready.load(Relaxed)
    }

    /// Calls `f(i)` for every `i` in `0..total`, spread over this thread and the workers, and
    /// returns once all are done. `f` must be safe to call for different `i` at once.
    pub fn run<F: Fn(usize) + Sync>(&self, total: usize, f: &F) {
        unsafe fn call<F: Fn(usize)>(data: *mut (), i: usize) {
            unsafe { (*(data as *const F))(i) }
        }
        if total == 0 {
            return;
        }
        if self.workers() == 0 || total == 1 {
            (0..total).for_each(f);
            return;
        }
        let s = &*self.shared;
        unsafe { *s.job.get() = Job { run: call::<F>, data: f as *const F as *mut () } };
        s.total.store(total, Relaxed);
        s.done.store(0, Relaxed);
        s.next.store(0, Release);
        s.generation.fetch_add(1, Release);
        for t in s.threads.iter().filter_map(|t| t.get()) {
            t.unpark();
        }
        work(s);
        while s.done.load(Acquire) < total {
            std::hint::spin_loop();
        }
        // Nothing may still be reading `f` once this returns.
        s.next.store(usize::MAX / 2, Release);
    }
}

/// Whether this thread runs at real-time priority, allowing a moment for it to be granted:
/// PipeWire asks RTKit for it after the thread has already started.
fn became_realtime() -> bool {
    for _ in 0..100 {
        let policy = unsafe { libc::sched_getscheduler(0) } & !libc::SCHED_RESET_ON_FORK;
        if policy == libc::SCHED_FIFO || policy == libc::SCHED_RR {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

fn work(s: &Shared) {
    loop {
        let i = s.next.fetch_add(1, AcqRel);
        if i >= s.total.load(Acquire) {
            return;
        }
        let job = unsafe { *s.job.get() };
        unsafe { (job.run)(job.data, i) };
        s.done.fetch_add(1, Release);
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let s = &self.shared;
        s.stop.store(true, Release);
        while s.alive.load(Acquire) > 0 {
            for t in s.threads.iter().filter_map(|t| t.get()) {
                t.unpark();
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

/// A `Spawn` on plain threads, for tests and for running without a real-time server.
pub fn std_spawn(f: Box<dyn FnOnce() + Send>) -> bool {
    std::thread::Builder::new().name("solstice-dsp".into()).spawn(f).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_item_runs_once() {
        let pool = Pool::new(3, &std_spawn, false, Arc::default());
        while pool.workers() < 3 {
            std::thread::yield_now();
        }
        for round in 0..200 {
            let hits: Vec<AtomicUsize> = (0..17).map(|_| AtomicUsize::new(0)).collect();
            pool.run(hits.len(), &|i| {
                hits[i].fetch_add(round + 1, Relaxed);
            });
            assert!(hits.iter().all(|h| h.load(Relaxed) == round + 1));
        }
    }

    #[test]
    fn workers_that_cannot_go_real_time_stay_out() {
        let pool = Pool::new(2, &std_spawn, true, Arc::default());
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(pool.workers(), 0);
        let sum = AtomicUsize::new(0);
        pool.run(5, &|i| {
            sum.fetch_add(i, Relaxed);
        });
        assert_eq!(sum.load(Relaxed), 10);
    }
}
