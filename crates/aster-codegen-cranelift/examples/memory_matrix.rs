#![allow(clippy::must_use_candidate)]

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::compile;
use aster_mir as mir;

const SAMPLES: usize = 5;
const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Temporary,
    Persistent,
}

impl Region {
    pub fn as_str(self) -> &'static str {
        match self {
            Region::Temporary => "temporary",
            Region::Persistent => "persistent",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    Small,
    Medium,
    Large,
}

impl Scale {
    pub fn as_str(self) -> &'static str {
        match self {
            Scale::Small => "small",
            Scale::Medium => "medium",
            Scale::Large => "large",
        }
    }

    fn entry_function(self) -> &'static str {
        match self {
            Scale::Small => "RunSmall",
            Scale::Medium => "RunMedium",
            Scale::Large => "RunLarge",
        }
    }

    fn parse(token: &str) -> Option<Scale> {
        match token {
            "small" => Some(Scale::Small),
            "medium" => Some(Scale::Medium),
            "large" => Some(Scale::Large),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
        }
    }
}

pub struct Workload {
    pub case: &'static str,
    pub region: Region,
    pub source: &'static str,
    pub per_iteration_checksum: i64,
    pub objects_per_iteration: u64,
    pub arrays_per_iteration: u64,
    pub strings_per_iteration: u64,
}

impl Workload {
    fn iterations(&self, scale: Scale) -> u64 {
        match (self.region, scale) {
            (_, Scale::Small) => 10_000,
            (_, Scale::Medium) => 100_000,
            (Region::Temporary, Scale::Large) => 500_000,
            (Region::Persistent, Scale::Large) => 150_000,
        }
    }
}

pub fn workloads() -> Vec<Workload> {
    vec![
        Workload {
            case: "object",
            region: Region::Temporary,
            source: include_str!("../../../benchmarks/memory/object_temporary.aster"),
            per_iteration_checksum: 39,
            objects_per_iteration: 1,
            arrays_per_iteration: 0,
            strings_per_iteration: 0,
        },
        Workload {
            case: "object",
            region: Region::Persistent,
            source: include_str!("../../../benchmarks/memory/object_persistent.aster"),
            per_iteration_checksum: 39,
            objects_per_iteration: 1,
            arrays_per_iteration: 0,
            strings_per_iteration: 0,
        },
        Workload {
            case: "array",
            region: Region::Temporary,
            source: include_str!("../../../benchmarks/memory/array_temporary.aster"),
            per_iteration_checksum: 7,
            objects_per_iteration: 0,
            arrays_per_iteration: 1,
            strings_per_iteration: 0,
        },
        Workload {
            case: "array",
            region: Region::Persistent,
            source: include_str!("../../../benchmarks/memory/array_persistent.aster"),
            per_iteration_checksum: 7,
            objects_per_iteration: 0,
            arrays_per_iteration: 1,
            strings_per_iteration: 0,
        },
        Workload {
            case: "string",
            region: Region::Temporary,
            source: include_str!("../../../benchmarks/memory/string_temporary.aster"),
            per_iteration_checksum: 2,
            objects_per_iteration: 0,
            arrays_per_iteration: 0,
            strings_per_iteration: 1,
        },
        Workload {
            case: "string",
            region: Region::Persistent,
            source: include_str!("../../../benchmarks/memory/string_persistent.aster"),
            per_iteration_checksum: 2,
            objects_per_iteration: 0,
            arrays_per_iteration: 0,
            strings_per_iteration: 1,
        },
        Workload {
            case: "mixed",
            region: Region::Temporary,
            source: include_str!("../../../benchmarks/memory/temporary.aster"),
            per_iteration_checksum: 42,
            objects_per_iteration: 1,
            arrays_per_iteration: 1,
            strings_per_iteration: 1,
        },
        Workload {
            case: "mixed",
            region: Region::Persistent,
            source: include_str!("../../../benchmarks/memory/persistent.aster"),
            per_iteration_checksum: 42,
            objects_per_iteration: 1,
            arrays_per_iteration: 1,
            strings_per_iteration: 1,
        },
    ]
}

