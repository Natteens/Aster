//! `Random.FromEntropy()`: an OS-seeded generator that leaves every explicitly
//! seeded sequence untouched and stays out of worker bodies.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_stats};
use aster_compiler::{ProjectCompilation, compile_project, mir};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn project(source: &str) -> ProjectCompilation {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("aster-entropy-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary project source");
    let compilation = compile_project(&path);
    std::fs::remove_file(&path).expect("remove temporary project source");
    compilation.unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
}

fn run(compilation: &ProjectCompilation, function: &str) -> ExecutionValue {
    execute(&compilation.compilation.mir, function).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn entropy_generators_differ_and_respect_the_documented_ranges() {
    let compilation = project(
        "using aster.random;
         public int Distinct()
         {
             ulong[] firsts = new ulong[64];
             for (int i = 0; i < 64; i++)
             {
                 Random random = Random.FromEntropy();
                 firsts[i] = random.NextULong();
             }
             for (int i = 0; i < 64; i++)
             {
                 for (int j = i + 1; j < 64; j++)
                 {
                     if (firsts[i] == firsts[j]) { return 1; }
                 }
             }
             return 0;
         }
         public int Ranges()
         {
             Random random = Random.FromEntropy();
             for (int i = 0; i < 20000; i++)
             {
                 int value = random.NextInt(10, 20);
                 if (value < 10 || value >= 20) { return 1; }
                 long wide = random.NextLong(-5L, 5L);
                 if (wide < -5L || wide >= 5L) { return 2; }
                 float chance = random.NextFloat();
                 if (chance < 0.0f || chance >= 1.0f) { return 3; }
                 double unit = random.NextDouble();
                 if (unit < 0.0d || unit >= 1.0d) { return 4; }
             }
             return 0;
         }
         public int Advances()
         {
             Random random = Random.FromEntropy();
             ulong first = random.NextULong();
             ulong second = random.NextULong();
             if (first == second) { return 1; }
             return 0;
         }",
    );
    for function in ["Distinct", "Ranges", "Advances"] {
        assert_eq!(
            run(&compilation, function),
            ExecutionValue::Int(0),
            "{function}"
        );
    }
}

#[test]
fn explicit_seeds_keep_their_exact_sequences() {
    let compilation = project(
        "using aster.random;
         public int Fixed()
         {
             // Reference values of the SplitMix64 sequence for seeds 0, 1 and 123.
             if (new Random(0).NextULong() != 16294208416658607535UL) { return 1; }
             if (new Random(1).NextULong() != 10451216379200822465UL) { return 2; }
             Random random = new Random(123);
             if (random.NextULong() != 13032462758197477675UL) { return 3; }
             if (random.NextULong() != 18015028434894305148UL) { return 4; }
             if (random.NextULong() != 15857969311440292840UL) { return 5; }
             Random a = new Random(99);
             Random b = new Random(99);
             for (int i = 0; i < 100; i++)
             {
                 if (a.NextInt(0, 1000) != b.NextInt(0, 1000)) { return 6; }
             }
             return 0;
         }",
    );
    assert_eq!(run(&compilation, "Fixed"), ExecutionValue::Int(0));
}

#[test]
fn invalid_ranges_stay_controlled_errors_on_entropy_generators() {
    let compilation = project(
        "using aster.random;
         public int Empty() { Random random = Random.FromEntropy(); return random.NextInt(5, 5); }
         public int Reversed() { Random random = Random.FromEntropy(); return random.NextInt(9, 3); }",
    );
    for function in ["Empty", "Reversed"] {
        let error = execute(&compilation.compilation.mir, function)
            .expect_err("an invalid range is a controlled runtime error");
        assert!(
            error.to_string().contains("Aster runtime error"),
            "{function}: {error}"
        );
    }
}

#[test]
fn workers_use_explicit_seeds_and_reject_entropy() {
    let compilation = project(
        "using aster.random;
         public int Roll(ulong seed) { Random random = new Random(seed); return random.NextInt(0, 100); }
         public int Seeded()
         {
             Task<int> first = Task.Run(Roll, 42UL);
             Task<int> second = Task.Run(Roll, 42UL);
             Random local = new Random(42);
             int expected = local.NextInt(0, 100);
             if (first.Wait() != expected) { return 1; }
             if (second.Wait() != expected) { return 2; }
             return 0;
         }
         public int Unseeded() { Random random = Random.FromEntropy(); return random.NextInt(0, 100); }
         public int MainThread() { return Unseeded() >= 0 ? 0 : 1; }",
    );
    // A worker given an explicit seed reproduces the sequential result, and the
    // main thread may draw entropy for the seeds it hands out.
    assert_eq!(run(&compilation, "Seeded"), ExecutionValue::Int(0));
    assert_eq!(run(&compilation, "MainThread"), ExecutionValue::Int(0));

    // Validation is module-wide and fail-closed, so the rejected shape lives in
    // its own module: a worker body that (transitively) reads entropy.
    for source in [
        "using aster.random;
         public int Unseeded() { Random random = Random.FromEntropy(); return random.NextInt(0, 100); }
         public int Entropy() { Task<int> task = Task.Run(Unseeded); return task.Wait(); }",
        "using aster.random;
         public int Helper() { Random random = Random.FromEntropy(); return random.NextInt(0, 100); }
         public int Indirect() { return Helper() + 1; }
         public int Entropy() { Task<int> task = Task.Run(Indirect); return task.Wait(); }",
    ] {
        let rejected = project(source);
        let error = execute(&rejected.compilation.mir, "Entropy")
            .expect_err("an entropy read in a worker body is rejected before execution");
        assert!(
            error
                .to_string()
                .contains("entropy host operation through `Random.FromEntropy`"),
            "{error}"
        );
    }
}

#[test]
fn only_the_factory_touches_the_host_and_the_hot_path_is_unchanged() {
    let compilation = project(
        "using aster.random;
         public ulong Draw() { Random random = Random.FromEntropy(); return random.NextULong(); }",
    );
    let mir = &compilation.compilation.mir;
    let intrinsics = |name: &str| -> (usize, Vec<String>) {
        let function = mir
            .functions
            .iter()
            .find(|function| function.name == name)
            .unwrap_or_else(|| panic!("MIR function {name}"));
        let mut calls = 0;
        let mut intrinsics = Vec::new();
        for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
            match instruction {
                mir::Instruction::Call { .. } => calls += 1,
                mir::Instruction::CallIntrinsic { intrinsic, .. } => {
                    intrinsics.push(format!("{intrinsic:?}"));
                }
                _ => {}
            }
        }
        (calls, intrinsics)
    };
    // The entropy read happens once, in the factory.
    assert_eq!(intrinsics("FromEntropy").1, ["RandomEntropySeed"]);
    // The per-number path is exactly what it was: one mixer intrinsic, no call.
    for name in ["NextULong", "NextUInt"] {
        let (calls, found) = intrinsics(name);
        assert_eq!(calls, 0, "{name}");
        assert_eq!(found, ["RandomMix"], "{name}");
    }

    // No string or array allocation, however many generators are created.
    let (value, stats) = execute_with_stats(mir, "Draw").expect("runs");
    assert!(matches!(value, ExecutionValue::ULong(_)));
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.array_allocations, 0);
}
