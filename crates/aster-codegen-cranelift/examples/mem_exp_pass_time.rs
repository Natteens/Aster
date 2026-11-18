//! MEM-EXP-1 compile-time gate input: front end, backend MIR validation,
//! and the C6 ownership pass plus its validator, timed separately on the
//! generated `frontend_large` program (closure §4.C). Run with `--release`.

#[path = "perf_baseline.rs"]
#[allow(dead_code)]
mod baseline;

use std::time::Instant;

use aster_compiler::{compile, mir};

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn main() {
    let (source, _) = baseline::generate_large_program(baseline::GENERATED_UNITS);
    let mut front = Vec::new();
    let mut validate = Vec::new();
    let mut ownership = Vec::new();
    for _ in 0..15 {
        let started = Instant::now();
        let module = compile(&source).expect("generated program compiles").mir;
        front.push(started.elapsed().as_secs_f64() * 1e3);
        let started = Instant::now();
        mir::validate::validate_types(&module).expect("generated MIR validates");
        validate.push(started.elapsed().as_secs_f64() * 1e3);
        let started = Instant::now();
        let (counted, _) = mir::ownership::insert_ownership_elided(&module);
        mir::validate::ownership::validate_ownership(&counted).expect("ownership validates");
        ownership.push(started.elapsed().as_secs_f64() * 1e3);
    }
    let (front, validate, ownership) = (median(front), median(validate), median(ownership));
    println!("front_end_ms {front:.3}");
    println!("mir_validation_ms {validate:.3}");
    println!("ownership_pass_and_validator_ms {ownership:.3}");
    println!(
        "C6/C0 (front end + validation [+ ownership]) = {:.3}",
        (front + validate + ownership) / (front + validate)
    );
}
