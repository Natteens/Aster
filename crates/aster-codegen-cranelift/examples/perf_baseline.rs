//! Performance Baseline v1: phase-separated timings for a small workload suite.
//!
//! Measures the real production pipeline (`aster_compiler::compile`, backend
//! validation + Cranelift code generation + JIT finalization, invocation) and
//! reports deterministic invariants separately from wall-clock timings. See
//! `benchmarks/performance/README.md` for the metric contracts.

#![allow(clippy::must_use_candidate)]

use std::fmt::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aster_codegen_cranelift::{ExecutionValue, PreparedSequentialExecution};
use aster_compiler::compile;

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_WARMUP: usize = 1;
pub const DEFAULT_SAMPLES: usize = 5;
pub const MAX_COUNT: usize = 10_000;

/// Phase names in report order, with their exact boundaries and cold/warm mode.
pub const PHASES: [(&str, &str, &str); 8] = [
    (
        "frontend_compile",
        "cold",
        "One `aster_compiler::compile(source)` call: lex, parse, semantic analysis, HIR, MIR lowering, MIR optimizer.",
    ),
    (
        "backend_validate",
        "cold",
        "MIR validation, Task.Run rejection and entry selection, before any Cranelift work.",
    ),
    (
        "backend_codegen",
        "cold",
        "JIT module setup, declarations and Cranelift compilation of every MIR function, up to but excluding finalize_definitions.",
    ),
    (
        "backend_finalize",
        "cold",
        "JITModule::finalize_definitions plus entry-address lookup.",
    ),
    (
        "backend_codegen_and_finalize",
        "cold",
        "backend_codegen + backend_finalize; the two contiguous intervals of one preparation, excluding backend_validate.",
    ),
    (
        "execution_cold",
        "cold",
        "First invocation of the entry on freshly finalized code (new execution context); excludes all compilation.",
    ),
    (
        "execution_warm",
        "warm",
        "Repeated invocation of the entry on one already-prepared artifact; no compilation inside the timed interval.",
    ),
    (
        "end_to_end_cold",
        "cold",
        "One outer timer around source -> compile -> prepare -> finalize -> first invocation -> result. Measured independently; not the sum of the phases.",
    ),
];

#[derive(Clone, Copy)]
pub struct Workload {
    pub name: &'static str,
    pub category: &'static str,
    pub source: &'static str,
    pub entry: &'static str,
    pub expected: i64,
}

#[allow(clippy::too_many_lines, reason = "one literal table of workloads")]
pub fn workloads() -> Vec<Workload> {
    vec![
        Workload {
            name: "numeric_loops",
            category: "numeric",
            source: include_str!("../../../benchmarks/performance/numeric_loops.aster"),
            entry: "Run",
            expected: 7_711_710,
        },
        Workload {
            name: "strings",
            category: "strings",
            source: include_str!("../../../benchmarks/performance/strings.aster"),
            entry: "Run",
            expected: 1_600_999,
        },
        Workload {
            name: "collections",
            category: "collections",
            source: include_str!("../../../benchmarks/performance/collections.aster"),
            entry: "Run",
            expected: 89_800_000,
        },
        Workload {
            name: "calls_and_interfaces",
            category: "calls",
            source: include_str!("../../../benchmarks/performance/calls_and_interfaces.aster"),
            entry: "Run",
            expected: 300_900_000,
        },
        Workload {
            name: "generics",
            category: "generics",
            source: include_str!("../../../benchmarks/performance/generics.aster"),
            entry: "Run",
            expected: 300_333,
        },
        Workload {
            name: "allocation_mixed",
            category: "allocation",
            source: include_str!("../../../benchmarks/memory/persistent.aster"),
            entry: "Run",
            expected: 4_200_000,
        },
        Workload {
            name: "longrun_frame",
            category: "long_running",
            source: include_str!("../../../benchmarks/performance/longrun_frame.aster"),
            entry: "Run",
            expected: 16_351_259,
        },
        Workload {
            name: "longrun_request",
            category: "long_running",
            source: include_str!("../../../benchmarks/performance/longrun_request.aster"),
            entry: "Run",
            expected: 45_498_889,
        },
        Workload {
            name: "cache_churn",
            category: "long_running",
            source: include_str!("../../../benchmarks/performance/cache_churn.aster"),
            entry: "Run",
            expected: 9_703_250,
        },
        Workload {
            name: "object_graph",
            category: "objects",
            source: include_str!("../../../benchmarks/performance/object_graph.aster"),
            entry: "Run",
            expected: 184_230,
        },
        Workload {
            name: "accessor_heavy",
            category: "objects",
            source: include_str!("../../../benchmarks/performance/accessor_heavy.aster"),
            entry: "Run",
            expected: 2_508_936_383,
        },
        Workload {
            name: "interface_dispatch",
            category: "objects",
            source: include_str!("../../../benchmarks/performance/interface_dispatch.aster"),
            entry: "Run",
            expected: 7_025_000,
        },
        Workload {
            name: "collection_growth",
            category: "collections",
            source: include_str!("../../../benchmarks/performance/collection_growth.aster"),
            entry: "Run",
            expected: 56_269,
        },
        Workload {
            name: "string_pipeline",
            category: "strings",
            source: include_str!("../../../benchmarks/performance/string_pipeline.aster"),
            entry: "Run",
            expected: 403_335,
        },
        generated_frontend_workload(),
    ]
}

