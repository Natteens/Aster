//! M2B: `aster.io.IOErrorKind`/`IOError`, the portable filesystem error model
//! prepared ahead of M2C (`Path`) and M2D (read/write). No filesystem
//! operation exists yet; this only covers the type shape, `Result<T,
//! IOError>` construction/propagation, layout, nominal identity, MIR
//! adulteration, and the absence of ASTER allocation.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute, execute_with_stats};
use aster_compiler::{compile_project, mir};

fn compile(source: &str) -> Result<mir::Module, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("aster-io-error-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    compilation.map(|compilation| compilation.compilation.mir)
}

fn compile_errors(source: &str) -> String {
    match compile(source) {
        Ok(_) => String::new(),
        Err(message) => message,
    }
}

fn compile_mir(source: &str) -> mir::Module {
    compile(source).expect("source should compile")
}

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    execute(&compile_mir(source), function).map_err(|error| error.to_string())
}

// --- Section 19.1-6: types ----------------------------------------------------

#[test]
fn the_official_enum_and_struct_are_accessible_with_every_case() {
    let cases = [
        ("NotFound", 0),
        ("PermissionDenied", 1),
        ("AlreadyExists", 2),
        ("InvalidPath", 3),
        ("InvalidUtf8", 4),
        ("NotFile", 5),
        ("NotDirectory", 6),
        ("LimitExceeded", 7),
        ("Other", 8),
    ];
    for (case, _) in cases {
        let source = format!(
            "using aster.io;\n\
             public int Main() {{\n\
                 IOError error = IOError {{ Kind: IOErrorKind.{case}, OsCode: 0 }};\n\
                 switch (error.Kind) {{ case {case}: return 1; default: return 0; }}\n\
             }}"
        );
        assert_eq!(
            run(&source, "Main"),
            Ok(ExecutionValue::Int(1)),
            "case {case}"
        );
    }
}

