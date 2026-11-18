//! `Parallel.Reduce` is reproducible (Q-CONC-4 / CONC-009): chunk boundaries
//! depend only on the input length (`chunks = min(length, 64)`, chunk `i`
//! covers `[i * length / chunks, (i + 1) * length / chunks)`), partials are
//! combined left to right in chunk order, and the worker count only affects
//! scheduling. Non-associative combines therefore give the same result for
//! every worker count.

use aster_codegen_cranelift::{ExecutionValue, execute_with_worker_count};
use aster_compiler::compile;

const WORKER_COUNTS: [usize; 5] = [1, 2, 3, 4, 8];
const LENGTHS: [usize; 7] = [1, 5, 63, 64, 65, 1000, 4099];

fn run(source: &str, workers: usize) -> ExecutionValue {
    let compilation = compile(source).unwrap_or_else(|d| panic!("{d:#?}"));
    execute_with_worker_count(&compilation.mir, "Main", workers)
        .unwrap_or_else(|error| panic!("workers {workers}: {error}"))
        .0
}

/// Independent model of the rule: the chunk grouping, folded sequentially.
fn chunks(length: usize) -> Vec<std::ops::Range<usize>> {
    let count = length.min(64);
    (0..count)
        .map(|i| (i * length / count)..((i + 1) * length / count))
        .collect()
}

fn double_value(index: usize) -> f64 {
    if index.is_multiple_of(3) { 1.0e16 } else { 0.1 }
}

#[test]
fn floating_point_sum_is_identical_for_every_worker_count() {
    for length in LENGTHS {
        let source = format!(
            "public double Add(double a, double b) {{ return a + b; }} \
             public double Main() {{ \
                 double[] values = new double[{length}]; \
                 for (int i = 0; i < {length}; i++) {{ values[i] = i % 3 == 0 ? 10000000000000000.0d : 0.1d; }} \
                 return Parallel.Reduce(values, 0.0d, Add, Add); \
             }}"
        );
        let mut expected = 0.0f64;
        for range in chunks(length) {
            let mut partial = 0.0f64;
            for index in range {
                partial += double_value(index);
            }
            expected += partial;
        }
        for workers in WORKER_COUNTS {
            assert_eq!(
                run(&source, workers),
                ExecutionValue::double(expected),
                "length {length}, workers {workers}"
            );
        }
    }
}

#[test]
fn non_associative_integer_combine_is_identical_for_every_worker_count() {
    // Accumulate: (acc * 3 + value) % M ; Combine: (left * 7 - right) % M.
    // Both are order and grouping sensitive.
    const M: i64 = 1_000_003;
    for length in LENGTHS {
        let source = format!(
            "public long Acc(long a, long v) {{ return (a * 3L + v) % 1000003L; }} \
             public long Comb(long l, long r) {{ return (l * 7L - r) % 1000003L; }} \
             public long Main() {{ \
                 long[] values = new long[{length}]; \
                 for (int i = 0; i < {length}; i++) {{ values[i] = (long)(i * 37 % 101); }} \
                 return Parallel.Reduce(values, 0L, Acc, Comb); \
             }}"
        );
        let mut partials = Vec::new();
        for range in chunks(length) {
            let mut acc = 0i64;
            for index in range {
                acc = (acc * 3 + i64::try_from(index * 37 % 101).unwrap()) % M;
            }
            partials.push(acc);
        }
        let mut expected = partials[0];
        for right in &partials[1..] {
            expected = (expected * 7 - right) % M;
        }
        for workers in WORKER_COUNTS {
            assert_eq!(
                run(&source, workers),
                ExecutionValue::Long(expected),
                "length {length}, workers {workers}"
            );
        }
    }
}

#[test]
fn empty_input_returns_the_identity_for_every_worker_count() {
    let source = "public int Add(int a, int b) { return a + b; } \
         public int Main() { int[] values = new int[0]; return Parallel.Reduce(values, 7, Add, Add); }";
    for workers in WORKER_COUNTS {
        assert_eq!(run(source, workers), ExecutionValue::Int(7));
    }
}
