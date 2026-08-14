//! Release-only direct-versus-helper collection ownership matrix for issue #45.
//!
//! Each pair performs the same insertions and returns the same logical length.
//! JIT preparation is outside the timed interval; measurements are informative
//! and intentionally have no performance assertions.

use std::time::Instant;

use aster_codegen_cranelift::{
    AarmMemoryTelemetry, ExecutionValue, MemoryStats, PreparedSequentialExecution,
};
use aster_compiler::compile;

const SAMPLES: usize = 5;

#[derive(Clone, Copy)]
enum Collection {
    List,
    Dictionary,
}

impl Collection {
    const fn name(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Dictionary => "dictionary",
        }
    }

    fn source(self, count: usize, helper: bool) -> String {
        let count = i32::try_from(count).expect("matrix count fits ASTER int");
        match (self, helper) {
            (Self::List, false) => format!(
                "public int Run() {{ List<int> values = new List<int>(); int i = 0; \
                 while (i < {count}) {{ values.Add(i); i = i + 1; }} return values.Length; }}"
            ),
            (Self::List, true) => format!(
                "public void Grow(List<int> values, int count) {{ int i = 0; \
                 while (i < count) {{ values.Add(i); i = i + 1; }} }} \
                 public int Run() {{ List<int> values = new List<int>(); Grow(values, {count}); \
                 return values.Length; }}"
            ),
            (Self::Dictionary, false) => format!(
                "public int Run() {{ Dictionary<int, int> values = new Dictionary<int, int>(); \
                 int i = 0; while (i < {count}) {{ values.Add(i, i + 1); i = i + 1; }} \
                 return values.Length; }}"
            ),
            (Self::Dictionary, true) => format!(
                "public void Grow(Dictionary<int, int> values, int count) {{ int i = 0; \
                 while (i < count) {{ values.Add(i, i + 1); i = i + 1; }} }} \
                 public int Run() {{ Dictionary<int, int> values = new Dictionary<int, int>(); \
                 Grow(values, {count}); return values.Length; }}"
            ),
        }
    }
}

fn median_execution_ms(prepared: &PreparedSequentialExecution, expected: &ExecutionValue) -> f64 {
    assert_eq!(prepared.invoke().expect("warmup executes"), *expected);
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        assert_eq!(
            prepared.invoke().expect("measured execution executes"),
            *expected
        );
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    samples.sort_by(f64::total_cmp);
    samples[SAMPLES / 2]
}

fn measure(collection: Collection, count: usize, helper: bool) {
    let module = compile(&collection.source(count, helper))
        .expect("direct/helper collection matrix source compiles")
        .mir;
    let expected = ExecutionValue::Int(i32::try_from(count).expect("count fits result"));
    let prepared = PreparedSequentialExecution::prepare(&module, "Run")
        .expect("direct/helper collection matrix JIT prepares");
    let median_ms = median_execution_ms(&prepared, &expected);
    let (value, stats): (ExecutionValue, MemoryStats) = prepared
        .invoke_with_stats()
        .expect("stats invocation executes");
    assert_eq!(value, expected);
    let (value, telemetry): (ExecutionValue, AarmMemoryTelemetry) = prepared
        .invoke_with_aarm_telemetry()
        .expect("telemetry invocation executes");
    assert_eq!(value, expected);

    println!(
        "collection={} path={} count={count} median_ms={median_ms:.3} checksum={count} allocations={} requested_bytes={} used_bytes={} peak_used_bytes={} reserved_bytes={} temporary_used={} temporary_peak={} temporary_capacity={} persistent_used={} persistent_peak={} persistent_capacity={}",
        collection.name(),
        if helper { "helper" } else { "direct" },
        stats.total_allocations,
        stats.requested_bytes,
        stats.used_bytes,
        stats.peak_used_bytes,
        stats.reserved_bytes,
        telemetry.temporary.live_used_bytes,
        telemetry.temporary.peak_live_used_bytes,
        telemetry.temporary.arena_capacity_bytes,
        telemetry.persistent.live_used_bytes,
        telemetry.persistent.peak_live_used_bytes,
        telemetry.persistent.arena_capacity_bytes,
    );
}

fn main() {
    for count in [1_000, 10_000, 50_000, 100_000] {
        for collection in [Collection::List, Collection::Dictionary] {
            measure(collection, count, false);
            measure(collection, count, true);
        }
    }
}
