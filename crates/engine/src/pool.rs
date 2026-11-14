//! Worker threads that share a period's independent track processing with the audio thread.
//!
//! The audio thread publishes a job, wakes the workers (a futex wake: no locks, no
//! allocation), takes items itself and spins until every item is done. Workers only help, but
//! an item a worker has taken must finish before the period can: workers run at the audio
//! thread's real-time priority, just under it, so nothing else on the system gets in first.
//!
//! Items are claimed with the job's number alongside the index, so a worker that wakes late
//! can never take an item of the next job.

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize};
use std::sync::OnceLock;
use std::thread::Thread;

/// Starts a thread running the given function; false if it couldn't.
pub type Spawn<'a> = &'a dyn Fn(Box<dyn FnOnce() + Send>) -> bool;

#[derive(Clone, Copy)]
struct Job {
    run: unsafe fn(*mut (), usize),
    data: *mut (),
}

/// The index part of a claim once a job is finished.
const CLOSED: u64 = u32::MAX as u64;

struct Shared {
    generation: AtomicU64,
    /// The current job's number in the high half and the next unclaimed item in the low half.
    claim: AtomicU64,
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
    /// The audio thread's SCHED_FIFO priority, 0 when it has none. Read again now and then,
    /// since the server can raise it after the first periods.
    priority: AtomicI32,
    jobs: AtomicU64,
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
            claim: AtomicU64::new(CLOSED),
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            job: UnsafeCell::new(Job { run: nothing, data: std::ptr::null_mut() }),
            stop: AtomicBool::new(false),
            ready,
            threads: (0..count).map(|_| OnceLock::new()).collect(),
            alive: AtomicUsize::new(0),
            priority: AtomicI32::new(0),
            jobs: AtomicU64::new(0),
        });
        for i in 0..count {
            let s = shared.clone();
            s.alive.fetch_add(1, Relaxed);
            let started = spawn(Box::new(move || {
                if !require_rt || became_realtime() {
                    let _ = s.threads[i].set(std::thread::current());
                    s.ready.fetch_add(1, Relaxed);
                    let mut seen = 0;
                    let mut priority = 0;
                    while !s.stop.load(Acquire) {
                        let g = s.generation.load(Acquire);
                        if g == seen {
                            std::thread::park();
                            continue;
                        }
                        seen = g;
                        let want = s.priority.load(Relaxed);
                        if want != priority {
                            priority = want;
                            if want > 1 {
                                // RTKit's threads carry SCHED_RESET_ON_FORK, which an
                                // unprivileged thread may not clear.
                                let param = libc::sched_param { sched_priority: want - 1 };
                                let policy = libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK;
                                unsafe { libc::pthread_setschedparam(libc::pthread_self(), policy, &param) };
                            }
                        }
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
        if s.jobs.fetch_add(1, Relaxed) % 4096 == 0 {
            s.priority.store(own_priority(), Relaxed);
        }
        let number = s.generation.load(Relaxed).wrapping_add(1) & CLOSED;
        unsafe { *s.job.get() = Job { run: call::<F>, data: f as *const F as *mut () } };
        s.total.store(total, Relaxed);
        s.done.store(0, Relaxed);
        s.claim.store(number << 32, Release);
        s.generation.fetch_add(1, Release);
        for t in s.threads.iter().filter_map(|t| t.get()) {
            t.unpark();
        }
        work(s);
        while s.done.load(Acquire) < total {
            std::hint::spin_loop();
        }
        // Nothing may still be reading `f` once this returns.
        s.claim.store((number << 32) | CLOSED, Release);
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

/// This thread's SCHED_FIFO or SCHED_RR priority, 0 for none.
fn own_priority() -> i32 {
    let mut policy = 0;
    let mut param = libc::sched_param { sched_priority: 0 };
    unsafe { libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param) };
    let policy = policy & !libc::SCHED_RESET_ON_FORK;
    if policy == libc::SCHED_FIFO || policy == libc::SCHED_RR { param.sched_priority } else { 0 }
}

fn work(s: &Shared) {
    loop {
        let claim = s.claim.load(Acquire);
        let i = claim & CLOSED;
        // Read before the claim is taken: if another job has been published since, its claim
        // differs and the exchange below fails.
        if i == CLOSED || i as usize >= s.total.load(Acquire) {
            return;
        }
        if s.claim.compare_exchange_weak(claim, claim + 1, AcqRel, Relaxed).is_err() {
            continue;
        }
        let job = unsafe { *s.job.get() };
        unsafe { (job.run)(job.data, i as usize) };
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
    fn back_to_back_jobs_never_mix() {
        let pool = Pool::new(3, &std_spawn, false, Arc::default());
        while pool.workers() < 3 {
            std::thread::yield_now();
        }
        for _ in 0..20000 {
            let a: Vec<AtomicUsize> = (0..3).map(|_| AtomicUsize::new(0)).collect();
            let b: Vec<AtomicUsize> = (0..5).map(|_| AtomicUsize::new(0)).collect();
            pool.run(a.len(), &|i| {
                a[i].fetch_add(1, Relaxed);
            });
            pool.run(b.len(), &|i| {
                b[i].fetch_add(1, Relaxed);
            });
            assert!(a.iter().chain(&b).all(|h| h.load(Relaxed) == 1));
        }
    }

    #[test]
    fn workers_follow_the_audio_threads_priority() {
        let pool = Arc::new(Pool::new(2, &std_spawn, false, Arc::default()));
        while pool.workers() < 2 {
            std::thread::yield_now();
        }
        let p = pool.clone();
        let seen = std::thread::spawn(move || {
            let param = libc::sched_param { sched_priority: 20 };
            if unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) } != 0 {
                return None;
            }
            let worst = std::sync::atomic::AtomicI32::new(i32::MAX);
            for _ in 0..200 {
                p.run(8, &|_| {
                    let mut policy = 0;
                    let mut sp = libc::sched_param { sched_priority: 0 };
                    unsafe { libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut sp) };
                    let prio = if policy & !libc::SCHED_RESET_ON_FORK == libc::SCHED_FIFO { sp.sched_priority } else { 0 };
                    worst.fetch_min(prio, Relaxed);
                    std::thread::sleep(std::time::Duration::from_micros(50));
                });
            }
            Some(worst.load(Relaxed))
        })
        .join()
        .unwrap();
        // Without a real-time limit there is nothing to check.
        if let Some(worst) = seen {
            assert_eq!(worst, 19, "workers run just under the audio thread");
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