#[derive(Clone, Copy, Debug)]
pub struct TimingStat {
    pub median: f64,
    pub min: f64,
    pub max: f64,
}

impl TimingStat {
    fn constant(value_ms: f64) -> TimingStat {
        TimingStat {
            median: value_ms,
            min: value_ms,
            max: value_ms,
        }
    }

    fn offset(self, delta_ms: f64) -> TimingStat {
        TimingStat {
            median: self.median + delta_ms,
            min: self.min + delta_ms,
            max: self.max + delta_ms,
        }
    }
}

pub struct CaseResult {
    pub case: &'static str,
    pub region: Region,
    pub scale: Scale,
    pub iterations: u64,
    pub status: Status,
    pub checksum: Option<i64>,
    pub expected_checksum: i64,
    pub samples: usize,
    pub memory: Option<MemoryStats>,
    pub frontend_compile: Option<TimingStat>,
    pub jit_and_execute: Option<TimingStat>,
    pub end_to_end: Option<TimingStat>,
    pub error: Option<String>,
}

pub struct Environment {
    pub aster_version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub target: String,
    pub profile: &'static str,
    pub git_revision: String,
}

impl Environment {
    fn detect() -> Environment {
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        let git_revision = std::env::var("ASTER_GIT_REVISION")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| String::from("unknown"));

        Environment {
            aster_version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            profile,
            git_revision,
        }
    }
}

pub struct Report {
    pub schema_version: u32,
    pub environment: Environment,
    pub results: Vec<CaseResult>,
}

impl Report {
    pub fn all_passed(&self) -> bool {
        self.results
            .iter()
            .all(|result| result.status == Status::Pass)
    }
}

fn allocation_regions(module: &mir::Module) -> Vec<mir::AllocationRegion> {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            mir::Instruction::AllocateObject { region, .. }
            | mir::Instruction::AllocateArray { region, .. } => Some(*region),
            mir::Instruction::CallIntrinsic { intrinsic, .. } => {
                intrinsic.string_allocation_region()
            }
            _ => None,
        })
        .collect()
}

fn expected_region(region: Region) -> mir::AllocationRegion {
    match region {
        Region::Temporary => mir::AllocationRegion::Temporary,
        Region::Persistent => mir::AllocationRegion::Persistent,
    }
}

fn verify_regions(module: &mir::Module, region: Region) -> Result<(), String> {
    let expected = expected_region(region);
    let observed = allocation_regions(module);
    if observed.is_empty() {
        return Err(String::from("workload produced no dynamic allocations"));
    }
    if !observed.contains(&expected) {
        return Err(format!(
            "expected at least one allocation in region {expected:?}, observed {observed:?}"
        ));
    }
    Ok(())
}

fn verify_counts(workload: &Workload, iterations: u64, stats: &MemoryStats) -> Result<(), String> {
    let expected_objects = workload.objects_per_iteration * iterations;
    let expected_arrays = workload.arrays_per_iteration * iterations;
    let expected_strings = workload.strings_per_iteration * iterations;
    let expected_total = expected_objects + expected_arrays + expected_strings;

    if stats.total_allocations != expected_total {
        return Err(format!(
            "total allocations {} differ from expected {expected_total}",
            stats.total_allocations
        ));
    }
    if stats.object_allocations != expected_objects {
        return Err(format!(
            "object allocations {} differ from expected {expected_objects}",
            stats.object_allocations
        ));
    }
    if stats.array_allocations != expected_arrays {
        return Err(format!(
            "array allocations {} differ from expected {expected_arrays}",
            stats.array_allocations
        ));
    }
    if stats.string_allocations != expected_strings {
        return Err(format!(
            "string allocations {} differ from expected {expected_strings}",
            stats.string_allocations
        ));
    }
    Ok(())
}

