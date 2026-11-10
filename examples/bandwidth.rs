//! How much memory bandwidth this machine actually has, at every thread count the grid might use.
//!
//! The spatial rebuild's later passes are memory bound: `clear` writes the cell tables, `count` walks the entry list and
//! increments a 1.69 MB histogram, `place` scatters. A scaling curve for those passes means nothing without the ceiling
//! they are scaling towards, and a core count cannot state that ceiling. This measures it directly, with the same three
//! access patterns the rebuild uses:
//!
//! - `write`: store a constant into a buffer. What `clear` does to the cell tables, and what a page zeroing pass costs.
//! - `read`: sum a buffer. What `count` does to the entry list and the histogram.
//! - `copy`: read one buffer, write another. What `place` does when it walks entries and writes destinations.
//!
//! ```text
//! cargo run --release --example bandwidth
//! cargo run --release --example bandwidth -- --threads 1,2,4,6,8,12,16 --size 256
//! ```
//!
//! Records are JSON lines in the same shape as `build_passes`, so `docs/dev/tuning/` can diff them. Each row states the
//! thread count, the kernel, the bytes it moved and the gigabytes per second, where a `copy` counts the read and the
//! write separately because both cross the bus.

use bevy_flock::domain::num;
use std::hint::black_box;
use std::time::Instant;

/// The schema's version, shared with the other harnesses: bumped only when a field changes meaning or disappears.
const SCHEMA_VERSION: u32 = 1;
/// Bytes in a gibibyte, so a figure can be read against a datasheet.
const GIB: f64 = 1_073_741_824.0;
/// Runs per configuration; the best is recorded, because a disturbed run is noise and not a property of the machine.
const RUNS: usize = 3;

