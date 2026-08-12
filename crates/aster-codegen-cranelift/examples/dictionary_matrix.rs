//! Informative release-only operation curves for native `Dictionary<int, int>`.
//! No threshold is asserted; sizes increase and every result is checksummed.

use std::time::Instant;

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::compile_project;

const SAMPLES: usize = 5;

fn source(size: i32) -> String {
    format!(
        "public int Construction() {{ int i = 0; int total = 0; while (i < {size}) {{ Dictionary<int, int> values = new Dictionary<int, int>(); total += values.Length; i += 1; }} return total; }} \
         public int Insert() {{ Dictionary<int, int> values = new Dictionary<int, int>(); int i = 0; while (i < {size}) {{ values.Add(i, i); i += 1; }} return values.Length; }} \
         public int Update() {{ Dictionary<int, int> values = new Dictionary<int, int>(); int i = 0; while (i < {size}) {{ values.Add(i, i); i += 1; }} i = 0; while (i < {size}) {{ values.Set(i, i + 1); i += 1; }} return values.Length; }} \
         public int LookupHit() {{ Dictionary<int, int> values = new Dictionary<int, int>(); int i = 0; while (i < {size}) {{ values.Add(i, i); i += 1; }} int hits = 0; i = 0; while (i < {size}) {{ if (values.ContainsKey(i)) {{ hits += 1; }} i += 1; }} return hits; }} \
         public int LookupMiss() {{ Dictionary<int, int> values = new Dictionary<int, int>(); int i = 0; while (i < {size}) {{ values.Add(i, i); i += 1; }} int misses = 0; i = 0; while (i < {size}) {{ if (!values.ContainsKey(-i - 1)) {{ misses += 1; }} i += 1; }} return misses; }} \
         public int Churn() {{ Dictionary<int, int> values = new Dictionary<int, int>(); int i = 0; while (i < {size}) {{ values.Add(i, i); i += 1; }} i = 0; while (i < {size}) {{ if (!values.Remove(i)) {{ return -1; }} values.Add(i, i); i += 1; }} return values.Length; }}"
    )
}

fn compile(source: &str, size: i32) -> aster_compiler::mir::Module {
    let path = std::env::temp_dir().join(format!(
        "aster-dictionary-matrix-{}-{size}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write dictionary matrix source");
    let result = compile_project(&path).expect("dictionary matrix source compiles");
    std::fs::remove_file(path).expect("remove dictionary matrix source");
    result.compilation.mir
}

fn measure(module: &aster_compiler::mir::Module, entry: &str, expected: i32) -> (f64, MemoryStats) {
    let mut durations = Vec::with_capacity(SAMPLES);
    let mut memory = None;
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let (value, stats) = execute_with_stats(module, entry).expect("matrix executes");
        durations.push(start.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(value, ExecutionValue::Int(expected));
        memory = Some(stats);
    }
    durations.sort_by(f64::total_cmp);
    (durations[SAMPLES / 2], memory.expect("at least one sample"))
}

fn main() {
    for size in [100, 1_000, 10_000, 100_000] {
        let module = compile(&source(size), size);
        for (operation, expected) in [
            ("Construction", 0),
            ("Insert", size),
            ("Update", size),
            ("LookupHit", size),
            ("LookupMiss", size),
            ("Churn", size),
        ] {
            let (timing, memory) = measure(&module, operation, expected);
            println!(
                "operation={operation:<12} size={size:<6} median_ms={timing:>9.3} allocations={:>8} requested_bytes={:>12} peak_used_bytes={:>12}",
                memory.total_allocations, memory.requested_bytes, memory.peak_used_bytes,
            );
        }
    }
}
