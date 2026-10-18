//! Load mode: a busy agent, for measuring what a person feels while agents
//! work (docs/dev/performance.md, "Under load").
//!
//! Started with a prompt like `load for 20s`, the probe keeps every CPU busy
//! and writes to its scratch space as fast as the jail lets it, reporting
//! progress on its tile, and stops when the time is up or the task is
//! stopped. It reads no user data and calls no actions.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use glass_agent_sdk::Agent;

const DEFAULT: Duration = Duration::from_secs(20);
/// The file it rewrites, so a long run doesn't fill the disk.
const FILE_SIZE: u64 = 64 << 20;
const CHUNK: usize = 1 << 20;

/// `load for 20s` -> 20 seconds; anything else -> the default.
pub fn duration(prompt: &str) -> Duration {
    prompt
        .split_whitespace()
        .find_map(|word| word.strip_suffix('s')?.parse::<u64>().ok())
        .map(Duration::from_secs)
        .filter(|d| !d.is_zero() && *d <= Duration::from_secs(600))
        .unwrap_or(DEFAULT)
}

pub async fn run(agent: &Agent) -> Result<String, String> {
    let how_long = duration(&agent.prompt);
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    let stop = Arc::new(AtomicBool::new(false));
    let spins = Arc::new(AtomicU64::new(0));
    let written = Arc::new(AtomicU64::new(0));

    let mut threads = Vec::new();
    for _ in 0..cpus {
        let (stop, spins) = (Arc::clone(&stop), Arc::clone(&spins));
        threads.push(std::thread::spawn(move || burn(&stop, &spins)));
    }
    let writer = {
        let (stop, written) = (Arc::clone(&stop), Arc::clone(&written));
        let path = agent.work_dir.join("load.bin");
        std::thread::spawn(move || write_loop(&path, &stop, &written))
    };

    let started = Instant::now();
    while started.elapsed() < how_long {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let secs = started.elapsed().as_secs_f64();
        let mb = written.load(Ordering::Relaxed) as f64 / f64::from(1 << 20);
        let _ = agent
            .progress(
                &format!(
                    "Busy on {cpus} CPUs, writing {:.0} MB/s",
                    mb / secs.max(0.001)
                ),
                Some(secs as u32),
                Some(how_long.as_secs() as u32),
            )
            .await;
    }
    stop.store(true, Ordering::Relaxed);
    for thread in threads {
        let _ = thread.join();
    }
    let write_error = writer
        .join()
        .map_err(|_| "the writer panicked".to_owned())?;
    let _ = std::fs::remove_file(agent.work_dir.join("load.bin"));

    let secs = started.elapsed().as_secs_f64();
    let mb = written.load(Ordering::Relaxed) as f64 / f64::from(1 << 20);
    let summary = format!(
        "Load: {secs:.0}s on {cpus} CPUs, wrote {mb:.0} MB ({:.0} MB/s), {:.1}G spins",
        mb / secs,
        spins.load(Ordering::Relaxed) as f64 / 1e9
    );
    match write_error {
        Some(e) => Err(format!("{summary}; writing stopped: {e}")),
        None => Ok(summary),
    }
}

fn burn(stop: &AtomicBool, spins: &AtomicU64) {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    while !stop.load(Ordering::Relaxed) {
        for _ in 0..1_000_000 {
            x = std::hint::black_box(x.rotate_left(5) ^ x.wrapping_mul(31));
        }
        spins.fetch_add(1_000_000, Ordering::Relaxed);
    }
}

/// Writes 1 MB at a time, flushing to disk every 16 MB and starting over at
/// 64 MB. Returns the error that stopped it, if any.
fn write_loop(path: &Path, stop: &AtomicBool, written: &AtomicU64) -> Option<String> {
    let mut file = match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => return Some(e.to_string()),
    };
    // Varied bytes, so compression can't make the writes free.
    let mut chunk = vec![0u8; CHUNK];
    let mut seed: u32 = 0x1234_5678;
    for b in &mut chunk {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        *b = seed as u8;
    }
    let mut at = 0u64;
    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = file.write_all(&chunk) {
            return Some(e.to_string());
        }
        chunk[(at >> 20) as usize % CHUNK] ^= 0xff;
        at += CHUNK as u64;
        written.fetch_add(CHUNK as u64, Ordering::Relaxed);
        if at.is_multiple_of(16 << 20) {
            let _ = file.sync_data();
        }
        if at >= FILE_SIZE {
            at = 0;
            if let Err(e) = file.seek(SeekFrom::Start(0)) {
                return Some(e.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_duration_from_the_prompt() {
        assert_eq!(duration("load for 15s"), Duration::from_secs(15));
        assert_eq!(duration("load"), DEFAULT);
        assert_eq!(duration("load for 0s"), DEFAULT);
        assert_eq!(duration("load for 9999s"), DEFAULT);
    }
}