/// Number of generated units in the `frontend_generated` workload. Each unit is
/// one class with a constructor and a method plus one free function, so the
/// source has about 33 lines per unit.
pub const GENERATED_UNITS: usize = 400;

/// Deterministic generated program for front-end and code-generation cost.
///
/// Returns the source and its checksum. The checksum is computed here from the
/// same arithmetic the generated ASTER code performs, independently of the
/// compiler: for unit `n` with `input = n % 17`, `total = n + 6 * input`, then
/// halved when even or incremented when odd, then reduced modulo 1000.
///
/// # Panics
///
/// Panics if `units` does not fit in an `i64`.
pub fn generate_large_program(units: usize) -> (String, i64) {
    let mut source = String::new();
    let mut expected = 0_i64;
    for unit in 0..units {
        let _ = write!(
            source,
            "public class Unit{unit}\n{{\n    private int seed;\n\n    public Unit{unit}(int seed)\n    {{\n        this.seed = seed;\n    }}\n\n    public int Value(int input)\n    {{\n        int total = seed;\n        for (int step = 0; step < 3; step++)\n        {{\n            total = total + input * (step + 1);\n        }}\n        if (total % 2 == 0)\n        {{\n            total = total / 2;\n        }}\n        else\n        {{\n            total = total + 1;\n        }}\n        return total % 1000;\n    }}\n}}\n\ninternal int Call{unit}(int input)\n{{\n    Unit{unit} unit = new Unit{unit}({unit});\n    return unit.Value(input);\n}}\n\n"
        );
        let n = i64::try_from(unit).expect("unit count fits i64");
        let input = n % 17;
        let mut total = n + 6 * input;
        total = if total % 2 == 0 { total / 2 } else { total + 1 };
        expected += total % 1000;
    }
    source.push_str("public long Run()\n{\n    long total = 0L;\n");
    for unit in 0..units {
        let _ = writeln!(
            source,
            "    total = total + (long)Call{unit}({});",
            unit % 17
        );
    }
    source.push_str("    return total;\n}\n");
    (source, expected)
}

