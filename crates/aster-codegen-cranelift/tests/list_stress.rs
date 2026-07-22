//! Closure/hardening pass for `List<T>` (List D): stress and growth
//! coverage that exercises complete, interleaved `new`/`Add`/`Get`/
//! `RemoveAt`/`Length` sequences through the full pipeline (parser,
//! semantic analysis, HIR, MIR, escape analysis, codegen, JIT execution).
//! No new public API is introduced here; this only adds coverage for the
//! existing operations under sustained, deterministic use. Long-lived
//! memory-metric stress lives alongside the other allocation categories in
//! `memory_stress.rs`; this file is about `List<T>`'s own operational
//! correctness under load.

use std::fmt::Write as _;

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run(source: &str, entry: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, entry).map_err(|error| error.to_string())
}

#[test]
fn a_long_interleaved_sequence_of_every_operation_reaches_the_expected_final_state() {
    // Deterministically interleaves every operation over many iterations:
    // `Add` every step, an extra `Add` every 3rd step, a `RemoveAt(0)` every
    // 5th step, and a running checksum read back via `Get` every step -
    // covering complete new/Add/Get/RemoveAt/Length sequences with
    // operations interleaved rather than run in separate blocks.
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            int checksum = 0;
            for (int step = 0; step < 200; step++)
            {
                values.Add(step);
                if (step % 3 == 0) { values.Add(step * 2); }
                if (step % 5 == 4 && values.Length > 0) { values.RemoveAt(0); }

                int last = values.Get(values.Length - 1);
                checksum = checksum + last;
            }
            return checksum + values.Length;
        }
        ";
    let result = run(source, "Main");
    assert!(
        result.is_ok(),
        "expected the interleaved sequence to run without error, got {result:?}"
    );
    // The exact checksum is a deterministic function of the fixed loop
    // above; pinning it turns any future behavioral drift (dropped element,
    // reordered slot, wrong growth) into an immediate, exact-reproduction
    // failure rather than a vague "it still runs" pass.
    assert_eq!(result, Ok(ExecutionValue::Int(26760)));
}

#[test]
fn growth_across_every_capacity_doubling_preserves_every_element_in_order() {
    // Adds 70 elements (crossing 0->4->8->16->32->64->128), then reads every
    // element back by index and sums them - proving every element survives
    // every growth in its original order and position, not just that the
    // final `Length` is correct.
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            for (int i = 0; i < 70; i++)
            {
                values.Add(i * 3 + 1);
            }
            int sum = 0;
            for (int i = 0; i < values.Length; i++)
            {
                sum = sum + values.Get(i);
            }
            return sum;
        }
        ";
    // sum_{i=0}^{69} (3i + 1) = 3 * (69*70/2) + 70 = 3*2415 + 70 = 7315
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(7315)));
}

#[test]
fn growth_only_happens_when_length_equals_capacity_never_early() {
    // Adds exactly up to each doubling boundary and confirms `Length`
    // (the only externally observable signal) tracks the element count
    // exactly, with no off-by-one growth trigger, at every boundary
    // 0->4->8->16->32.
    for boundary in [4, 8, 16, 32] {
        let mut source = String::from("public int Main() { List<int> values = new List<int>();");
        for i in 0..boundary {
            let _ = write!(source, "values.Add({i});");
        }
        source.push_str("return values.Length; }");
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Int(boundary)),
            "boundary {boundary} did not produce the expected length"
        );
    }
}

#[test]
fn emptying_and_refilling_a_list_reuses_its_capacity_without_visible_growth_error() {
    // Grows to 16, empties completely via `RemoveAt`, then refills to 16
    // again - the second fill must not need to re-cross any growth boundary
    // in a way that's user-visible (no error, same final length).
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            for (int i = 0; i < 16; i++) { values.Add(i); }
            for (int i = 0; i < 16; i++) { values.RemoveAt(0); }
            for (int i = 0; i < 16; i++) { values.Add(i * 2); }
            int sum = 0;
            for (int i = 0; i < values.Length; i++) { sum = sum + values.Get(i); }
            return sum;
        }
        ";
    // sum_{i=0}^{15} (2i) = 2 * (15*16/2) = 240
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(240)));
}

#[test]
fn two_lists_growing_in_lockstep_never_cross_contaminate() {
    let source = "
        public int Main()
        {
            List<int> a = new List<int>();
            List<int> b = new List<int>();
            for (int i = 0; i < 50; i++)
            {
                a.Add(i);
                b.Add(-i);
            }
            return a.Get(49) + b.Get(49) + a.Length + b.Length;
        }
        ";
    // a.Get(49) = 49, b.Get(49) = -49, a.Length = b.Length = 50
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(100)));
}

#[test]
#[ignore = "long-running List<T> stress"]
fn a_much_larger_deterministic_sequence_survives_sustained_growth_and_shrink() {
    // A bigger-than-normal, fully deterministic load: grows to 5,000
    // elements (crossing every capacity doubling from 0 up past 4096),
    // reads every element back, then repeatedly empties and refills in
    // smaller batches. No randomness, no network, no hardware dependency,
    // no `sleep`, and no time-based assertion - the same input always
    // produces the same checksum, so a regression reproduces exactly.
    let source = "
        public int Main()
        {
            List<int> values = new List<int>();
            for (int i = 0; i < 5000; i++)
            {
                values.Add(i);
            }
            long sum = 0L;
            for (int i = 0; i < values.Length; i++)
            {
                sum = sum + (long)values.Get(i);
            }
            for (int round = 0; round < 20; round++)
            {
                for (int i = 0; i < 100; i++) { values.RemoveAt(0); }
                for (int i = 0; i < 100; i++) { values.Add(i); }
            }
            return (int)(sum % 1000000007L) + values.Length;
        }
        ";
    // sum_{i=0}^{4999} i = 4999*5000/2 = 12497500; 20 rounds of
    // remove-100/add-100 leave Length unchanged at 5000.
    assert_eq!(
        run(source, "Main"),
        Ok(ExecutionValue::Int(12_497_500 + 5000))
    );
}
