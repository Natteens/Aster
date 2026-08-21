use aster_compiler::{compile, compile_without_array_loop_optimization_for_research, mir};

fn compile_valid(source: &str) -> aster_compiler::Compilation {
    compile(source).unwrap_or_else(|diagnostics| panic!("source must compile: {diagnostics:#?}"))
}

fn baseline(source: &str) -> aster_compiler::Compilation {
    compile_without_array_loop_optimization_for_research(source)
        .unwrap_or_else(|diagnostics| panic!("baseline source must compile: {diagnostics:#?}"))
}

fn proven_indices(module: &mir::Module) -> usize {
    module.to_string().matches("bounds: Proven").count()
}

fn loop_length_reads(module: &mir::Module) -> usize {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .filter(|block| matches!(block.terminator, mir::Terminator::Branch { .. }))
        .flat_map(|block| &block.instructions)
        .filter(|instruction| format!("{instruction:?}").contains("ArrayLength("))
        .count()
}

#[test]
fn canonical_loop_hoists_length_and_proves_exact_read_and_write_accesses() {
    let source = r"
        public int Run() {
            int[] values = new int[8];
            for (int i = 0; i < values.Length; i++) {
                values[i] = values[i] + 1;
            }
            return values[7];
        }
    ";
    let baseline = baseline(source);
    let optimized = compile_valid(source);

    assert_eq!(proven_indices(&baseline.mir), 0);
    assert_eq!(loop_length_reads(&baseline.mir), 1);
    assert_eq!(proven_indices(&optimized.mir), 2);
    assert_eq!(loop_length_reads(&optimized.mir), 0);
    assert_eq!(
        optimized.mir.to_string().matches("ArrayLength(").count(),
        1,
        "the immutable length read is hoisted, not deleted"
    );
}

#[test]
fn dominating_loop_fact_reaches_a_branch_inside_the_body() {
    let optimized = compile_valid(
        "public int Run(bool useIt) { int[] values = new int[4]; for (int i = 0; i < values.Length; i++) { if (useIt) { values[i] = i; } } return 0; }",
    );
    assert_eq!(proven_indices(&optimized.mir), 1);
}

#[test]
fn continue_keeps_the_canonical_latch_but_break_declines_the_proof() {
    let with_continue = compile_valid(
        "public int Run() { int[] values = new int[4]; for (int i = 0; i < values.Length; i++) { if (i == 2) { continue; } values[i] = i; } return values[1]; }",
    );
    assert_eq!(proven_indices(&with_continue.mir), 1);

    let with_break = compile_valid(
        "public int Run() { int[] values = new int[4]; for (int i = 0; i < values.Length; i++) { values[i] = i; if (i == 2) { break; } } return values[1]; }",
    );
    assert_eq!(proven_indices(&with_break.mir), 0);
}

#[test]
fn only_spellings_that_lower_to_the_exact_unit_update_shape_are_accepted() {
    let accepted = [
        "public int Run() { int[] values = new int[4]; for (int i = 0; i < values.Length; i++) { values[i] = i; } return values[3]; }",
        "public int Run() { int[] values = new int[4]; for (int i = 0; i < values.Length; ++i) { values[i] = i; } return values[3]; }",
        "public int Run() { int[] values = new int[4]; for (int i = 0; i < values.Length; i += 1) { values[i] = i; } return values[3]; }",
    ];
    for source in accepted {
        assert_eq!(proven_indices(&compile_valid(source).mir), 1, "{source}");
    }
    let explicit_assignment = "public int Run() { int[] values = new int[4]; for (int i = 0; i < values.Length; i = i + 1) { values[i] = i; } return values[3]; }";
    assert_eq!(
        proven_indices(&compile_valid(explicit_assignment).mir),
        0,
        "the pass deliberately accepts only its exact unit-update MIR"
    );
}