#[test]
fn io_error_fields_are_readable_and_hold_the_constructed_values() {
    let source = "using aster.io;\n\
        public int Main() {\n\
            IOError error = IOError { Kind: IOErrorKind.PermissionDenied, OsCode: 13 };\n\
            if (error.OsCode != 13) { return 1; }\n\
            switch (error.Kind) { case PermissionDenied: return 0; default: return 2; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

#[test]
fn io_error_is_copied_by_value_not_by_reference() {
    let source = "using aster.io;\n\
        public int Main() {\n\
            IOError first = IOError { Kind: IOErrorKind.NotFound, OsCode: 1 };\n\
            IOError second = first;\n\
            second.OsCode = 99;\n\
            if (first.OsCode != 1) { return 1; }\n\
            if (second.OsCode != 99) { return 2; }\n\
            return 0;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

// --- Section 19.7-12: Result<T, IOError> --------------------------------------

const FAIL_AND_CONVERT: &str = "using aster.core;\nusing aster.io;\n\
    public Result<int, IOError> Fail() {\n\
        IOError error = IOError { Kind: IOErrorKind.NotFound, OsCode: 2 };\n\
        return Result<int, IOError>.Error(error);\n\
    }\n\
    public Result<string, IOError> Convert() {\n\
        int value = Fail()?;\n\
        return Result<string, IOError>.Ok(value.ToString());\n\
    }";

#[test]
fn error_of_io_error_preserves_kind_and_os_code() {
    let source = format!(
        "{FAIL_AND_CONVERT}\n\
         public int Main() {{\n\
             switch (Fail()) {{\n\
                 case Ok(v): return -1;\n\
                 case Error(err): switch (err.Kind) {{ case NotFound: return err.OsCode; default: return -2; }}\n\
             }}\n\
         }}"
    );
    assert_eq!(run(&source, "Main"), Ok(ExecutionValue::Int(2)));
}

#[test]
fn postfix_try_propagates_io_error_into_a_result_with_a_different_success_type() {
    let source = format!(
        "{FAIL_AND_CONVERT}\n\
         public string Main() {{\n\
             switch (Convert()) {{\n\
                 case Ok(text): return text;\n\
                 case Error(err): switch (err.Kind) {{ case NotFound: return \"caught:\" + err.OsCode.ToString(); default: return \"other\"; }}\n\
             }}\n\
         }}"
    );
    assert_eq!(
        run(&source, "Main"),
        Ok(ExecutionValue::String("caught:2".to_owned()))
    );
}

#[test]
fn ok_path_of_result_int_io_error_returns_the_success_payload() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> Succeed() { return Result<int, IOError>.Ok(42); }\n\
        public int Main() {\n\
            switch (Succeed()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn result_io_error_string_reports_the_error_case_directly() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<IOError, string> Try() { return Result<IOError, string>.Error(\"denied\"); }\n\
        public string Main() {\n\
            switch (Try()) {\n\
                case Ok(err): return \"ok\";\n\
                case Error(message): return message;\n\
            }\n\
        }";
    assert_eq!(
        run(source, "Main"),
        Ok(ExecutionValue::String("denied".to_owned()))
    );
}

#[test]
fn fail_is_evaluated_exactly_once_by_postfix_try() {
    let source = "using aster.core;\nusing aster.io;\n\
        public class Counter {\n\
            public int calls;\n\
            public Result<int, IOError> Fail() {\n\
                calls = calls + 1;\n\
                return Result<int, IOError>.Error(IOError { Kind: IOErrorKind.Other, OsCode: 0 });\n\
            }\n\
        }\n\
        public Result<int, IOError> Wrap(Counter counter) {\n\
            int value = counter.Fail()?;\n\
            return Result<int, IOError>.Ok(value);\n\
        }\n\
        public int Main() {\n\
            Counter counter = new Counter();\n\
            Wrap(counter);\n\
            return counter.calls;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1)));
}

// --- Section 19.13-19: layout (behavioral, no hard-coded offsets) -------------

#[test]
fn io_error_survives_array_storage_and_indexing() {
    let source = "using aster.io;\n\
        public int Main() {\n\
            IOError[] errors = [\n\
                IOError { Kind: IOErrorKind.NotFound, OsCode: 1 },\n\
                IOError { Kind: IOErrorKind.AlreadyExists, OsCode: 2 }\n\
            ];\n\
            if (errors[0].OsCode != 1) { return 1; }\n\
            if (errors[1].OsCode != 2) { return 2; }\n\
            switch (errors[1].Kind) { case AlreadyExists: return 0; default: return 3; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(0)));
}

#[test]
fn io_error_survives_class_field_storage() {
    let source = "using aster.io;\n\
        public class Holder {\n\
            public IOError last;\n\
            public Holder() { last = IOError { Kind: IOErrorKind.Other, OsCode: 0 }; }\n\
        }\n\
        public int Main() {\n\
            Holder holder = new Holder();\n\
            holder.last = IOError { Kind: IOErrorKind.LimitExceeded, OsCode: 7 };\n\
            switch (holder.last.Kind) { case LimitExceeded: return holder.last.OsCode; default: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(7)));
}

/// Bug fix regression: a field read on a struct-valued *call result* used
/// directly as a `switch` discriminant (`GetError().Kind`, no intermediate
/// variable) used to panic the compiler itself (`mir_lowering/places.rs`'s
/// `place()` only handled a `Symbol`/`Member`/`Index` base, never a plain
/// call), reachable from `aster check` alone, before any execution. This is
/// exactly the shape `List<IOError>.Get(index).Kind` needs (see
/// `io_error_works_inside_a_generic_function_and_a_list`, which exercises
/// the identical path through a real `List<T>`).
#[test]
fn a_struct_field_read_directly_on_a_call_result_no_longer_panics() {
    let source = "using aster.io;\n\
        public IOError GetError() { return IOError { Kind: IOErrorKind.NotDirectory, OsCode: 20 }; }\n\
        public int Main() {\n\
            switch (GetError().Kind) { case NotDirectory: return 1; default: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1)));
}

#[test]
fn a_struct_field_read_on_a_call_result_evaluates_the_call_exactly_once() {
    let source = "using aster.io;\n\
        public class Counter {\n\
            public int calls;\n\
            public IOError GetError() { calls = calls + 1; return IOError { Kind: IOErrorKind.NotFound, OsCode: 9 }; }\n\
        }\n\
        public int Main() {\n\
            Counter counter = new Counter();\n\
            int code = counter.GetError().OsCode;\n\
            return counter.calls * 1000 + code;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(1009)));
}

/// Follow-up correction: `place()`'s fix (materialize any non-place base
/// into a temporary) made *reads* work, but as an unwanted side effect also
/// let `GetError().OsCode = 5;` compile silently -- the write would land in
/// that temporary and vanish with no observable effect. Assigning to a
/// field of a value with no real backing location must be a compile-time
/// error instead. The corrected layer is `semantic/general/expressions.rs`'s
/// `assignment()` (`is_assignable_place`), not `places.rs`: MIR lowering
/// should not have to guess whether a write was supposed to be observable.
#[test]
fn assigning_to_a_field_of_a_call_result_struct_is_a_compile_error() {
    let source = "using aster.io;\n\
        public IOError GetError() { return IOError { Kind: IOErrorKind.NotFound, OsCode: 1 }; }\n\
        public int Main() {\n\
            GetError().OsCode = 5;\n\
            return 0;\n\
        }";
    let errors = compile_errors(source);
    assert!(
        errors.contains("cannot assign to a field of a temporary value"),
        "expected the stable temporary-assignment diagnostic, got {errors}"
    );
}

#[test]
fn aster_check_rejects_the_invalid_assignment_without_panicking() {
    let source = "using aster.io;\n\
        public IOError GetError() { return IOError { Kind: IOErrorKind.NotFound, OsCode: 1 }; }\n\
        public int Main() {\n\
            GetError().OsCode = 5;\n\
            return 0;\n\
        }";
    // `compile()` is exactly what `aster check` runs (semantic + HIR/MIR
    // lowering, no execution); a panic here would unwind the test process
    // instead of returning `Err`, so a plain `Err` result is itself proof
    // there was no panic.
    let result = compile(source);
    assert!(result.is_err());
    assert!(compile_errors(source).contains("cannot assign to a field of a temporary value"));
}

#[test]
fn aster_run_rejects_the_same_invalid_assignment_without_panicking() {
    let source = "using aster.io;\n\
        public IOError GetError() { return IOError { Kind: IOErrorKind.NotFound, OsCode: 1 }; }\n\
        public int Main() {\n\
            GetError().OsCode = 5;\n\
            return 0;\n\
        }";
    // `aster run` calls the same `compile_project` used here before it ever
    // reaches `execute`; a compile-time `Err` (not a panic) is exactly what
    // it would see, so the invalid program never reaches the JIT at all.
    let error = compile(source).expect_err("must be rejected before execution");
    assert!(error.contains("cannot assign to a field of a temporary value"));
}

#[test]
fn assigning_to_a_field_of_a_struct_variable_still_works() {
    let source = "using aster.io;\n\
        public IOError GetError() { return IOError { Kind: IOErrorKind.NotFound, OsCode: 1 }; }\n\
        public int Main() {\n\
            IOError error = GetError();\n\
            error.OsCode = 5;\n\
            return error.OsCode;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(5)));
}

#[test]
fn assigning_to_a_field_of_a_class_returned_by_a_call_still_works() {
    // Unlike a struct, a class is a shared reference: mutating a field
    // through it is observable even when the reference itself came directly
    // from a call, as long as the SAME underlying object is read back.
    let source = "public class Holder { public int value; }\n\
        public class Registry { private Holder holder = new Holder(); public Holder Get() { return holder; } }\n\
        public int Main() {\n\
            Registry registry = new Registry();\n\
            registry.Get().value = 42;\n\
            return registry.Get().value;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn assigning_through_an_addressable_field_and_index_base_still_works() {
    let source = "using aster.io;\n\
        public class Container {\n\
            public IOError Error;\n\
            public Container() { Error = IOError { Kind: IOErrorKind.Other, OsCode: 0 }; }\n\
        }\n\
        public int Main() {\n\
            Container container = new Container();\n\
            container.Error.OsCode = 5;\n\
            IOError[] errors = [IOError { Kind: IOErrorKind.NotFound, OsCode: 1 }];\n\
            int index = 0;\n\
            errors[index].OsCode = 9;\n\
            return container.Error.OsCode + errors[0].OsCode;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(14)));
}

#[test]
fn ordinary_variable_and_this_field_assignments_are_unaffected() {
    let source = "public class Counter {\n\
        public int value;\n\
        public void Bump() { value = value + 1; }\n\
    }\n\
    public int Main() {\n\
        int total = 0;\n\
        total = total + 1;\n\
        Counter counter = new Counter();\n\
        counter.Bump();\n\
        counter.Bump();\n\
        return total + counter.value;\n\
    }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(3)));
}

#[test]
fn io_error_works_inside_a_generic_function_and_a_list() {
    let source = "using aster.io;\n\
        public T First<T>(T a, T b) { return a; }\n\
        public int Main() {\n\
            IOError picked = First(\n\
                IOError { Kind: IOErrorKind.NotDirectory, OsCode: 20 },\n\
                IOError { Kind: IOErrorKind.NotFound, OsCode: 2 }\n\
            );\n\
            List<IOError> errors = new List<IOError>();\n\
            errors.Add(picked);\n\
            switch (errors.Get(0).Kind) { case NotDirectory: return errors.Get(0).OsCode; default: return -1; }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(20)));
}

#[test]
fn io_error_survives_nested_option_and_result_composition() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Option<IOError> Maybe() { return Option<IOError>.Some(IOError { Kind: IOErrorKind.InvalidUtf8, OsCode: 5 }); }\n\
        public Result<Option<IOError>, string> Wrapped() { return Result<Option<IOError>, string>.Ok(Maybe()); }\n\
        public int Main() {\n\
            switch (Wrapped()) {\n\
                case Ok(inner):\n\
                    switch (inner) {\n\
                        case Some(error): switch (error.Kind) { case InvalidUtf8: return error.OsCode; default: return -1; }\n\
                        case None: return -2;\n\
                    }\n\
                case Error(message): return -3;\n\
            }\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(5)));
}

// --- Section 19.20-23: nominal identity ---------------------------------------

#[test]
fn a_user_namespace_lookalike_does_not_collide_without_using_aster_io() {
    // No `using aster.io;` here at all: this file's own `IOErrorKind`/
    // `IOError` are ordinary, unrelated user types declared in the root
    // (global) namespace, matching this single-file project's own root.
    let source = "public enum IOErrorKind { NotFound }\n\
        public struct IOError { public IOErrorKind Kind; public int OsCode; }\n\
        public int Main() {\n\
            IOError error = IOError { Kind: IOErrorKind.NotFound, OsCode: 99 };\n\
            return error.OsCode;\n\
        }";
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(99)));
}

#[test]
fn a_root_declaration_colliding_with_the_official_io_error_export_is_rejected() {
    let errors = compile_errors(
        "using aster.io;\n\
         public struct IOError { public int OsCode; }\n\
         public int Main() { return 0; }",
    );
    assert!(
        errors.contains("conflicts with the official export"),
        "expected a conflict diagnostic, got {errors}"
    );
    let errors = compile_errors(
        "using aster.io;\n\
         public enum IOErrorKind { NotFound }\n\
         public int Main() { return 0; }",
    );
    assert!(
        errors.contains("conflicts with the official export"),
        "expected a conflict diagnostic, got {errors}"
    );
}

#[test]
fn structurally_identical_fake_types_stay_distinct_across_files() {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("aster-io-error-ns-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::write(
        root.join("Aster.toml"),
        "[application]\nentry = \"app.Main\"\n",
    )
    .expect("write manifest");
    let app_dir = root.join("app");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(
        app_dir.join("main.aster"),
        "namespace app;\n\
         using aster.core;\n\
         using aster.io;\n\
         using fake;\n\
         public int Main() {\n\
             IOError real = IOError { Kind: IOErrorKind.NotFound, OsCode: 1 };\n\
             FakeIOError fake_error = FakeIOError { Kind: FakeIOErrorKind.NotFound, OsCode: 2 };\n\
             if (real.OsCode == fake_error.OsCode) { return -1; }\n\
             return real.OsCode + fake_error.OsCode;\n\
         }",
    )
    .expect("write main.aster");
    let fake_dir = root.join("fake");
    std::fs::create_dir_all(&fake_dir).expect("create fake namespace dir");
    std::fs::write(
        fake_dir.join("fake.aster"),
        "namespace fake;\n\
         public enum FakeIOErrorKind { NotFound }\n\
         public struct FakeIOError { public FakeIOErrorKind Kind; public int OsCode; }",
    )
    .expect("write fake namespace file");
    let compilation = compile_project(&app_dir.join("main.aster"))
        .map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_dir_all(&root).ok();
    let compilation = compilation.expect("project with two distinct error types should compile");
    assert_eq!(
        execute(&compilation.compilation.mir, "Main"),
        Ok(ExecutionValue::Int(3))
    );
}

fn stats_for(source: &str) -> MemoryStats {
    let module = compile_mir(source);
    let (_, stats) = execute_with_stats(&module, "Main").expect("source should execute");
    stats
}

#[test]
fn thousands_of_constructions_and_copies_allocate_nothing() {
    let source = "using aster.io;\n\
        public int Main() {\n\
            int total = 0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                IOError first = IOError { Kind: IOErrorKind.NotFound, OsCode: i };\n\
                IOError second = first;\n\
                second.OsCode = second.OsCode + 1;\n\
                total = total + second.OsCode - first.OsCode;\n\
            }\n\
            return total;\n\
        }";
    let stats = stats_for(source);
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.total_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
    assert_eq!(run(source, "Main"), Ok(ExecutionValue::Int(5000)));
}

#[test]
fn thousands_of_result_propagations_allocate_nothing() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> MaybeFail(int i) {\n\
            if (i % 2 == 0) { return Result<int, IOError>.Error(IOError { Kind: IOErrorKind.NotFound, OsCode: i }); }\n\
            return Result<int, IOError>.Ok(i);\n\
        }\n\
        public Result<int, IOError> Wrap(int i) {\n\
            int value = MaybeFail(i)?;\n\
            return Result<int, IOError>.Ok(value * 2);\n\
        }\n\
        public int Main() {\n\
            int total = 0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                switch (Wrap(i)) { case Ok(v): total = total + v; case Error(e): total = total + e.OsCode; }\n\
            }\n\
            return total;\n\
        }";
    let stats = stats_for(source);
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.total_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
}

