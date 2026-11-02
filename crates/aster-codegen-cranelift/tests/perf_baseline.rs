#[allow(dead_code)]
#[path = "../examples/perf_baseline.rs"]
mod baseline;

use std::sync::OnceLock;

use aster_codegen_cranelift::PreparedSequentialExecution;
use aster_compiler::compile;
use baseline::{Config, PHASES, Report, parse_args, run, serialize_report, workloads};

fn args(values: &[&str]) -> Result<Config, String> {
    parse_args(values.iter().map(|value| (*value).to_owned()))
}

fn report() -> &'static Report {
    static REPORT: OnceLock<Report> = OnceLock::new();
    REPORT
        .get_or_init(|| run(&args(&["--warmup", "0", "--samples", "2"]).expect("valid arguments")))
}

#[test]
fn defaults_and_valid_flags_parse() {
    let config = args(&[]).unwrap();
    assert_eq!((config.warmup, config.samples), (1, 5));
    let config = args(&[
        "--json",
        "--samples=3",
        "--warmup=0",
        "--workload",
        "strings,generics",
    ])
    .unwrap();
    assert!(config.json_only);
    assert_eq!((config.warmup, config.samples), (0, 3));
    assert_eq!(config.workloads, ["strings", "generics"]);
}

#[test]
fn invalid_arguments_are_rejected_cleanly() {
    for bad in [
        &["--samples", "0"][..],
        &["--samples", "many"],
        &["--warmup", "-1"],
        &["--samples", "999999"],
        &["--samples"],
        &["--bogus"],
        &["--workload", "nope"],
        &["--category", "nope"],
        &["--json=1"],
    ] {
        assert!(args(bad).is_err(), "{bad:?} should be rejected");
    }
}

#[test]
fn every_workload_passes_with_independent_expected_checksum() {
    let report = report();
    assert_eq!(report.results.len(), workloads().len());
    for result in &report.results {
        assert_eq!(result.error, None, "{}", result.workload.name);
        assert_eq!(result.checksum, Some(result.workload.expected));
    }
}

#[test]
fn every_phase_has_the_configured_sample_count() {
    for result in &report().results {
        for ((name, _, _), samples) in PHASES.iter().zip(&result.samples.phases) {
            assert_eq!(samples.len(), 2, "{} {name}", result.workload.name);
        }
    }
}

#[test]
#[allow(clippy::needless_range_loop)]
fn end_to_end_encloses_the_individually_timed_phases() {
    // The outer timer strictly contains the inner intervals, so this is a
    // structural property of the monotonic clock, not a timing threshold.
    for result in &report().results {
        let p = &result.samples.phases;
        for i in 0..2 {
            let inner = p[0][i] + p[1][i] + p[2][i] + p[3][i] + p[5][i];
            assert!(p[7][i] >= inner, "{} sample {i}", result.workload.name);
            assert_eq!(p[4][i], p[2][i] + p[3][i]);
        }
    }
}

#[test]
fn json_has_stable_shape_and_separates_invariants_from_timings() {
    let json = serialize_report(report());
    assert_eq!(json.matches('{').count(), json.matches('}').count());
    assert_eq!(json.matches('[').count(), json.matches(']').count());
    for key in [
        "\"schema_version\":1",
        "\"kind\":\"aster-performance-baseline\"",
        "\"git_revision\":",
        "\"timing_unit\":\"ns\"",
        "\"invariants\":",
        "\"timings\":",
    ] {
        assert!(json.contains(key), "missing {key}");
    }
    for workload in workloads() {
        assert!(json.contains(&format!("\"name\":\"{}\"", workload.name)));
    }
    for (phase, mode, _) in PHASES {
        assert!(json.contains(&format!("\"{phase}\":{{\"mode\":\"{mode}\"")));
    }
}

#[test]
fn category_filter_selects_matching_workloads() {
    let config = args(&["--category", "numeric", "--warmup", "0", "--samples", "1"]).unwrap();
    let report = run(&config);
    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].workload.name, "numeric_loops");
}

#[test]
fn prepared_artifact_is_reusable_and_matches_production_execution() {
    let source =
        "public int Run() { int t = 0; for (int i = 0; i < 100; i++) { t += i; } return t; }";
    let compilation = compile(source).unwrap();
    let production = aster_codegen_cranelift::execute(&compilation.mir, "Run").unwrap();
    let (program, timings) =
        PreparedSequentialExecution::prepare_timed(&compilation.mir, "Run").unwrap();
    assert!(timings.codegen > std::time::Duration::ZERO);
    for _ in 0..3 {
        assert_eq!(program.invoke().unwrap(), production);
    }
}

#[test]
fn prepare_timed_fails_closed_for_unknown_entry() {
    let compilation = compile("public int Run() { return 1; }").unwrap();
    assert!(PreparedSequentialExecution::prepare_timed(&compilation.mir, "Missing").is_err());
}