#[test]
fn noncanonical_indices_arrays_and_mutations_remain_checked() {
    let cases = [
        "public int Run() { int[] values = new int[5]; for (int i = 0; i < values.Length; i++) { if (i + 1 < values.Length) { values[i + 1] = i; } } return 0; }",
        "public int Run() { int[] a = new int[5]; int[] b = new int[4]; for (int i = 0; i < a.Length; i++) { if (i < b.Length) { b[i] = i; } } return 0; }",
        "public int Run() { int[] values = new int[5]; int[] other = new int[5]; for (int i = 0; i < values.Length; i++) { values = other; values[i] = i; } return 0; }",
        "public int Run(bool replace) { int[] values = new int[5]; int[] other = new int[5]; for (int i = 0; i < values.Length; i++) { if (replace) { values = other; } values[i] = i; } return 0; }",
        "public int Run() { int[] values = new int[5]; for (int i = 0; i < values.Length; i++) { i = i + 1; if (i < values.Length) { values[i] = i; } } return 0; }",
        "public int Run() { int[] values = new int[5]; for (int i = -1; i < values.Length; i++) { if (i >= 0) { values[i] = i; } } return 0; }",
        "public int Run() { int[] values = new int[5]; int i = 0; i = 0; while (i < values.Length) { values[i] = i; i++; } return 0; }",
        "public int Run() { int[] values = new int[5]; for (int i = 4; i >= 0; i--) { values[i] = i; } return 0; }",
        "public int Run() { int[] values = new int[1]; for (int i = 2147483646; i < values.Length; i++) { values[i] = i; } return 0; }",
    ];
    for source in cases {
        assert_eq!(proven_indices(&compile_valid(source).mir), 0, "{source}");
    }
}

#[test]
fn aliases_and_cached_arbitrary_limits_do_not_gain_array_identity_proof() {
    let alias = compile_valid(
        "public int Run() { int[] a = new int[5]; int[] b = a; for (int i = 0; i < a.Length; i++) { a[i] = i; b[i] = i; } return a[4]; }",
    );
    assert_eq!(proven_indices(&alias.mir), 1);
    assert!(alias.mir.to_string().contains("bounds: Checked"));

    let cached = compile_valid(
        "public int Run() { int[] values = new int[5]; int count = values.Length; for (int i = 0; i < count; i++) { values[i] = i; } return values[4]; }",
    );
    assert_eq!(proven_indices(&cached.mir), 0);
}

#[test]
fn multiple_arrays_require_independent_length_proofs() {
    let optimized = compile_valid(
        "public int Run() { int[] a = new int[5]; int[] b = new int[4]; for (int i = 0; i < a.Length; i++) { a[i] = i; if (i < b.Length) { b[i] = i; } } return a[4]; }",
    );
    assert_eq!(proven_indices(&optimized.mir), 1);
    let debug = optimized.mir.to_string();
    assert!(debug.contains("bounds: Checked"));
}

#[test]
fn nested_loops_do_not_reuse_an_outer_proof_for_an_inner_index() {
    let optimized = compile_valid(
        "public int Run() { int[] values = new int[4]; for (int outer = 0; outer < values.Length; outer++) { for (int inner = 0; inner < values.Length; inner++) { values[inner] = outer; } values[outer] = outer; } return values[3]; }",
    );
    assert_eq!(proven_indices(&optimized.mir), 1);
}

#[test]
fn sequential_loops_reusing_an_induction_name_keep_independent_proofs() {
    let optimized = compile_valid(
        "public int Run() { int[] a = new int[2]; int[] b = new int[3]; for (int i = 0; i < a.Length; i++) { a[i] = i; } for (int i = 0; i < b.Length; i++) { b[i] = i; } return a[1] + b[2]; }",
    );
    assert_eq!(proven_indices(&optimized.mir), 2);
}

#[test]
fn constant_and_runtime_out_of_bounds_accesses_are_never_authorized() {
    let optimized = compile_valid(
        "public int Run(int index) { int[] values = new int[1]; int negative = -1; int first = values[negative]; int second = values[index]; return first + second; }",
    );
    assert_eq!(proven_indices(&optimized.mir), 0);
}
