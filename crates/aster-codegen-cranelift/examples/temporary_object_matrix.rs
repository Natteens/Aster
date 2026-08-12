//! Manual release-build comparison of temporary objects allocated directly in
//! one function scope versus through a short helper scope. Timings are
//! informative only and are never asserted by tests.

use std::time::Instant;

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute, execute_with_stats};
use aster_compiler::compile;

const SAMPLES: usize = 7;

fn source(iterations: usize, helper: bool) -> String {
    let body = if helper {
        "total += Build();"
    } else {
        "Box box = new Box(); box.value = 1; total += box.value;"
    };
    format!(
        "public class Box {{ public int value; }} \
         internal int Build() {{ Box box = new Box(); box.value = 1; return box.value; }} \
         public int Run() {{ int total = 0; for (int index = 0; index < {iterations}; index++) {{ {body} }} return total; }}"
    )
}

fn run_case(iterations: usize, helper: bool) -> (f64, MemoryStats) {
    let compilation = compile(&source(iterations, helper)).expect("benchmark source compiles");
    let expected = ExecutionValue::Int(i32::try_from(iterations).expect("iterations fit in int"));
    let (value, stats) =
        execute_with_stats(&compilation.mir, "Run").expect("benchmark source executes");
    assert_eq!(value, expected);
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let value = execute(&compilation.mir, "Run").expect("benchmark source executes");
        samples.push(start.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(value, expected);
    }
    samples.sort_by(f64::total_cmp);
    (samples[SAMPLES / 2], stats)
}

fn main() {
    for iterations in [100_000, 500_000, 1_000_000, 2_000_000, 4_000_000] {
        for (shape, helper) in [("direct", false), ("helper", true)] {
            let (median_ms, stats) = run_case(iterations, helper);
            println!(
                "shape={shape:<6} iterations={iterations:<7} median_ms={median_ms:>9.3} allocations={:>8} requested_bytes={:>10} used_bytes={:>9} peak_used_bytes={:>9} reserved_bytes={:>9}",
                stats.object_allocations,
                stats.requested_bytes,
                stats.used_bytes,
                stats.peak_used_bytes,
                stats.reserved_bytes,
            );
        }
    }
}
