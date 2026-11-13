//! MEM-EXP-1 C5/C6 count report: checksums, static and executed count
//! operations, and heap block facts for the performance workloads.
//!
//! Build once with `--features mem-exp-c5` and once with `mem-exp-c6`; the
//! executed counts reflect the configuration the binary was built with, the
//! static counts are computed for both.

use aster_codegen_cranelift::{ExecutionValue, PreparedSequentialExecution};
use aster_compiler::{compile, mir};

const WORKLOADS: [(&str, &str); 13] = [
    (
        "accessor_heavy",
        include_str!("../../../benchmarks/performance/accessor_heavy.aster"),
    ),
    (
        "collections",
        include_str!("../../../benchmarks/performance/collections.aster"),
    ),
    (
        "cache_churn",
        include_str!("../../../benchmarks/performance/cache_churn.aster"),
    ),
    (
        "calls_and_interfaces",
        include_str!("../../../benchmarks/performance/calls_and_interfaces.aster"),
    ),
    (
        "collection_growth",
        include_str!("../../../benchmarks/performance/collection_growth.aster"),
    ),
    (
        "generics",
        include_str!("../../../benchmarks/performance/generics.aster"),
    ),
    (
        "interface_dispatch",
        include_str!("../../../benchmarks/performance/interface_dispatch.aster"),
    ),
    (
        "longrun_frame",
        include_str!("../../../benchmarks/performance/longrun_frame.aster"),
    ),
    (
        "longrun_request",
        include_str!("../../../benchmarks/performance/longrun_request.aster"),
    ),
    (
        "numeric_loops",
        include_str!("../../../benchmarks/performance/numeric_loops.aster"),
    ),
    (
        "object_graph",
        include_str!("../../../benchmarks/performance/object_graph.aster"),
    ),
    (
        "string_pipeline",
        include_str!("../../../benchmarks/performance/string_pipeline.aster"),
    ),
    (
        "strings",
        include_str!("../../../benchmarks/performance/strings.aster"),
    ),
];

fn main() {
    let configuration = if cfg!(feature = "mem-exp-elide") {
        "C6"
    } else {
        "C5"
    };
    println!(
        "workload\tentry\tchecksum\tstatic_c5\tstatic_c6\texecuted_retains\texecuted_releases\tfresh\treused\treleased\tused\tpeak\t({configuration})"
    );
    for (name, source) in WORKLOADS {
        let compilation = compile(source).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let (_, c5) = mir::ownership::insert_ownership(&compilation.mir);
        let (_, c6) = mir::ownership::insert_ownership_elided(&compilation.mir);
        let entries: &[&str] = if source.contains("RunLong()") {
            &["Run", "RunLong"]
        } else {
            &["Run"]
        };
        for entry in entries {
            let program = PreparedSequentialExecution::prepare(&compilation.mir, entry)
                .unwrap_or_else(|error| panic!("{name} {entry}: {error}"));
            let (value, stats) = program
                .invoke_with_stats()
                .unwrap_or_else(|error| panic!("{name} {entry}: {error}"));
            let checksum = match value {
                ExecutionValue::Long(value) => value,
                ExecutionValue::Int(value) => i64::from(value),
                other => panic!("{name} {entry}: unexpected result {other:?}"),
            };
            let heap = stats.object_heap;
            println!(
                "{name}\t{entry}\t{checksum}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                c5.retains + c5.releases,
                c6.retains + c6.releases,
                heap.retains,
                heap.releases,
                heap.fresh_blocks,
                heap.reused_blocks,
                heap.released_blocks,
                stats.used_bytes,
                stats.peak_used_bytes,
            );
        }
    }
}