// --- Section 19.35-40: regressions --------------------------------------------

#[test]
fn result_and_option_propagation_and_plain_enums_structs_generics_still_work() {
    assert_eq!(
        run(
            "using aster.core;\n\
             public Result<int, string> Parse() { return Result<int, string>.Ok(9); }\n\
             public Result<int, string> Calc() { int v = Parse()?; return Result<int, string>.Ok(v + 1); }\n\
             public int Main() { switch (Calc()) { case Ok(v): return v; case Error(e): return -1; } }",
            "Main"
        ),
        Ok(ExecutionValue::Int(10))
    );
    assert_eq!(
        run(
            "using aster.core;\n\
             public Option<int> Parse() { return Option<int>.Some(4); }\n\
             public Option<int> Calc() { int v = Parse()?; return Option<int>.Some(v); }\n\
             public int Main() { switch (Calc()) { case Some(v): return v; case None: return -1; } }",
            "Main"
        ),
        Ok(ExecutionValue::Int(4))
    );
    assert_eq!(
        run(
            "public enum Color { Red, Green, Blue }\n\
             public int Main() { switch (Color.Green) { case Green: return 1; default: return 0; } }",
            "Main"
        ),
        Ok(ExecutionValue::Int(1))
    );
    assert_eq!(
        run(
            "public struct P { public int x; public int y; }\n\
             public int Main() { P p = P { x: 1, y: 2 }; return p.x + p.y; }",
            "Main"
        ),
        Ok(ExecutionValue::Int(3))
    );
}