fn generated_frontend_workload() -> Workload {
    static GENERATED: std::sync::OnceLock<(&'static str, i64)> = std::sync::OnceLock::new();
    let (source, expected) = *GENERATED.get_or_init(|| {
        let (source, expected) = generate_large_program(GENERATED_UNITS);
        (Box::leak(source.into_boxed_str()), expected)
    });
    Workload {
        name: "frontend_generated",
        category: "frontend",
        source,
        entry: "Run",
        expected,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub warmup: usize,
    pub samples: usize,
    pub workloads: Vec<String>,
    pub categories: Vec<String>,
    pub json_only: bool,
}

fn parse_count(flag: &str, value: &str, minimum: usize) -> Result<usize, String> {
    let parsed: usize = value
        .parse()
        .map_err(|_| format!("{flag} expects a whole number, got `{value}`"))?;
    if parsed < minimum || parsed > MAX_COUNT {
        return Err(format!(
            "{flag} must be between {minimum} and {MAX_COUNT}, got {parsed}"
        ));
    }
    Ok(parsed)
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Parse CLI arguments (without the program name). Unknown flags, bad numbers
/// and unknown workload/category names are controlled errors.
///
/// # Errors
///
/// Returns a message describing the first invalid argument.
pub fn parse_args<I: IntoIterator<Item = String>>(arguments: I) -> Result<Config, String> {
    let mut config = Config {
        warmup: DEFAULT_WARMUP,
        samples: DEFAULT_SAMPLES,
        workloads: Vec::new(),
        categories: Vec::new(),
        json_only: false,
    };
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let (flag, inline) = match argument.split_once('=') {
            Some((flag, value)) => (flag.to_owned(), Some(value.to_owned())),
            None => (argument, None),
        };
        if flag == "--json" && inline.is_none() {
            config.json_only = true;
            continue;
        }
        if !["--warmup", "--samples", "--workload", "--category"].contains(&flag.as_str()) {
            return Err(format!("unknown argument `{flag}`"));
        }
        let value = match inline {
            Some(value) => value,
            None => arguments
                .next()
                .ok_or_else(|| format!("{flag} requires a value"))?,
        };
        match flag.as_str() {
            "--warmup" => config.warmup = parse_count(&flag, &value, 0)?,
            "--samples" => config.samples = parse_count(&flag, &value, 1)?,
            "--workload" => config.workloads.extend(split_list(&value)),
            _ => config.categories.extend(split_list(&value)),
        }
    }
    let suite = workloads();
    for name in &config.workloads {
        if !suite.iter().any(|workload| workload.name == name) {
            let known: Vec<_> = suite.iter().map(|workload| workload.name).collect();
            return Err(format!(
                "unknown workload `{name}` (known: {})",
                known.join(", ")
            ));
        }
    }
    for category in &config.categories {
        if !suite.iter().any(|workload| workload.category == category) {
            let mut known: Vec<_> = suite.iter().map(|workload| workload.category).collect();
            known.dedup();
            return Err(format!(
                "unknown category `{category}` (known: {})",
                known.join(", ")
            ));
        }
    }
    Ok(config)
}

fn selected(config: &Config) -> Vec<Workload> {
    workloads()
        .into_iter()
        .filter(|workload| {
            (config.workloads.is_empty() || config.workloads.iter().any(|n| n == workload.name))
                && (config.categories.is_empty()
                    || config.categories.iter().any(|c| c == workload.category))
        })
        .collect()
}

/// Per-phase raw samples in nanoseconds, in `PHASES` order.
#[derive(Default)]
pub struct Samples {
    pub phases: [Vec<u128>; 8],
}

pub struct BenchResult {
    pub workload: Workload,
    pub checksum: Option<i64>,
    pub mir_functions: usize,
    pub samples: Samples,
    pub error: Option<String>,
}

fn checksum_of(value: &ExecutionValue) -> Result<i64, String> {
    match value {
        ExecutionValue::Int(value) => Ok(i64::from(*value)),
        ExecutionValue::Long(value) => Ok(*value),
        other => Err(format!("entry returned non-integer value {other:?}")),
    }
}

fn verify(workload: &Workload, value: &ExecutionValue) -> Result<i64, String> {
    let checksum = checksum_of(value)?;
    if checksum == workload.expected {
        Ok(checksum)
    } else {
        Err(format!(
            "checksum {checksum} differs from expected {}",
            workload.expected
        ))
    }
}

fn nanos(duration: Duration) -> u128 {
    duration.as_nanos()
}

/// One cold run of the full pipeline under a single outer timer, with the
/// individual phases timed inside it.
fn cold_run(workload: &Workload, samples: Option<&mut Samples>) -> Result<(i64, usize), String> {
    let end_to_end = Instant::now();
    let frontend = Instant::now();
    let compilation = compile(workload.source).map_err(|diagnostics| {
        format!("frontend failed with {} diagnostic(s)", diagnostics.len())
    })?;
    let frontend = frontend.elapsed();
    let (program, timings) =
        PreparedSequentialExecution::prepare_timed(&compilation.mir, workload.entry)
            .map_err(|error| error.to_string())?;
    let execution = Instant::now();
    let value = program.invoke().map_err(|error| error.to_string())?;
    let execution = execution.elapsed();
    let end_to_end = end_to_end.elapsed();
    let checksum = verify(workload, &value)?;
    if let Some(samples) = samples {
        let values = [
            frontend,
            timings.validation,
            timings.codegen,
            timings.finalize,
            timings.codegen + timings.finalize,
            execution,
        ];
        for (slot, value) in samples.phases.iter_mut().zip(values) {
            slot.push(nanos(value));
        }
        samples.phases[7].push(nanos(end_to_end));
    }
    Ok((checksum, compilation.mir.functions.len()))
}

fn run_workload(workload: Workload, config: &Config) -> BenchResult {
    let mut samples = Samples::default();
    let mut result = BenchResult {
        workload,
        checksum: None,
        mir_functions: 0,
        samples: Samples::default(),
        error: None,
    };
    let outcome = (|| -> Result<(i64, usize), String> {
        for _ in 0..config.warmup {
            cold_run(&workload, None)?;
        }
        let mut last = (0, 0);
        for _ in 0..config.samples {
            last = cold_run(&workload, Some(&mut samples))?;
        }
        // Warm execution: compile and prepare once, then only time invocations.
        let compilation = compile(workload.source).map_err(|diagnostics| {
            format!("frontend failed: {} diagnostic(s)", diagnostics.len())
        })?;
        let (program, _) =
            PreparedSequentialExecution::prepare_timed(&compilation.mir, workload.entry)
                .map_err(|error| error.to_string())?;
        for _ in 0..config.warmup {
            verify(&workload, &program.invoke().map_err(|e| e.to_string())?)?;
        }
        for _ in 0..config.samples {
            let started = Instant::now();
            let value = program.invoke().map_err(|error| error.to_string())?;
            let elapsed = started.elapsed();
            verify(&workload, &value)?;
            samples.phases[6].push(nanos(elapsed));
        }
        Ok(last)
    })();
    match outcome {
        Ok((checksum, functions)) => {
            result.checksum = Some(checksum);
            result.mir_functions = functions;
            result.samples = samples;
        }
        Err(error) => result.error = Some(error),
    }
    result
}

pub struct Report {
    pub config: Config,
    pub started_unix_seconds: u64,
    pub results: Vec<BenchResult>,
}

impl Report {
    pub fn all_passed(&self) -> bool {
        self.results.iter().all(|result| result.error.is_none())
    }
}

pub fn run(config: &Config) -> Report {
    let started_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let results = selected(config)
        .into_iter()
        .map(|workload| run_workload(workload, config))
        .collect();
    Report {
        config: config.clone(),
        started_unix_seconds,
        results,
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_optional(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_owned(), json_string)
}

fn rustc_version() -> Option<String> {
    let output = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn median(sorted: &[u128]) -> u128 {
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        sorted[middle - 1].midpoint(sorted[middle])
    }
}

fn json_timing(mode: &str, raw: &[u128]) -> String {
    let mut sorted = raw.to_vec();
    sorted.sort_unstable();
    let total: u128 = raw.iter().sum();
    let list: Vec<String> = raw.iter().map(u128::to_string).collect();
    format!(
        "{{\"mode\":{},\"unit\":\"ns\",\"count\":{},\"min\":{},\"median\":{},\"max\":{},\"mean\":{},\"samples\":[{}]}}",
        json_string(mode),
        raw.len(),
        sorted[0],
        median(&sorted),
        sorted[sorted.len() - 1],
        total / raw.len() as u128,
        list.join(",")
    )
}

fn json_result(result: &BenchResult) -> String {
    let workload = &result.workload;
    let status = if result.error.is_none() {
        "pass"
    } else {
        "fail"
    };
    let mut out = format!(
        "{{\"name\":{},\"category\":{},\"status\":{},\"error\":{},\"invariants\":{{\"entry\":{},\"expected_checksum\":{},\"checksum\":{},\"mir_function_count\":{}}},\"timings\":{{",
        json_string(workload.name),
        json_string(workload.category),
        json_string(status),
        json_optional(result.error.as_deref()),
        json_string(workload.entry),
        workload.expected,
        result
            .checksum
            .map_or_else(|| "null".to_owned(), |value| value.to_string()),
        result.mir_functions,
    );
    if result.error.is_none() {
        let entries: Vec<String> = PHASES
            .iter()
            .zip(&result.samples.phases)
            .map(|((name, mode, _), raw)| {
                format!("{}:{}", json_string(name), json_timing(mode, raw))
            })
            .collect();
        out.push_str(&entries.join(","));
    }
    out.push_str("}}");
    out
}

pub fn serialize_report(report: &Report) -> String {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let git_revision = std::env::var("ASTER_GIT_REVISION")
        .ok()
        .filter(|value| !value.is_empty());
    let contracts: Vec<String> = PHASES
        .iter()
        .map(|(name, mode, boundary)| {
            format!(
                "{}:{{\"mode\":{},\"boundary\":{}}}",
                json_string(name),
                json_string(mode),
                json_string(boundary)
            )
        })
        .collect();
    let benchmarks: Vec<String> = report.results.iter().map(json_result).collect();
    format!(
        "{{\"schema_version\":{SCHEMA_VERSION},\"kind\":\"aster-performance-baseline\",\"metadata\":{{\"aster_version\":{},\"git_revision\":{},\"os\":{},\"arch\":{},\"target\":{},\"profile\":{},\"rustc_version\":{},\"timing_unit\":\"ns\",\"warmup\":{},\"samples\":{},\"phases\":{{{}}}}},\"run\":{{\"started_unix_seconds\":{}}},\"benchmarks\":[{}]}}",
        json_string(env!("CARGO_PKG_VERSION")),
        json_optional(git_revision.as_deref()),
        json_string(std::env::consts::OS),
        json_string(std::env::consts::ARCH),
        json_string(&format!(
            "{}-{}",
            std::env::consts::ARCH,
            std::env::consts::OS
        )),
        json_string(profile),
        json_optional(rustc_version().as_deref()),
        report.config.warmup,
        report.config.samples,
        contracts.join(","),
        report.started_unix_seconds,
        benchmarks.join(","),
    )
}

#[allow(clippy::cast_precision_loss)]
fn ms(nanoseconds: u128) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}

fn phase_median(result: &BenchResult, index: usize) -> f64 {
    let mut sorted = result.samples.phases[index].clone();
    sorted.sort_unstable();
    ms(median(&sorted))
}

fn print_human_summary(report: &Report) {
    println!(
        "ASTER Performance Baseline v{SCHEMA_VERSION} ({}, warmup {}, samples {}); median ms, comparable only on the same machine",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        report.config.warmup,
        report.config.samples
    );
    println!(
        "{:<22} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "workload", "frontend", "codegen", "finalize", "exec cold", "exec warm", "e2e cold"
    );
    for result in &report.results {
        if let Some(error) = &result.error {
            println!("{:<22} FAIL: {error}", result.workload.name);
            continue;
        }
        println!(
            "{:<22} {:>10.3} {:>10.3} {:>10.3} {:>10.3} {:>10.3} {:>10.3}",
            result.workload.name,
            phase_median(result, 0),
            phase_median(result, 2),
            phase_median(result, 3),
            phase_median(result, 5),
            phase_median(result, 6),
            phase_median(result, 7),
        );
    }
}

fn main() {
    let config = match parse_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("perf_baseline: {error}");
            eprintln!(
                "usage: perf_baseline [--json] [--warmup N] [--samples N] [--workload a,b] [--category c]"
            );
            std::process::exit(2);
        }
    };
    let report = run(&config);
    if config.json_only {
        println!("{}", serialize_report(&report));
    } else {
        print_human_summary(&report);
        println!();
        println!("{}", serialize_report(&report));
    }
    if !report.all_passed() {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::json_string;

    #[test]
    fn json_string_escapes_quotes_backslashes_and_whitespace() {
        assert_eq!(json_string("a\"b"), r#""a\"b""#);
        assert_eq!(json_string(r"C:\dir\file"), "\"C:\\\\dir\\\\file\"");
        assert_eq!(json_string("l1\nl2\r\tend"), r#""l1\nl2\r\tend""#);
    }

    #[test]
    fn json_string_escapes_every_control_character_below_0x20() {
        for code in 0u8..0x20 {
            let escaped = json_string(&char::from(code).to_string());
            assert!(
                escaped.chars().all(|c| (c as u32) >= 0x20),
                "raw control character {code:#x} leaked into {escaped:?}"
            );
            let expected = match code {
                b'\n' => r#""\n""#.to_owned(),
                b'\r' => r#""\r""#.to_owned(),
                b'\t' => r#""\t""#.to_owned(),
                _ => format!("\"\\u{code:04x}\""),
            };
            assert_eq!(escaped, expected);
        }
    }

    #[test]
    fn json_string_passes_unicode_through_unchanged() {
        assert_eq!(json_string("ünï ✓ 😀"), "\"ünï ✓ 😀\"");
    }
}
