// Summarizing thousands of callables in parallel is allocation-bound: on a
// 393-file corpus mimalloc cuts the median cold run from 5.43 s to 4.91 s
// and the spread from 0.91 s to 0.06 s, at the same peak memory.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::process::ExitCode;

fn main() -> ExitCode {
    qual::cli::run()
}