/// `--threads 1,2,4,6`: the thread counts to measure. Defaults to one through the core count, then 8, 12 and 16.
///
/// The counts past the core count are deliberate. They answer a different question than "how fast is it": they say what
/// oversubscription costs, which is the honest thing to measure on a host that has six cores and a question about sixteen.
fn thread_sweep(cores: usize) -> Vec<usize> {
    let mut args = std::env::args().skip_while(|arg| arg != "--threads");
    args.next();
    let listed: Vec<usize> = args
        .next()
        .map(|list| {
            list.split(',')
                .filter_map(|item| item.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    if !listed.is_empty() {
        return listed;
    }
    let mut counts: Vec<usize> = (1..=cores.max(1)).collect();
    for extra in [8, 12, 16] {
        if extra > cores {
            counts.push(extra);
        }
    }
    counts
}

/// `--size 256`: the buffer in mebibytes. Defaults to 256, which is far past every cache on any machine this runs on,
/// so the result is a memory figure rather than a cache figure.
fn size_mib() -> usize {
    let mut args = std::env::args().skip_while(|arg| arg != "--size");
    args.next();
    args.next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(256)
}

/// Store a constant into every slot.
fn write_chunk(chunk: &mut [u64]) {
    for slot in chunk.iter_mut() {
        *slot = 0x9E37_79B9_7F4A_7C15;
    }
}

/// Sum every slot. The sum is returned so the loop cannot be removed as dead.
fn read_chunk(chunk: &[u64]) -> u64 {
    let mut total = 0_u64;
    for value in chunk {
        total = total.wrapping_add(*value);
    }
    total
}

/// Copy one buffer into another.
fn copy_chunk(destination: &mut [u64], source: &[u64]) {
    for (to, from) in destination.iter_mut().zip(source.iter()) {
        *to = *from;
    }
}

/// Split a buffer into at most `threads` disjoint chunks, sized so every byte is covered exactly once.
fn chunk_len(len: usize, threads: usize) -> usize {
    len.div_ceil(threads.max(1)).max(1)
}

/// The best of `RUNS` timings of `work`, in seconds.
///
/// The best rather than the median: this is a ceiling, and a run disturbed by the scheduler can only make the machine look
/// slower than it is. Reporting the fastest observation is the only unbiased way to state an upper bound.
fn best_of<F: FnMut()>(mut work: F) -> f64 {
    let mut best = f64::INFINITY;
    for _ in 0..RUNS {
        let started = Instant::now();
        work();
        let elapsed = started.elapsed().as_secs_f64();
        if elapsed < best {
            best = elapsed;
        }
    }
    best
}

/// Write throughput: one buffer of `words` u64 values, filled by `threads` workers.
fn write_rate(buffer: &mut [u64], threads: usize) -> f64 {
    let per = chunk_len(buffer.len(), threads);
    let seconds = best_of(|| {
        let chunks: Vec<&mut [u64]> = buffer.chunks_mut(per).collect();
        std::thread::scope(|scope| {
            for chunk in chunks {
                scope.spawn(move || write_chunk(chunk));
            }
        });
    });
    let bytes = std::mem::size_of_val(buffer);
    num::usize_to_f64(bytes) / seconds / GIB
}

/// Read throughput: one buffer, summed by `threads` workers.
fn read_rate(buffer: &[u64], threads: usize) -> f64 {
    let per = chunk_len(buffer.len(), threads);
    let seconds = best_of(|| {
        let chunks: Vec<&[u64]> = buffer.chunks(per).collect();
        let total = std::thread::scope(|scope| {
            chunks
                .into_iter()
                .map(|chunk| scope.spawn(move || read_chunk(chunk)))
                .map(|handle| handle.join().unwrap_or_default())
                .sum::<u64>()
        });
        black_box(total);
    });
    let bytes = std::mem::size_of_val(buffer);
    num::usize_to_f64(bytes) / seconds / GIB
}

/// Copy throughput: read plus write, so the figure is the traffic and not the payload.
fn copy_rate(destination: &mut [u64], source: &[u64], threads: usize) -> f64 {
    let per = chunk_len(source.len(), threads);
    let seconds = best_of(|| {
        let pairs: Vec<(&mut [u64], &[u64])> = destination
            .chunks_mut(per)
            .zip(source.chunks(per))
            .collect();
        std::thread::scope(|scope| {
            for (to, from) in pairs {
                scope.spawn(move || copy_chunk(to, from));
            }
        });
    });
    let bytes = std::mem::size_of_val(source) * 2;
    num::usize_to_f64(bytes) / seconds / GIB
}

fn main() {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let size = size_mib();
    let words = size * 1024 * 1024 / size_of::<u64>();
    println!(
        "{{\"kind\":\"meta\",\"schema_version\":{SCHEMA_VERSION},\"machine\":\"{}\",\"cores\":{cores},\
         \"size_mib\":{size},\"runs\":{RUNS}}}",
        std::env::consts::ARCH
    );

    let mut buffer = vec![0_u64; words];
    let source = vec![1_u64; words];
    for threads in thread_sweep(cores) {
        let write = write_rate(&mut buffer, threads);
        let read = read_rate(&buffer, threads);
        let copy = copy_rate(&mut buffer, &source, threads);
        for (kernel, rate) in [("write", write), ("read", read), ("copy", copy)] {
            println!(
                "{{\"kind\":\"bandwidth\",\"schema_version\":{SCHEMA_VERSION},\"kernel\":\"{kernel}\",\
                 \"threads\":{threads},\"size_mib\":{size},\"gib_s\":{rate}}}"
            );
        }
        eprintln!("{threads:>3} threads: write {write:6.1} read {read:6.1} copy {copy:6.1} GiB/s");
    }
}

#[cfg(test)]
mod tests {
    use super::{chunk_len, copy_chunk, read_chunk, write_chunk};

    #[test]
    fn every_byte_is_in_exactly_one_chunk() {
        for len in [1_usize, 7, 8, 9, 1000, 4097] {
            for threads in [1_usize, 2, 3, 6, 16] {
                let per = chunk_len(len, threads);
                let chunks: Vec<usize> = (0..len)
                    .collect::<Vec<_>>()
                    .chunks(per)
                    .map(<[usize]>::len)
                    .collect();
                assert_eq!(
                    chunks.iter().sum::<usize>(),
                    len,
                    "len {len} threads {threads}"
                );
                assert!(
                    chunks.len() <= threads.max(1),
                    "len {len} threads {threads} made {} chunks",
                    chunks.len()
                );
            }
        }
    }

    #[test]
    fn more_threads_than_words_still_makes_one_word_chunks() {
        assert_eq!(chunk_len(3, 16), 1);
        assert_eq!(chunk_len(0, 4), 1);
    }

    #[test]
    fn the_kernels_do_what_their_names_say() {
        let mut buffer = [0_u64; 4];
        write_chunk(&mut buffer);
        assert!(buffer.iter().all(|value| *value == 0x9E37_79B9_7F4A_7C15));
        assert_eq!(
            read_chunk(&buffer),
            4_u64.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        );

        let source = [1_u64, 2, 3, 4];
        let mut destination = [0_u64; 4];
        copy_chunk(&mut destination, &source);
        assert_eq!(destination, source);
    }
}
