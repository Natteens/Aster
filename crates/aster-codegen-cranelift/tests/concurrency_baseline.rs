#[allow(dead_code)]
#[path = "../examples/concurrency_baseline.rs"]
mod baseline;

use aster_codegen_cranelift::execute_with_worker_count;
use aster_compiler::compile;
use baseline::{WORKLOADS, measure, parse_args};

#[test]
fn checksums_are_independent_of_the_worker_count() {
    for workload in &WORKLOADS {
        for workers in [1, 2, 4] {
            let sample = measure(workload, workers, 1)
                .unwrap_or_else(|error| panic!("{} at {workers}: {error}", workload.name));
            assert_eq!(sample.checksum, workload.expected);
            assert_eq!(sample.nanos.len(), 1);
        }
    }
}

#[test]
fn zero_workers_is_a_controlled_error() {
    let compilation = compile(WORKLOADS[0].source).unwrap();
    let error = execute_with_worker_count(&compilation.mir, "Run", 0).unwrap_err();
    assert!(error.to_string().contains("at least 1"), "{error}");
}

#[test]
fn arguments_parse_and_reject_invalid_values() {
    let owned = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    assert_eq!(parse_args(owned(&[])).unwrap(), (vec![1, 2, 4], 5));
    assert_eq!(
        parse_args(owned(&["--workers", "1,8", "--samples", "3"])).unwrap(),
        (vec![1, 8], 3)
    );
    for bad in [
        &["--workers", "0"][..],
        &["--workers", "1,x"],
        &["--samples"],
        &["--bogus", "1"],
    ] {
        assert!(parse_args(owned(bad)).is_err(), "{bad:?}");
    }
}
