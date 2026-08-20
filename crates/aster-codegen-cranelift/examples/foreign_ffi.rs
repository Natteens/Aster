//! Minimal execution-scoped native registration and informational call matrix.

#![allow(unsafe_code)]

use std::time::Instant;

use aster_codegen_cranelift::{
    ExecutionValue, ForeignRegistry, ForeignSignature, ForeignType, execute,
    execute_with_foreign_registry,
};
use aster_compiler::compile;

const SAMPLES: usize = 7;

extern "C" fn add_one(value: i64, out: *mut i64) -> i32 {
    if out.is_null() {
        return 91;
    }
    unsafe { out.write(value.wrapping_add(1)) };
    0
}

extern "C" fn fail(_value: i64, _out: *mut i64) -> i32 {
    23
}

fn registry(address: *const ()) -> ForeignRegistry {
    let mut registry = ForeignRegistry::new();
    let signature = ForeignSignature::new([ForeignType::Long], ForeignType::Long)
        .expect("example signature is valid");
    // SAFETY: both example wrappers have this exact C ABI and static lifetime.
    unsafe { registry.register("NativeAddOne", signature, address) }
        .expect("example binding is unique");
    registry
}

fn source(iterations: i64, foreign: bool) -> String {
    let declaration = if foreign {
        "public unsafe foreign long NativeAddOne(long value);"
    } else {
        "public long NativeAddOne(long value) { return value + 1; }"
    };
    let call = if foreign {
        "unsafe { total += NativeAddOne(i); }"
    } else {
        "total += NativeAddOne(i);"
    };
    format!(
        "{declaration} public long Run() {{ long total = 0; for (long i = 0; i < {iterations}; i++) {{ {call} }} return total; }}"
    )
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[SAMPLES / 2]
}

fn main() {
    let bindings = registry(add_one as *const ());
    for iterations in [1_i64, 1_000, 100_000, 1_000_000] {
        let direct = compile(&source(iterations, false)).expect("direct source compiles");
        let foreign = compile(&source(iterations, true)).expect("foreign source compiles");
        let expected = ExecutionValue::Long(iterations * (iterations + 1) / 2);
        let mut direct_ms = Vec::with_capacity(SAMPLES);
        let mut foreign_ms = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let start = Instant::now();
            let value = execute(&direct.mir, "Run").expect("direct source executes");
            direct_ms.push(start.elapsed().as_secs_f64() * 1_000.0);
            assert_eq!(value, expected);

            let start = Instant::now();
            let value = execute_with_foreign_registry(&foreign.mir, "Run", &bindings)
                .expect("registered source executes");
            foreign_ms.push(start.elapsed().as_secs_f64() * 1_000.0);
            assert_eq!(value, expected);
        }
        println!(
            "calls={iterations:<7} direct_jit_exec_ms={:>8.3} foreign_jit_exec_ms={:>8.3}",
            median(direct_ms),
            median(foreign_ms)
        );
    }

    let failure = compile(&source(1, true)).expect("failure source compiles");
    let error = execute_with_foreign_registry(&failure.mir, "Run", &registry(fail as *const ()))
        .expect_err("non-zero native status becomes a controlled ASTER error");
    assert!(error.message().contains("native status 23"));
}
