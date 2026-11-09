//! What a thread hand-off costs, per mechanism and per pass, with the allocations counted.
//!
//! Phase 5's first question is whether the rebuild is paying for threads or for work. `place` moves 12 MB and
//! takes 2.2 ms at six workers; a `thread::scope` that does nothing takes something else entirely, and that
//! something is paid three times per rebuild — once for the clear, once for the count, once for the place.
//!
//! This measures three ways to get `n` workers going, all with an empty body so the figure is dispatch and
//! nothing else:
//!
//! - `scope` — `std::thread::scope` per pass, which is what the rebuild does today.
//! - `scope+barrier` — one scope for all rounds, with a [`std::sync::Barrier`] between them. This is the shape a
//!   persistent pool would have inside one rebuild: spawn once, meet at the barrier per pass.
//! - `pool` — `bevy_tasks::ComputeTaskPool::scope`, the crate's existing executor, which parks its threads.
//!
//! The allocator is counted because a dispatch that removes a thread spawn but adds a boxed task has not
//! necessarily bought anything: the no-allocation suites in this repository assert zero steady-frame
//! allocations, so a mechanism that allocates per pass is not adoptable however fast it is.
//!
//! Output is JSON lines so the numbers can be pasted into docs/performance.md without retyping them.
//!
//! ```text
//! cargo run --release --example spawn_cost
//! cargo run --release --example spawn_cost -- --threads 1,2,3,6
//! ```
// A counting allocator is the only way to price "spawn plus one boxed task" honestly, and `GlobalAlloc` is
// unsafe by definition. The unsafe code forwards to the system allocator; the crate itself still denies unsafe.
#![expect(
    unsafe_code,
    reason = "a counting allocator is the only way to price the allocations a dispatch mechanism makes"
)]
use bevy_flock::domain::num;
use bevy_tasks::{ComputeTaskPool, TaskPool};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Barrier;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// Rounds per sample: how many passes one scope is asked to cover.
const ROUNDS: usize = 5;
/// Samples per configuration; the median is recorded.
const REPEATS: usize = 15;
/// Dispatches per sample, so a single dispatch is above the timer's resolution.
const INNER: usize = 200;

/// Allocations made since the counter was last reset, across every thread.
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

// SAFETY: every method forwards to the system allocator with the same arguments and adds a relaxed counter
// increment, which does not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Run `body` and report the allocations it made.
fn counting<T>(body: impl FnOnce() -> T) -> (T, usize) {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let value = body();
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    (value, after.saturating_sub(before))
}

/// One `thread::scope` per round: what the rebuild does today.
fn std_scope(threads: usize) {
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {});
        }
    });
}

/// One scope for every round, meeting at a barrier between them: the shape a persistent pool would have.
fn std_scope_barrier(threads: usize) {
    let barrier = Barrier::new(threads.max(1));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                for _ in 0..ROUNDS {
                    barrier.wait();
                }
            });
        }
    });
}

/// The crate's existing executor: a pool that parks its threads between tasks.
fn pool_scope(threads: usize) {
    let pool = ComputeTaskPool::try_get()
        .unwrap_or_else(|| ComputeTaskPool::get_or_init(TaskPool::default));
    pool.scope(|scope| {
        for _ in 0..threads {
            scope.spawn(async {});
        }
    });
}

/// The median of the samples, or zero when there are none.
fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples.get(samples.len() / 2).copied().unwrap_or_default()
}

/// Time `dispatches` calls of `body`, and report the median microseconds per round and allocations per round.
fn measure(name: &str, threads: usize, rounds: usize, body: impl Fn(usize)) -> (f64, f64) {
    let mut times = Vec::with_capacity(REPEATS);
    let mut allocations = 0_usize;
    for _ in 0..REPEATS {
        let start = Instant::now();
        for _ in 0..INNER {
            body(threads);
        }
        let elapsed = start.elapsed().as_secs_f64();
        times.push(elapsed / num::usize_to_f64(INNER * rounds));
        let ((), made) = counting(|| body(threads));
        allocations = allocations.max(made / rounds.max(1));
    }
    let micros = median(times) * 1_000_000.0;
    let allocs = num::usize_to_f64(allocations);
    println!(
        "{{\"kind\":\"dispatch\",\"mechanism\":\"{name}\",\"threads\":{threads},\"rounds\":{rounds},\
         \"micros_per_round\":{micros:.2},\"allocs_per_round\":{allocs:.1}}}"
    );
    (micros, allocs)
}

/// `--threads 1,2,3,6` from the command line, or the machine's core count.
fn thread_counts() -> Vec<usize> {
    let mut args = std::env::args().skip_while(|arg| arg != "--threads");
    args.next();
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    args.next().map_or_else(
        || vec![1, 2, 3, cores],
        |list| {
            list.split(',')
                .filter_map(|item| item.parse().ok())
                .collect()
        },
    )
}

fn main() {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    println!(
        "{{\"kind\":\"meta\",\"machine\":\"{}\",\"cores\":{cores},\"rounds\":{ROUNDS},\
         \"repeats\":{REPEATS},\"inner\":{INNER}}}",
        std::env::consts::ARCH
    );

    for threads in thread_counts() {
        // A scope per round, so the per-round figure is one spawn-and-join of `threads` bodies.
        measure("scope", threads, 1, |count| {
            for _ in 0..ROUNDS {
                std_scope(count);
            }
        });
        // One scope covering every round: spawn cost amortised across the passes.
        measure("scope+barrier", threads, ROUNDS, |count| {
            std_scope_barrier(count);
        });
        // The parked-thread executor, for the same round count.
        measure("pool", threads, ROUNDS, |count| {
            for _ in 0..ROUNDS {
                pool_scope(count);
            }
        });
    }
}
