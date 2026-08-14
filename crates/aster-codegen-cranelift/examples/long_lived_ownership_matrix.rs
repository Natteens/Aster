#![cfg_attr(not(feature = "aarm-telemetry"), allow(dead_code, unused_imports))]

//! Release-only before/after evidence for compiler-proven long-lived ownership.
//! Timings are informational and never asserted.

use std::time::{Duration, Instant};

#[cfg(feature = "aarm-telemetry")]
use aster_codegen_cranelift::PreparedSequentialExecution;
use aster_codegen_cranelift::{AarmMemoryTelemetry, ExecutionValue, MemoryStats};
use aster_compiler::compile;

const SAMPLES: usize = 5;

const ARRAY_TEMPORARY: &str = r"
    internal int BuildAndUse() {
        int[] values = new int[500000];
        for (int i = 0; i < values.Length; i++) { values[i] = i; }
        return values[0] + values[499999] + 1;
    }
    public int Run() {
        int total = 0;
        for (int i = 0; i < 100; i++) { total += BuildAndUse(); }
        return total;
    }
";

const ARRAY_RETURNED_TEMPLATE: &str = r"
    internal int[] Make() {
        int[] values = new int[500000];
        for (int i = 0; i < values.Length; i++) { values[i] = i; }
        return values;
    }
    public int Run() {
        int total = 0;
        int[] current = [0];
        for (int i = 0; i < __ITERATIONS__; i++) {
            current = Make();
            total += current[0] + current[499999] + 1;
        }
        return total;
    }
";

const OBJECT_RETURNED_TEMPLATE: &str = r"
    public class Box {
        public int Value;
        public Box(int value) { Value = value; }
    }
    internal Box Make(int value) { return new Box(value); }
    public int Run() {
        int total = 0;
        for (int i = 0; i < __ITERATIONS__; i++) {
            Box box = Make(i);
            total += box.Value % 1000;
        }
        return total;
    }
";

const STRING_RETURNED: &str = r#"
    internal string Make(int value) { return $"value{value}"; }
    public int Run() {
        int total = 0;
        for (int i = 0; i < 10000; i++) { string text = Make(i); total += text.Length; }
        return total;
    }
"#;

const LIST_RETURNED: &str = r"
    internal List<int> Make(int value) {
        List<int> values = new List<int>();
        values.Add(value);
        values.Add(value + 1);
        return values;
    }
    public int Run() {
        int total = 0;
        for (int i = 0; i < 10000; i++) { List<int> values = Make(i); total += values.Length; }
        return total;
    }
";

const DICTIONARY_RETURNED: &str = r"
    internal Dictionary<int, int> Make(int value) {
        Dictionary<int, int> values = new Dictionary<int, int>();
        values.Add(1, value);
        return values;
    }
    public int Run() {
        int total = 0;
        for (int i = 0; i < 10000; i++) {
            Dictionary<int, int> values = Make(i);
            total += values.Length;
        }
        return total;
    }
";

struct Case {
    name: String,
    source: String,
    expected: i32,
}

struct Measurement {
    median: Duration,
    stats: MemoryStats,
    telemetry: AarmMemoryTelemetry,
}

#[cfg(not(feature = "aarm-telemetry"))]
fn main() {
    eprintln!("long_lived_ownership_matrix requires --features aarm-telemetry");
    std::process::exit(2);
}

#[cfg(feature = "aarm-telemetry")]
fn main() {
    let mut cases = vec![Case {
        name: "array-temporary".to_owned(),
        source: ARRAY_TEMPORARY.to_owned(),
        expected: 50_000_000,
    }];
    for iterations in [1, 10, 100, 1000] {
        cases.push(Case {
            name: format!("array-returned-{iterations}"),
            source: ARRAY_RETURNED_TEMPLATE.replace("__ITERATIONS__", &iterations.to_string()),
            expected: 500_000 * iterations,
        });
    }
    for iterations in [10_000, 100_000, 1_000_000] {
        cases.push(Case {
            name: format!("object-returned-{iterations}"),
            source: OBJECT_RETURNED_TEMPLATE.replace("__ITERATIONS__", &iterations.to_string()),
            expected: (iterations / 1000) * 499_500,
        });
    }
    cases.extend([
        Case {
            name: "string-returned".to_owned(),
            source: STRING_RETURNED.to_owned(),
            expected: 88_890,
        },
        Case {
            name: "list-returned".to_owned(),
            source: LIST_RETURNED.to_owned(),
            expected: 20_000,
        },
        Case {
            name: "dictionary-returned".to_owned(),
            source: DICTIONARY_RETURNED.to_owned(),
            expected: 10_000,
        },
    ]);

    println!(
        "{:<20} {:>10} {:>8} {:>12} {:>12} {:>12} {:>12} {:>12}",
        "case", "median_ms", "allocs", "requested", "used", "peak_used", "capacity", "persistent"
    );
    for case in cases {
        let measurement = measure(&case);
        println!(
            "{:<20} {:>10.3} {:>8} {:>12} {:>12} {:>12} {:>12} {:>12}",
            case.name,
            measurement.median.as_secs_f64() * 1_000.0,
            measurement.stats.total_allocations,
            measurement.stats.requested_bytes,
            measurement.stats.used_bytes,
            measurement.stats.peak_used_bytes,
            measurement.stats.reserved_bytes,
            measurement.telemetry.persistent.live_used_bytes,
        );
    }
}

#[cfg(feature = "aarm-telemetry")]
fn measure(case: &Case) -> Measurement {
    let compilation = compile(&case.source).expect("ownership matrix source compiles");
    let prepared = PreparedSequentialExecution::prepare(&compilation.mir, "Run")
        .expect("ownership matrix prepares");
    let (value, stats) = prepared
        .invoke_with_stats()
        .expect("ownership matrix executes");
    assert_eq!(value, ExecutionValue::Int(case.expected));
    let (telemetry_value, telemetry) = prepared
        .invoke_with_aarm_telemetry()
        .expect("ownership matrix telemetry executes");
    assert_eq!(telemetry_value, value);

    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let sample = prepared.invoke().expect("ownership matrix sample executes");
        samples.push(start.elapsed());
        assert_eq!(sample, value);
    }
    samples.sort_unstable();

    Measurement {
        median: samples[SAMPLES / 2],
        stats,
        telemetry,
    }
}