// --- Section 14: MIR adulteration ---------------------------------------------

fn find_first_matching(
    module: &mut mir::Module,
    matches: impl Fn(&mir::Instruction) -> bool,
) -> &mut mir::Instruction {
    module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches(instruction))
        .expect("a matching instruction exists")
}

const CONSTRUCT_IO_ERROR: &str = "using aster.io;\n\
    public int Main() {\n\
        IOError error = IOError { Kind: IOErrorKind.NotFound, OsCode: 2 };\n\
        return error.OsCode;\n\
    }";

#[test]
fn adulterated_mir_rejects_a_struct_with_a_missing_field() {
    let mut module = compile_mir(CONSTRUCT_IO_ERROR);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Aggregate(_),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign {
        value:
            mir::Rvalue {
                kind: mir::RvalueKind::Aggregate(fields),
                ..
            },
        ..
    } = instruction
    else {
        unreachable!();
    };
    fields.pop();
    let error = execute(&module, "Main").expect_err("a struct missing a field must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_a_struct_with_an_extra_field() {
    let mut module = compile_mir(CONSTRUCT_IO_ERROR);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Aggregate(_),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign {
        value:
            mir::Rvalue {
                kind: mir::RvalueKind::Aggregate(fields),
                ..
            },
        ..
    } = instruction
    else {
        unreachable!();
    };
    let extra = fields[0].clone();
    fields.push(extra);
    let error =
        execute(&module, "Main").expect_err("a struct with an extra field must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_io_error_kind_field_with_the_wrong_type() {
    let mut module = compile_mir(CONSTRUCT_IO_ERROR);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Aggregate(_),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign {
        value:
            mir::Rvalue {
                kind: mir::RvalueKind::Aggregate(fields),
                ..
            },
        ..
    } = instruction
    else {
        unreachable!();
    };
    // `Kind` (an enum) is field 0; retype its operand to `int` (still an
    // internally-consistent constant, so only a struct-shape check catches
    // this).
    fields[0].value.type_ = mir::Type::Int;
    fields[0].value.kind = mir::OperandKind::Constant(mir::Constant::Integer("0".to_owned()));
    let error =
        execute(&module, "Main").expect_err("IOError.Kind with the wrong type must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_io_error_os_code_field_with_the_wrong_type() {
    let mut module = compile_mir(CONSTRUCT_IO_ERROR);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Aggregate(_),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign {
        value:
            mir::Rvalue {
                kind: mir::RvalueKind::Aggregate(fields),
                ..
            },
        ..
    } = instruction
    else {
        unreachable!();
    };
    fields[1].value.type_ = mir::Type::Bool;
    fields[1].value.kind = mir::OperandKind::Constant(mir::Constant::Boolean(true));
    let error =
        execute(&module, "Main").expect_err("IOError.OsCode with the wrong type must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_an_unknown_struct_symbol() {
    let mut module = compile_mir(CONSTRUCT_IO_ERROR);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Aggregate(_),
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign { value, .. } = instruction else {
        unreachable!();
    };
    value.type_ = mir::Type::User(mir::SymbolId(u32::MAX));
    let error = execute(&module, "Main").expect_err("an unknown struct symbol must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_io_error_kind_construct_with_an_unknown_case() {
    let mut module = compile_mir(CONSTRUCT_IO_ERROR);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::EnumConstruct { .. },
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign {
        value:
            mir::Rvalue {
                kind: mir::RvalueKind::EnumConstruct { case, .. },
                ..
            },
        ..
    } = instruction
    else {
        unreachable!();
    };
    *case = mir::SymbolId(u32::MAX);
    let error = execute(&module, "Main").expect_err("an unknown enum case symbol must be rejected");
    assert!(!error.to_string().is_empty());
}

#[test]
fn adulterated_mir_rejects_io_error_kind_construct_with_a_divergent_tag() {
    let mut module = compile_mir(CONSTRUCT_IO_ERROR);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::EnumConstruct { .. },
                    ..
                },
                ..
            }
        )
    });
    let mir::Instruction::Assign {
        value:
            mir::Rvalue {
                kind: mir::RvalueKind::EnumConstruct { tag, .. },
                ..
            },
        ..
    } = instruction
    else {
        unreachable!();
    };
    *tag = tag.wrapping_add(1);
    let error = execute(&module, "Main").expect_err("a divergent enum tag must be rejected");
    assert!(!error.to_string().is_empty());
}
