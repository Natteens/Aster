use aster_compiler::{compile, mir};

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source should be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn lowers_foreign_declarations_and_calls_to_typed_mir() {
    let compilation = compile(
        r"
        public unsafe foreign int NativeAdd(int left, int right);
        public int Run() {
            unsafe { return NativeAdd(20, 22); }
        }
        ",
    )
    .expect("foreign source compiles without requiring a host binding");
    assert_eq!(compilation.mir.foreign_functions.len(), 1);
    let declaration = &compilation.mir.foreign_functions[0];
    assert_eq!(declaration.name, "NativeAdd");
    assert_eq!(declaration.parameters, [mir::Type::Int, mir::Type::Int]);
    assert_eq!(declaration.return_type, mir::Type::Int);
    assert!(compilation.mir.functions.iter().any(|function| {
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::ForeignCall {
                        function: symbol,
                        return_type: mir::Type::Int,
                        ..
                    } if *symbol == declaration.symbol
                )
            })
    }));
}

#[test]
fn foreign_calls_require_lexical_unsafe_but_safe_wrappers_are_ordinary() {
    let diagnostics = messages(
        "public unsafe foreign int Native(int value); public int Run() { return Native(1); }",
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("requires an `unsafe` block"))
    );

    compile(
        r"
        public unsafe foreign int Native(int value);
        public int Safe(int value) { unsafe { return Native(value); } }
        public int Run() { return Safe(42); }
        ",
    )
    .expect("ordinary callers may use a safe wrapper");
}

#[test]
fn foreign_signatures_fail_closed_before_hir() {
    for source in [
        "public unsafe foreign string Bad(int value);",
        "public unsafe foreign int Bad(string value);",
        "public unsafe foreign decimal Bad(int value);",
        "public unsafe foreign int Bad(int[] value);",
        "public unsafe foreign int Bad(List<int> value);",
        "public class Box { public Box() {} } public unsafe foreign int Bad(Box value);",
        "public struct Pair { public int value; } public unsafe foreign int Bad(Pair value);",
        "public interface IValue { int Get(); } public unsafe foreign int Bad(IValue value);",
        "public unsafe foreign int Bad<T>(int value);",
        "public unsafe foreign async int Bad(int value);",
        "public unsafe foreign int bad;",
        "public unsafe foreign class Bad {}",
    ] {
        assert!(
            compile(source).is_err(),
            "accepted invalid foreign source: {source}"
        );
    }
}

#[test]
fn foreign_modifier_misuses_remain_controlled_parse_or_semantic_errors() {
    for source in [
        "public foreign int MissingUnsafe();",
        "public unsafe foreign int Bodied() { return 0; }",
        "public unsafe unsafe foreign int Duplicate();",
        "public foreign unsafe int WrongOrder();",
        "public class Holder { public unsafe foreign int Method(); }",
    ] {
        assert!(
            compile(source).is_err(),
            "accepted invalid modifier use: {source}"
        );
    }
}

#[test]
fn foreign_calls_are_rejected_directly_and_transitively_from_workers() {
    let direct = messages(
        r"
        public unsafe foreign int Native(int value);
        public int Work(int value) { unsafe { return Native(value); } }
        public int Run() { return Task.Run(Work, 1).Wait(); }
        ",
    );
    assert!(
        direct
            .iter()
            .any(|message| message.contains("foreign call"))
    );

    let transitive = messages(
        r"
        public unsafe foreign int Native(int value);
        public int Host(int value) { unsafe { return Native(value); } }
        public int Work(int value) { return Host(value); }
        public int Run() { return Task.Run(Work, 1).Wait(); }
        ",
    );
    assert!(
        transitive
            .iter()
            .any(|message| message.contains("foreign call"))
    );

    let parallel = messages(
        r"
        public unsafe foreign int Native(int value);
        public void Body(int value) { unsafe { Native(value); } }
        public int Run() { Parallel.For(0, 1, Body); return 0; }
        ",
    );
    assert!(
        parallel
            .iter()
            .any(|message| message.contains("foreign call"))
    );

    let parallel_transitive = messages(
        r"
        public unsafe foreign int Native(int value);
        public int Host(int value) { unsafe { return Native(value); } }
        public void Body(int value) { Host(value); }
        public int Run() { Parallel.For(0, 1, Body); return 0; }
        ",
    );
    assert!(
        parallel_transitive
            .iter()
            .any(|message| message.contains("foreign call"))
    );

    let for_each = messages(
        r"
        public unsafe foreign int Native(int value);
        public void Body(int value) { unsafe { Native(value); } }
        public int Run() {
            int[] values = [1];
            Parallel.ForEach(values, Body);
            return 0;
        }
        ",
    );
    assert!(
        for_each
            .iter()
            .any(|message| message.contains("foreign call"))
    );

    let reduce = messages(
        r"
        public unsafe foreign int Native(int value);
        public int Host(int value) { unsafe { return Native(value); } }
        public int Accumulate(int total, int value) { return Host(total + value); }
        public int Combine(int left, int right) { return left + right; }
        public int Run() {
            int[] values = [1];
            return Parallel.Reduce(values, 0, Accumulate, Combine);
        }
        ",
    );
    assert!(
        reduce
            .iter()
            .any(|message| message.contains("foreign call"))
    );
}

#[test]
fn unsafe_is_contextual_and_nested_blocks_accept_safe_code() {
    compile(
        r"
        public int unsafe(int foreign) { return foreign; }
        public unsafe foreign int Native(int value);
        public int Run() {
            int unsafe = 40;
            int foreign = 2;
            unsafe {
                if (true) {
                    unsafe { int value = unsafe(41); }
                    return Native(unsafe + foreign);
                }
            }
            return 0;
        }
        ",
    )
    .expect("unsafe and foreign remain contextual identifiers");
}

#[test]
fn ordinary_async_may_use_the_registered_host_on_its_non_worker_path() {
    compile(
        r"
        public unsafe foreign int Native(int value);
        public int Compute() { return 1; }
        public async Task<int> RunAsync() {
            int native = 0;
            unsafe { native = Native(41); }
            int worker = await Task.Run(Compute);
            return native + worker;
        }
        ",
    )
    .expect("ordinary async host path follows existing host-operation rules");
}

#[test]
fn optimizer_preserves_foreign_calls_as_opaque_effectful_operations() {
    let compilation = compile(
        r"
        public unsafe foreign void Observe(int value);
        public int Run() { unsafe { Observe(42); } return 7; }
        ",
    )
    .unwrap();
    assert!(compilation.mir.functions.iter().any(|function| {
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(instruction, mir::Instruction::ForeignCall { .. }))
    }));
}
