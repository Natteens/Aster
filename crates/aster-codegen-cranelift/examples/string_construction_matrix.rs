//! Informative release-only allocation/timing curves for static concatenation
//! chains and immutable loop-carried append. No timing threshold is asserted.

use std::{fmt::Write as _, time::Instant};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::compile;

const SAMPLES: usize = 5;

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn measure(source: &str, expected: i32) -> (f64, MemoryStats) {
    let module = compile(source).expect("string matrix source compiles").mir;
    let mut durations = Vec::with_capacity(SAMPLES);
    let mut memory = None;
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let (value, stats) = execute_with_stats(&module, "Main").expect("matrix executes");
        durations.push(start.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(value, ExecutionValue::Int(expected));
        memory = Some(stats);
    }
    (median(durations), memory.expect("at least one sample"))
}

fn static_chain(parts: usize) -> String {
    let mut source = String::from("public int Main() { string p = \"x\"; string value = ");
    for index in 0..parts {
        if index != 0 {
            source.push_str(" + ");
        }
        source.push('p');
    }
    write!(source, "; return value.Length; }}").expect("write source");
    source
}

fn loop_append(appends: i32) -> String {
    format!(
        "public int Main() {{ string value = \"\"; int i = 0; \
         while (i < {appends}) {{ value = value + \"x\"; i = i + 1; }} \
         return value.Length; }}"
    )
}

fn print(case: &str, size: usize, timing: f64, memory: &MemoryStats) {
    println!(
        "case={case:<13} size={size:<5} median_ms={timing:>9.3} allocations={:>8} requested_bytes={:>12} used_bytes={:>12} peak_used_bytes={:>12}",
        memory.string_allocations,
        memory.requested_bytes,
        memory.used_bytes,
        memory.peak_used_bytes,
    );
}

fn main() {
    for parts in [2, 4, 8, 16, 32, 64] {
        let (timing, memory) = measure(
            &static_chain(parts),
            i32::try_from(parts).expect("parts fit int"),
        );
        print("static-chain", parts, timing, &memory);
    }
    for appends in [1_000, 2_000, 4_000] {
        let (timing, memory) = measure(&loop_append(appends), appends);
        print(
            "loop-append",
            usize::try_from(appends).expect("appends fit usize"),
            timing,
            &memory,
        );
    }
}
