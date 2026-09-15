//! Handing the sync's peak back to the operating system.
//!
//! A sync pass is the largest thing Vuo ever allocates. A page of entries
//! arrives as compressed bytes, is inflated, is parsed into owned `String`s,
//! and is written to the mirror; a scrape does the same for one article's
//! whole body. Every byte of that is freed as soon as the pass ends -- and on
//! glibc, freed is not the same as returned.
//!
//! glibc gives a freed block back to the OS only when it happens to sit at the
//! top of the heap, and even then only past `M_TRIM_THRESHOLD`. Anything
//! freed below a still-live allocation stays mapped: the process keeps the
//! address space, the kernel keeps the pages, and `ps` keeps reporting them.
//! Worse, the mmap threshold is DYNAMIC -- glibc raises it, up to 32 MB, each
//! time it sees a large mmapped block freed -- so the big allocations that
//! would have been returned automatically stop being mmapped at all after the
//! first few passes and start coming from the heap like everything else.
//!
//! The result is a process whose resident size is its worst pass rather than
//! its working set, on a phone where that number is what gets it killed.
//! [`release_free_memory`] is the one call that fixes it.

/// Return every free page glibc is sitting on to the kernel.
///
/// Call this after work that peaked -- a sync pass, a scrape -- and not on the
/// Qt thread: it takes each arena's lock in turn, so it is brief but it is not
/// free, and the sync worker is the thread that can afford it.
///
/// A no-op where there is no glibc to ask. `malloc_trim` is a glibc extension:
/// musl has no equivalent and neither does any other libc Vuo could be built
/// against, so this compiles to nothing there rather than failing to link.
#[cfg(target_env = "gnu")]
pub fn release_free_memory() {
    // The only `unsafe` this crate writes by hand rather than generating; see
    // the crate docs. `malloc_trim` takes a byte count to leave untrimmed at
    // the top of the heap and returns whether it freed anything. It reads and
    // writes only the allocator's own bookkeeping, touches no memory this
    // program owns, takes each arena's lock while it works, and has no failure
    // mode other than doing nothing -- so there is no precondition to get
    // wrong and no state a caller could observe it through.
    unsafe {
        malloc_trim(0);
    }
}

#[cfg(target_env = "gnu")]
extern "C" {
    /// `int malloc_trim(size_t pad)` from glibc's `malloc.h`.
    fn malloc_trim(pad: usize) -> std::os::raw::c_int;
}

#[cfg(not(target_env = "gnu"))]
pub fn release_free_memory() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resident set size in bytes, or `None` where there is no procfs.
    fn resident_bytes() -> Option<u64> {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * 4096)
    }

    /// §the freed peak actually goes back, rather than staying resident.
    ///
    /// The shape is the one that matters, not an easier one: a large number of
    /// heap-sized blocks freed while something ALLOCATED LATER is still alive
    /// above them. `free` trims only the top of the heap, so that live block
    /// pins everything below it however much of it is free -- which is exactly
    /// what a sync pass leaves behind, and exactly what the allocator will not
    /// undo on its own.
    ///
    /// glibc only, because the call is glibc's. Skipped rather than failed
    /// anywhere else, and skipped rather than failed where procfs is not
    /// mounted.
    #[test]
    #[cfg(target_env = "gnu")]
    fn a_freed_peak_is_returned_to_the_operating_system() {
        const BLOCK: usize = 16 * 1024;
        const BLOCKS: usize = 4096;

        let Some(baseline) = resident_bytes() else {
            return;
        };

        // Touched, not merely reserved: an untouched page is not resident and
        // would make this test pass without measuring anything.
        let mut blocks: Vec<Vec<u8>> = (0..BLOCKS).map(|_| vec![1u8; BLOCK]).collect();
        let peak = resident_bytes().unwrap_or(baseline);
        assert!(
            peak > baseline + (BLOCK * BLOCKS / 2) as u64,
            "the fixture did not actually become resident: {baseline} -> {peak}"
        );

        // The last block stays alive and everything under it goes. This is the
        // pin: without it `free` walks the top of the heap down by itself and
        // there is nothing left for the call under test to do.
        let pin = blocks.pop();
        blocks.clear();
        blocks.shrink_to_fit();

        let freed = resident_bytes().unwrap_or(baseline);
        release_free_memory();
        let trimmed = resident_bytes().unwrap_or(baseline);
        drop(pin);

        let returned = freed.saturating_sub(trimmed);
        assert!(
            returned > (BLOCK * BLOCKS / 2) as u64,
            "freeing left {freed} bytes resident and the trim returned only {returned} of them"
        );
    }
}
