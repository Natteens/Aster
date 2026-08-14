//! Release-only execution curves for immutable loop concat, the automatic
//! rewrite, and explicit `StringBuilder`. JIT preparation is excluded from
//! timed samples; timing remains informational.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use aster_codegen_cranelift::{
    AarmMemoryTelemetry, ExecutionValue, MemoryStats, PreparedSequentialExecution,
};
use aster_compiler::{
    compile, compile_project, compile_without_loop_string_concat_rewrite_for_research,
};

const SAMPLES: usize = 5;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
enum Mode {
    Immutable,
    Automatic,
    ExplicitBuilder,
}

fn project_module(source: &str) -> aster_mir::Module {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-string-matrix-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write explicit StringBuilder matrix source");
    let module = compile_project(&path)
        .expect("explicit StringBuilder matrix source compiles")
        .compilation
        .mir;
    std::fs::remove_file(path).expect("remove explicit StringBuilder matrix source");
    module
}

fn module(source: &str, mode: Mode) -> aster_mir::Module {
    match mode {
        Mode::Immutable => {
            compile_without_loop_string_concat_rewrite_for_research(source)
                .expect("immutable source compiles")
                .mir
        }
        Mode::Automatic => compile(source).expect("automatic source compiles").mir,
        Mode::ExplicitBuilder => project_module(source),
    }
}

fn fine_regions(module: &aster_mir::Module) -> usize {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                aster_mir::Instruction::TemporarySubregionEnter { .. }
            )
        })
        .count()
}

fn measure(
    source: &str,
    mode: Mode,
    expected: i32,
) -> (f64, MemoryStats, AarmMemoryTelemetry, usize) {
    let module = module(source, mode);
    let fine_regions = fine_regions(&module);
    let prepared = PreparedSequentialExecution::prepare(&module, "Main")
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
    let (_, telemetry) = prepared
        .invoke_with_aarm_telemetry()
        .expect("telemetry execution succeeds");
    (samples[SAMPLES / 2], stats, telemetry, fine_regions)
}

fn loop_append(appends: i32) -> String {
    format!(
        "public int Main() {{ string value = \"\"; int i = 0; \
         while (i < {appends}) {{ value = value + \"x\"; i = i + 1; }} \
         return value.Length; }}"
    )
}

fn builder_append(appends: i32) -> String {
    format!(
        "using aster.core; public int Main() {{ StringBuilder builder = new StringBuilder(); \
         int i = 0; while (i < {appends}) {{ builder.Append(\"x\"); i = i + 1; }} \
         return builder.ToString().Length; }}"
    )
}

fn print(
    case: &str,
    size: i32,
    timing: f64,
    memory: &MemoryStats,
    telemetry: &AarmMemoryTelemetry,
    fine_regions: usize,
) {
    println!(
        "case={case:<16} size={size:<6} median_ms={timing:>9.3} allocations={:>8} strings={:>8} requested_bytes={:>12} peak_used={:>12} capacity={:>12} temporary_peak={:>12} persistent_peak={:>12} fine_regions={fine_regions}",
        memory.total_allocations,
        memory.string_allocations,
        memory.requested_bytes,
        memory.peak_used_bytes,
        memory.peak_reserved_bytes,
        telemetry.temporary.peak_live_used_bytes,
        telemetry.persistent.peak_live_used_bytes,
    );
}

fn main() {
    for appends in [1_000, 2_000, 4_000, 20_000] {
        let source = loop_append(appends);
        for (case, mode) in [
            ("immutable", Mode::Immutable),
            ("automatic", Mode::Automatic),
        ] {
            let (timing, memory, telemetry, fine_regions) = measure(&source, mode, appends);
            print(case, appends, timing, &memory, &telemetry, fine_regions);
        }
        let source = builder_append(appends);
        let (timing, memory, telemetry, fine_regions) =
            measure(&source, Mode::ExplicitBuilder, appends);
        print(
            "explicit-builder",
            appends,
            timing,
            &memory,
            &telemetry,
            fine_regions,
        );
    }

    let appends = 100_000;
    let source = loop_append(appends);
    let (timing, memory, telemetry, fine_regions) = measure(&source, Mode::Automatic, appends);
    print(
        "automatic",
        appends,
        timing,
        &memory,
        &telemetry,
        fine_regions,
    );
    let source = builder_append(appends);
    let (timing, memory, telemetry, fine_regions) =
        measure(&source, Mode::ExplicitBuilder, appends);
    print(
        "explicit-builder",
        appends,
        timing,
        &memory,
        &telemetry,
        fine_regions,
    );
    // The immutable 100K baseline exceeds ASTER's normal 1 GiB context
    // budget; it is intentionally not run or given a larger budget.
    println!("case=immutable        size=100000 skipped=budget");
}
