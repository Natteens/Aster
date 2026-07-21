#[allow(dead_code)]
#[path = "../examples/memory_matrix.rs"]
mod matrix;

use std::sync::OnceLock;

use aster_codegen_cranelift::execute_with_stats;
use aster_compiler::compile;
use matrix::{Region, Report, Scale, Status, run_matrix, serialize_report, workloads};

fn small_report() -> &'static Report {
    static REPORT: OnceLock<Report> = OnceLock::new();
    REPORT.get_or_init(|| run_matrix(&[Scale::Small]))
}

fn expected_categories(case: &str) -> (u64, u64, u64) {
    match case {
        "object" => (1, 0, 0),
        "array" => (0, 1, 0),
        "string" => (0, 0, 1),
        "mixed" => (1, 1, 1),
        other => panic!("unexpected case `{other}`"),
    }
}

#[test]
fn all_eight_cases_pass_at_small_scale() {
    let report = small_report();
    assert_eq!(report.results.len(), 8);
    for result in &report.results {
        assert_eq!(
            result.status,
            Status::Pass,
            "case {} {} failed: {:?}",
            result.case,
            result.region.as_str(),
            result.error
        );
        assert_eq!(result.iterations, 10_000);
        assert_eq!(result.checksum, Some(result.expected_checksum));
    }
}

#[test]
fn checksums_and_total_counts_are_correct() {
    let report = small_report();
    for result in &report.results {
        let memory = result.memory.as_ref().expect("passing case exposes memory");
        let (objects, arrays, strings) = expected_categories(result.case);
        let iterations = result.iterations;

        let per_iteration = result.expected_checksum / i64::try_from(iterations).unwrap();
        let expected_per_iteration = match result.case {
            "object" => 39,
            "array" => 7,
            "string" => 2,
            "mixed" => 42,
            other => panic!("unexpected case `{other}`"),
        };
        assert_eq!(per_iteration, expected_per_iteration);
        assert_eq!(result.checksum, Some(result.expected_checksum));

        assert_eq!(
            memory.total_allocations,
            (objects + arrays + strings) * iterations
        );
    }
}

#[test]
fn per_category_counts_match_each_workload() {
    let report = small_report();
    for result in &report.results {
        let memory = result.memory.as_ref().expect("passing case exposes memory");
        let (objects, arrays, strings) = expected_categories(result.case);
        let iterations = result.iterations;

        assert_eq!(memory.object_allocations, objects * iterations);
        assert_eq!(memory.array_allocations, arrays * iterations);
        assert_eq!(memory.string_allocations, strings * iterations);
    }
}

#[test]
fn temporary_cases_reclaim_all_used_bytes() {
    let report = small_report();
    for result in &report.results {
        if result.region == Region::Temporary {
            let memory = result.memory.as_ref().expect("passing case exposes memory");
            assert_eq!(memory.used_bytes, 0, "case {}", result.case);
            assert!(memory.peak_used_bytes > 0, "case {}", result.case);
        }
    }
}

#[test]
fn persistent_cases_retain_used_bytes() {
    let report = small_report();
    for result in &report.results {
        if result.region == Region::Persistent {
            let memory = result.memory.as_ref().expect("passing case exposes memory");
            assert!(memory.used_bytes > 0, "case {}", result.case);
            assert!(
                memory.peak_used_bytes >= memory.used_bytes,
                "case {}",
                result.case
            );
        }
    }
}

#[test]
fn deterministic_metrics_are_stable_between_executions() {
    let workloads = workloads();
    let workload = workloads
        .iter()
        .find(|workload| workload.case == "object" && workload.region == Region::Persistent)
        .expect("object persistent workload exists");

    let compilation = compile(workload.source).expect("workload compiles");
    let (first_value, first_stats) =
        execute_with_stats(&compilation.mir, "RunSmall").expect("first execution succeeds");
    let (second_value, second_stats) =
        execute_with_stats(&compilation.mir, "RunSmall").expect("second execution succeeds");

    assert_eq!(first_value, second_value);
    assert_eq!(first_stats, second_stats);
}

#[test]
fn structured_report_serializes_to_valid_json() {
    let report = small_report();
    let json = serialize_report(report);

    assert!(json.contains("\"schema_version\": 1"));
    assert!(json.contains("\"environment\""));
    assert!(json.contains("\"aster_version\""));
    assert!(json.contains("\"results\""));
    assert!(json.contains("\"timing_ms\""));
    assert!(json.contains("\"jit_and_execute\""));
    assert!(json.contains("\"used_bytes\""));

    assert_eq!(json.matches("\"case\":").count(), 8);

    let mut braces: i64 = 0;
    let mut brackets: i64 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for character in json.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' => braces += 1,
            '}' => braces -= 1,
            '[' => brackets += 1,
            ']' => brackets -= 1,
            _ => {}
        }
        assert!(braces >= 0 && brackets >= 0);
    }
    assert_eq!(braces, 0);
    assert_eq!(brackets, 0);
}
