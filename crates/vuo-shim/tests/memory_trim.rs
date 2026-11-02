//! `memory::release_free_memory` hands a freed peak back to the OS.
//!
//! Own test binary, one test: resident size is per PROCESS. Beside other tests
//! in same process, their allocations land between two readings and move the
//! number this measures.
//!
//! glibc only (call is glibc's).

// Test code: panic denials guard production foreign-input paths, not test
// assertions.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(target_env = "gnu")]

use vuo_shim::memory::release_free_memory;

/// Resident set size in bytes. glibc target = Linux: procfs is there.
fn resident_bytes() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").expect("procfs");
    let pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .expect("statm's resident field");
    pages * 4096
}

/// §the freed peak actually goes back, rather than staying resident.
///
/// Shape that matters: many heap-sized blocks freed while something ALLOCATED
/// LATER lives above. `free` trims only heap top; live block pins everything
/// below, however free: exactly what sync pass leaves, what allocator won't
/// undo alone.
#[test]
fn a_freed_peak_is_returned_to_the_operating_system() {
    const BLOCK: usize = 16 * 1024;
    const BLOCKS: usize = 4096;

    let baseline = resident_bytes();

    // Touched, not just reserved: untouched page not resident; test would pass
    // measuring nothing.
    let mut blocks: Vec<Vec<u8>> = (0..BLOCKS).map(|_| vec![1u8; BLOCK]).collect();
    let peak = resident_bytes();
    assert!(
        peak > baseline + (BLOCK * BLOCKS / 2) as u64,
        "the fixture did not actually become resident: {baseline} -> {peak}"
    );

    // Last block stays alive, everything under goes. The pin: without it
    // `free` walks heap top down itself, nothing left for call under test.
    let pin = blocks.pop();
    blocks.clear();
    blocks.shrink_to_fit();

    let freed = resident_bytes();
    release_free_memory();
    let trimmed = resident_bytes();
    drop(pin);

    let returned = freed.saturating_sub(trimmed);
    assert!(
        returned > (BLOCK * BLOCKS / 2) as u64,
        "freeing left {freed} bytes resident and the trim returned only {returned} of them"
    );
}
