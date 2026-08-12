//! Informative release-only allocation/timing curves for static concatenation,
//! immutable loop-carried append, and `StringBuilder`. No timing threshold is asserted.

use std::{
    fmt::Write as _,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::{compile, compile_project};

const SAMPLES: usize = 5;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn project_module(source: &str) -> aster_mir::Module {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-string-matrix-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write StringBuilder matrix source");
    let module = compile_project(&path)
        .expect("StringBuilder matrix source compiles")
        .compilation
        .mir;
    std::fs::remove_file(path).expect("remove StringBuilder matrix source");
    module
}

fn measure(source: &str, expected: i32, needs_stdlib: bool) -> (f64, MemoryStats) {
    let module = if needs_stdlib {
        project_module(source)
    } else {
        compile(source).expect("string matrix source compiles").mir
    };
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
    let expected = "x".repeat(usize::try_from(appends).expect("appends fit usize"));
    format!(
        "public int Main() {{ string value = \"\"; int i = 0; \
         while (i < {appends}) {{ value = value + \"x\"; i = i + 1; }} \
         return value == \"{expected}\" ? value.Length : -1; }}"
    )
}

fn builder_append(appends: i32) -> String {
    let expected = "x".repeat(usize::try_from(appends).expect("appends fit usize"));
    format!(
        "using aster.core; public int Main() {{ StringBuilder builder = new StringBuilder(); \
         int i = 0; while (i < {appends}) {{ builder.Append(\"x\"); i = i + 1; }} \
         string value = builder.ToString(); \
         return value == \"{expected}\" ? value.Length : -1; }}"
    )
}

fn print(case: &str, size: usize, timing: f64, memory: &MemoryStats) {
    println!(
        "case={case:<13} size={size:<6} median_ms={timing:>9.3} allocations={:>8} strings={:>8} requested_bytes={:>12} used_bytes={:>12} reserved_bytes={:>12} peak_used_bytes={:>12} peak_reserved_bytes={:>12}",
        memory.total_allocations,
        memory.string_allocations,
        memory.requested_bytes,
        memory.used_bytes,
        memory.reserved_bytes,
        memory.peak_used_bytes,
        memory.peak_reserved_bytes,
    );
}

fn main() {
    for parts in [2, 4, 8, 16, 32, 64] {
        let (timing, memory) = measure(
            &static_chain(parts),
            i32::try_from(parts).expect("parts fit int"),
            false,
        );
        print("static-chain", parts, timing, &memory);
    }
    for appends in [1_000, 2_000, 4_000, 20_000] {
        let (timing, memory) = measure(&loop_append(appends), appends, false);
        print(
            "loop-append",
            usize::try_from(appends).expect("appends fit usize"),
            timing,
            &memory,
        );
    }
    // At 100K, immutable concatenation requests about 5 GiB before arena
    // reservation overhead and is rejected by the 1 GiB context limit, so
    // only the builder path is measured at that size.
    for appends in [1_000, 2_000, 4_000, 20_000, 100_000] {
        let (timing, memory) = measure(&builder_append(appends), appends, true);
        print(
            "builder-append",
            usize::try_from(appends).expect("appends fit usize"),
            timing,
            &memory,
        );
    }
}
