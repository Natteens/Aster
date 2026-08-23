//! Compact release-only throughput and allocation sanity matrix for the
//! practical standard-library bulk APIs. JIT preparation is excluded from
//! samples; timings are informational and have no pass/fail threshold.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, PreparedSequentialExecution};
use aster_compiler::compile_project;
use aster_runtime::MemoryFileSystemBackend;

const SAMPLES: usize = 11;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
struct Timing {
    median: f64,
    p25: f64,
    p75: f64,
}

fn module(source: &str) -> aster_mir::Module {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-practical-stdlib-matrix-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write practical stdlib matrix source");
    let module = compile_project(&path)
        .expect("practical stdlib matrix source compiles")
        .compilation
        .mir;
    std::fs::remove_file(path).expect("remove practical stdlib matrix source");
    module
}

fn measure(source: &str, expected: i32) -> (Timing, MemoryStats) {
    let prepared = PreparedSequentialExecution::prepare(&module(source), "Main")
        .expect("matrix JIT prepares before timing");
    assert_eq!(
        prepared.invoke().expect("warmup executes"),
        ExecutionValue::Int(expected)
    );
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        assert_eq!(
            prepared.invoke().expect("sample executes"),
            ExecutionValue::Int(expected)
        );
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    samples.sort_by(f64::total_cmp);
    let (_, stats) = prepared
        .invoke_with_stats()
        .expect("stats execution succeeds");
    (
        Timing {
            median: samples[SAMPLES / 2],
            p25: samples[SAMPLES / 4],
            p75: samples[SAMPLES * 3 / 4],
        },
        stats,
    )
}

fn measure_read_lines(source: &str, content: &[u8], expected: i32) -> (Timing, MemoryStats) {
    let prepared = PreparedSequentialExecution::prepare(&module(source), "Main")
        .expect("ReadAllLines matrix JIT prepares before timing");
    let filesystem = MemoryFileSystemBackend::new().with_file("input.txt", content);
    assert_eq!(
        prepared
            .invoke_with_filesystem(Box::new(filesystem.clone()))
            .expect("warmup executes"),
        ExecutionValue::Int(expected)
    );
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        assert_eq!(
            prepared
                .invoke_with_filesystem(Box::new(filesystem.clone()))
                .expect("sample executes"),
            ExecutionValue::Int(expected)
        );
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    samples.sort_by(f64::total_cmp);
    let (_, stats) = prepared
        .invoke_with_filesystem_and_stats(Box::new(filesystem))
        .expect("stats execution succeeds");
    (
        Timing {
            median: samples[SAMPLES / 2],
            p25: samples[SAMPLES / 4],
            p75: samples[SAMPLES * 3 / 4],
        },
        stats,
    )
}

fn print(case: &str, operations: usize, timing: Timing, memory: &MemoryStats) {
    println!(
        "case={case:<22} operations={operations:<8} median_ms={:>9.3} p25_ms={:>9.3} p75_ms={:>9.3} allocations={:>8} requested_bytes={:>12} peak_used={:>12} capacity={:>12}",
        timing.median,
        timing.p25,
        timing.p75,
        memory.total_allocations,
        memory.requested_bytes,
        memory.peak_used_bytes,
        memory.peak_reserved_bytes,
    );
}

fn main() {
    let join_values = (0..1_000).map(|_| "\"x\"").collect::<Vec<_>>().join(",");
    let join = format!(
        "using aster.text; public int Main() {{ string[] values = [{join_values}]; int total = 0; for (int i = 0; i < 100; i++) {{ total += String.Join(\",\", values).Length; }} return total; }}"
    );
    let (timing, stats) = measure(&join, 199_900);
    print("String.Join", 100_000, timing, &stats);

    let repeat =
        "using aster.text; public int Main() { return String.Repeat(\"x\", 100000).Length; }";
    let (timing, stats) = measure(repeat, 100_000);
    print("String.Repeat", 100_000, timing, &stats);

    let builder = "using aster.core; public int Main() { StringBuilder builder = new(); for (int i = 0; i < 100000; i++) { builder.Append(i); } return builder.Length; }";
    let (timing, stats) = measure(builder, 488_890);
    print("StringBuilder scalar", 100_000, timing, &stats);

    let list_add = "public int Main() { List<int> values = new(); for (int i = 0; i < 100000; i++) { values.Add(i); } return values.Length; }";
    let (timing, stats) = measure(list_add, 100_000);
    print("List.Add", 100_000, timing, &stats);

    let list_add_range = "using aster.collections; public int Main() { int[] chunk = new int[1000]; List<int> values = new(); for (int i = 0; i < 100; i++) { values.AddRange(chunk); } return values.Length; }";
    let (timing, stats) = measure(list_add_range, 100_000);
    print("List.AddRange", 100_000, timing, &stats);

    let array_copy = "using aster.collections; public int Main() { int[] source = new int[100000]; Array.Fill<int>(source, 7); int[] copy = Array.Copy<int>(source); return copy[99999]; }";
    let (timing, stats) = measure(array_copy, 7);
    print("Array.Copy", 100_000, timing, &stats);

    let read_lines = "using aster.io; public int Main() { switch (ReadAllLines(\"input.txt\")) { case Ok(lines): return lines.Length; case Error(error): return -1; } }";
    let content = "line\n".repeat(10_000);
    let (timing, stats) = measure_read_lines(read_lines, content.as_bytes(), 10_000);
    print("ReadAllLines", 10_000, timing, &stats);

    let random = "using aster.random; public int Main() { Random random = new(123UL); uint value = 0u; int count = 0; for (int i = 0; i < 1000000; i++) { value = random.NextUInt(); count++; } if (value == 0u) { return -1; } return count; }";
    let (timing, stats) = measure(random, 1_000_000);
    print("Random.NextUInt", 1_000_000, timing, &stats);

    let random = "using aster.random; public int Main() { Random random = new(123UL); ulong value = 0UL; int count = 0; for (int i = 0; i < 1000000; i++) { value = random.NextULong(); count++; } if (value == 0UL) { return -1; } return count; }";
    let (timing, stats) = measure(random, 1_000_000);
    print("Random.NextULong", 1_000_000, timing, &stats);
}