fn verify_region_invariants(region: Region, stats: &MemoryStats) -> Result<(), String> {
    if stats.peak_used_bytes == 0 {
        return Err(String::from("peak used bytes never rose above zero"));
    }
    match region {
        Region::Temporary => {
            if stats.used_bytes != 0 {
                return Err(format!(
                    "temporary workload retained {} used bytes",
                    stats.used_bytes
                ));
            }
        }
        Region::Persistent => {
            if stats.used_bytes == 0 {
                return Err(String::from(
                    "persistent workload unexpectedly retained zero used bytes",
                ));
            }
            if stats.peak_used_bytes < stats.used_bytes {
                return Err(format!(
                    "peak used {} fell below final used {}",
                    stats.peak_used_bytes, stats.used_bytes
                ));
            }
        }
    }
    Ok(())
}

fn checksum_value(value: &ExecutionValue) -> Result<i64, String> {
    match value {
        ExecutionValue::Int(checksum) => Ok(i64::from(*checksum)),
        other => Err(format!("workload returned non-integer checksum {other:?}")),
    }
}

fn timing_stat(durations: &[Duration]) -> TimingStat {
    let mut sorted = durations.to_vec();
    sorted.sort_unstable();
    let to_ms = |duration: Duration| duration.as_secs_f64() * 1_000.0;
    TimingStat {
        median: to_ms(sorted[sorted.len() / 2]),
        min: to_ms(sorted[0]),
        max: to_ms(sorted[sorted.len() - 1]),
    }
}

fn failed_case(
    workload: &Workload,
    scale: Scale,
    frontend_compile: Option<TimingStat>,
    checksum: Option<i64>,
    memory: Option<MemoryStats>,
    error: String,
) -> CaseResult {
    let iterations = workload.iterations(scale);
    let expected_checksum =
        workload.per_iteration_checksum * i64::try_from(iterations).unwrap_or(0);
    CaseResult {
        case: workload.case,
        region: workload.region,
        scale,
        iterations,
        status: Status::Fail,
        checksum,
        expected_checksum,
        samples: 0,
        memory,
        frontend_compile,
        jit_and_execute: None,
        end_to_end: None,
        error: Some(error),
    }
}

fn run_case(
    workload: &Workload,
    module: &mir::Module,
    frontend_compile: TimingStat,
    scale: Scale,
) -> CaseResult {
    let iterations = workload.iterations(scale);
    let expected_checksum =
        workload.per_iteration_checksum * i64::try_from(iterations).unwrap_or(0);
    let entry = scale.entry_function();

    let make_failure =
        |checksum: Option<i64>, memory: Option<MemoryStats>, error: String| -> CaseResult {
            failed_case(
                workload,
                scale,
                Some(frontend_compile),
                checksum,
                memory,
                error,
            )
        };

    let (warmup_value, warmup_stats) = match execute_with_stats(module, entry) {
        Ok(output) => output,
        Err(error) => return make_failure(None, None, error.to_string()),
    };

    let checksum = match checksum_value(&warmup_value) {
        Ok(checksum) => checksum,
        Err(error) => return make_failure(None, Some(warmup_stats), error),
    };
    if checksum != expected_checksum {
        return make_failure(
            Some(checksum),
            Some(warmup_stats),
            format!("checksum {checksum} differs from expected {expected_checksum}"),
        );
    }
    if let Err(error) = verify_counts(workload, iterations, &warmup_stats) {
        return make_failure(Some(checksum), Some(warmup_stats), error);
    }
    if let Err(error) = verify_region_invariants(workload.region, &warmup_stats) {
        return make_failure(Some(checksum), Some(warmup_stats), error);
    }

    let mut durations = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        let sample = execute_with_stats(module, entry);
        let elapsed = started.elapsed();
        let (value, stats) = match sample {
            Ok(output) => output,
            Err(error) => {
                return make_failure(Some(checksum), Some(warmup_stats), error.to_string());
            }
        };
        durations.push(elapsed);

        let sample_checksum = match checksum_value(&value) {
            Ok(sample_checksum) => sample_checksum,
            Err(error) => return make_failure(Some(checksum), Some(warmup_stats), error),
        };
        if sample_checksum != expected_checksum {
            return make_failure(
                Some(sample_checksum),
                Some(warmup_stats),
                format!(
                    "sample checksum {sample_checksum} differs from expected {expected_checksum}"
                ),
            );
        }
        if stats != warmup_stats {
            return make_failure(
                Some(checksum),
                Some(warmup_stats.clone()),
                format!(
                    "deterministic memory stats changed between executions: {warmup_stats:?} versus {stats:?}"
                ),
            );
        }
    }

    let jit_and_execute = timing_stat(&durations);
    let end_to_end = jit_and_execute.offset(frontend_compile.median);

    CaseResult {
        case: workload.case,
        region: workload.region,
        scale,
        iterations,
        status: Status::Pass,
        checksum: Some(checksum),
        expected_checksum,
        samples: SAMPLES,
        memory: Some(warmup_stats),
        frontend_compile: Some(frontend_compile),
        jit_and_execute: Some(jit_and_execute),
        end_to_end: Some(end_to_end),
        error: None,
    }
}

fn run_workload(workload: &Workload, scales: &[Scale]) -> Vec<CaseResult> {
    let started = Instant::now();
    let compilation = compile(workload.source);
    let compile_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let frontend_compile = TimingStat::constant(compile_ms);

    let compilation = match compilation {
        Ok(compilation) => compilation,
        Err(diagnostics) => {
            let message = format!("compilation failed: {diagnostics:#?}");
            return scales
                .iter()
                .map(|scale| {
                    failed_case(
                        workload,
                        *scale,
                        Some(frontend_compile),
                        None,
                        None,
                        message.clone(),
                    )
                })
                .collect();
        }
    };

    if let Err(error) = verify_regions(&compilation.mir, workload.region) {
        return scales
            .iter()
            .map(|scale| {
                failed_case(
                    workload,
                    *scale,
                    Some(frontend_compile),
                    None,
                    None,
                    error.clone(),
                )
            })
            .collect();
    }

    scales
        .iter()
        .map(|scale| run_case(workload, &compilation.mir, frontend_compile, *scale))
        .collect()
}

pub fn run_matrix(scales: &[Scale]) -> Report {
    let mut results = Vec::new();
    for workload in workloads() {
        results.extend(run_workload(&workload, scales));
    }
    Report {
        schema_version: SCHEMA_VERSION,
        environment: Environment::detect(),
        results,
    }
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            control if u32::from(control) < 0x20 => {
                write!(escaped, "\\u{:04x}", u32::from(control))
                    .expect("writing to a String cannot fail");
            }
            other => escaped.push(other),
        }
    }
    escaped
}

pub fn json_string(value: &str) -> String {
    format!("\"{}\"", json_escape(value))
}

fn json_optional_i64(value: Option<i64>) -> String {
    match value {
        Some(number) => number.to_string(),
        None => String::from("null"),
    }
}

fn json_timing(stat: Option<TimingStat>) -> String {
    match stat {
        Some(stat) => format!(
            "{{\"median\": {:.6}, \"min\": {:.6}, \"max\": {:.6}}}",
            stat.median, stat.min, stat.max
        ),
        None => String::from("null"),
    }
}

fn json_memory(memory: Option<&MemoryStats>) -> String {
    match memory {
        Some(stats) => {
            format!(
                "{{\"total_allocations\": {}, \"object_allocations\": {}, \
                 \"array_allocations\": {}, \"string_allocations\": {}, \
                 \"requested_bytes\": {}, \"used_bytes\": {}, \"reserved_bytes\": {}, \
                 \"peak_used_bytes\": {}, \"peak_reserved_bytes\": {}}}",
                stats.total_allocations,
                stats.object_allocations,
                stats.array_allocations,
                stats.string_allocations,
                stats.requested_bytes,
                stats.used_bytes,
                stats.reserved_bytes,
                stats.peak_used_bytes,
                stats.peak_reserved_bytes
            )
        }
        None => String::from("null"),
    }
}

fn json_result(result: &CaseResult) -> String {
    let error = match &result.error {
        Some(message) => json_string(message),
        None => String::from("null"),
    };

    format!(
        "    {{\n\
         \"case\": {}, \"region\": {}, \"scale\": {}, \"iterations\": {}, \"status\": {},\n\
         \"checksum\": {}, \"expected_checksum\": {}, \"samples\": {},\n\
         \"memory\": {},\n\
         \"timing_ms\": {{\"frontend_compile\": {}, \"jit_and_execute\": {}, \"end_to_end\": {}}},\n\
         \"error\": {}\n\
         }}",
        json_string(result.case),
        json_string(result.region.as_str()),
        json_string(result.scale.as_str()),
        result.iterations,
        json_string(result.status.as_str()),
        json_optional_i64(result.checksum),
        result.expected_checksum,
        result.samples,
        json_memory(result.memory.as_ref()),
        json_timing(result.frontend_compile),
        json_timing(result.jit_and_execute),
        json_timing(result.end_to_end),
        error,
    )
}

pub fn serialize_report(report: &Report) -> String {
    let environment = format!(
        "  \"environment\": {{\"aster_version\": {}, \"os\": {}, \"arch\": {}, \
         \"target\": {}, \"profile\": {}, \"git_revision\": {}}}",
        json_string(report.environment.aster_version),
        json_string(report.environment.os),
        json_string(report.environment.arch),
        json_string(&report.environment.target),
        json_string(report.environment.profile),
        json_string(&report.environment.git_revision),
    );

    let results = report
        .results
        .iter()
        .map(json_result)
        .collect::<Vec<_>>()
        .join(",\n");

    format!(
        "{{\n  \"schema_version\": {},\n{},\n  \"results\": [\n{}\n  ]\n}}",
        report.schema_version, environment, results
    )
}

fn print_human_summary(report: &Report) {
    println!(
        "{:<8} {:<11} {:<7} {:>10} {:>7} {:>12} {:>12} {:>12}",
        "case", "region", "scale", "iterations", "status", "checksum", "used bytes", "jit ms"
    );
    for result in &report.results {
        let used = match &result.memory {
            Some(memory) => memory.used_bytes.to_string(),
            None => String::from("-"),
        };
        let jit = match result.jit_and_execute {
            Some(timing) => format!("{:.3}", timing.median),
            None => String::from("-"),
        };
        let checksum = match result.checksum {
            Some(value) => value.to_string(),
            None => String::from("-"),
        };
        println!(
            "{:<8} {:<11} {:<7} {:>10} {:>7} {:>12} {:>12} {:>12}",
            result.case,
            result.region.as_str(),
            result.scale.as_str(),
            result.iterations,
            result.status.as_str(),
            checksum,
            used,
            jit
        );
    }
    for result in &report.results {
        if let Some(error) = &result.error {
            eprintln!(
                "FAIL {} {} {}: {error}",
                result.case,
                result.region.as_str(),
                result.scale.as_str()
            );
        }
    }
}

fn selected_scales() -> Vec<Scale> {
    let mut tokens = Vec::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if let Some(value) = argument.strip_prefix("--scale=") {
            tokens.push(value.to_string());
        } else if argument == "--scale" {
            if let Some(value) = arguments.next() {
                tokens.push(value);
            }
        }
    }

    if tokens.is_empty() {
        return vec![Scale::Small, Scale::Medium];
    }

    let mut scales = Vec::new();
    for token in tokens.iter().flat_map(|token| token.split(',')) {
        if token == "all" {
            return vec![Scale::Small, Scale::Medium, Scale::Large];
        }
        if let Some(scale) = Scale::parse(token) {
            if !scales.contains(&scale) {
                scales.push(scale);
            }
        }
    }

    if scales.is_empty() {
        vec![Scale::Small, Scale::Medium]
    } else {
        scales
    }
}

fn json_only() -> bool {
    std::env::args()
        .skip(1)
        .any(|argument| argument == "--json")
}

fn main() {
    let scales = selected_scales();
    let report = run_matrix(&scales);

    if json_only() {
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
