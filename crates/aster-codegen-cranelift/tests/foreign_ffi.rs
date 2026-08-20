#![allow(unsafe_code)]

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use aster_codegen_cranelift::{
    ExecutionValue, ForeignRegistry, ForeignSignature, ForeignType, execute,
    execute_symbol_with_foreign_registry, execute_with_foreign_registry,
    execute_with_foreign_registry_and_stats, validate,
};
use aster_compiler::{compile, compile_project, select_application_entry};

extern "C" fn add(left: i32, right: i32, out: *mut i32) -> i32 {
    if out.is_null() {
        return 90;
    }
    unsafe { out.write(left.wrapping_add(right)) };
    0
}

extern "C" fn add_one(value: i32, out: *mut i32) -> i32 {
    if out.is_null() {
        return 90;
    }
    unsafe { out.write(value.wrapping_add(1)) };
    0
}

extern "C" fn status(_value: i32, _out: *mut i32) -> i32 {
    37
}

extern "C" fn invalid_bool(out: *mut i8) -> i32 {
    if !out.is_null() {
        unsafe { out.write(2) };
    }
    0
}

extern "C" fn invalid_char(out: *mut u32) -> i32 {
    if !out.is_null() {
        unsafe { out.write(0xD800) };
    }
    0
}

extern "C" fn invalid_large_char(out: *mut u32) -> i32 {
    if !out.is_null() {
        unsafe { out.write(0x11_0000) };
    }
    0
}

extern "C" fn failed_after_writing(out: *mut i32) -> i32 {
    if !out.is_null() {
        unsafe { out.write(i32::MIN) };
    }
    -91
}

extern "C" fn long_identity_for_mismatch(value: i64, out: *mut i64) -> i32 {
    if out.is_null() {
        return 92;
    }
    unsafe { out.write(value) };
    0
}

extern "C" fn metadata_wrapper(
    _boolean: i8,
    _signed_byte: i8,
    _short: i16,
    _integer: i32,
    _long: i64,
    _float: f32,
    _character: u32,
    _out: *mut i32,
) -> i32 {
    0
}

macro_rules! result_wrapper {
    ($name:ident, $rust:ty, $value:expr) => {
        extern "C" fn $name(out: *mut $rust) -> i32 {
            if out.is_null() {
                return 92;
            }
            unsafe { out.write($value) };
            0
        }
    };
}

result_wrapper!(return_bool, i8, 1);
result_wrapper!(return_sbyte, i8, -101);
result_wrapper!(return_byte, u8, 201);
result_wrapper!(return_short, i16, -30_001);
result_wrapper!(return_ushort, u16, 60_001);
result_wrapper!(return_char, u32, '🙂' as u32);
result_wrapper!(return_int, i32, -2_000_000_001);
result_wrapper!(return_uint, u32, 4_000_000_001);
result_wrapper!(return_long, i64, -9_000_000_001);
result_wrapper!(return_ulong, u64, 18_000_000_001);
result_wrapper!(return_float, f32, -0.0);
result_wrapper!(return_double, f64, 1.25);
result_wrapper!(return_sbyte_min, i8, i8::MIN);
result_wrapper!(return_byte_max, u8, u8::MAX);
result_wrapper!(return_short_min, i16, i16::MIN);
result_wrapper!(return_ushort_max, u16, u16::MAX);
result_wrapper!(return_char_max, u32, 0x10_FFFF);
result_wrapper!(return_int_min, i32, i32::MIN);
result_wrapper!(return_uint_max, u32, u32::MAX);
result_wrapper!(return_long_min, i64, i64::MIN);
result_wrapper!(return_ulong_max, u64, u64::MAX);
result_wrapper!(return_float_nan, f32, f32::NAN);
result_wrapper!(return_float_subnormal, f32, f32::from_bits(1));
result_wrapper!(return_double_infinity, f64, f64::INFINITY);

static OBSERVED: AtomicI32 = AtomicI32::new(0);
static STATUS: AtomicI32 = AtomicI32::new(0);
static ORDER: AtomicI32 = AtomicI32::new(0);
static COMBINE_COUNT: AtomicI32 = AtomicI32::new(0);
static LATER_OBSERVED: AtomicI32 = AtomicI32::new(0);
static TEMP_ID: AtomicI32 = AtomicI32::new(0);
static FOREIGN_BOOL_RESULT: AtomicU32 = AtomicU32::new(0);
static FOREIGN_CHAR_RESULT: AtomicU32 = AtomicU32::new(0);

extern "C" fn configured_bool(out: *mut u8) -> i32 {
    if out.is_null() {
        return 89;
    }
    let Ok(value) = u8::try_from(FOREIGN_BOOL_RESULT.load(Ordering::SeqCst)) else {
        return 88;
    };
    unsafe { out.write(value) };
    0
}

extern "C" fn configured_char(out: *mut u32) -> i32 {
    if out.is_null() {
        return 90;
    }
    unsafe { out.write(FOREIGN_CHAR_RESULT.load(Ordering::SeqCst)) };
    0
}

extern "C" fn observe(value: i32) -> i32 {
    OBSERVED.store(value, Ordering::SeqCst);
    0
}

extern "C" fn observe_later(value: i32) -> i32 {
    LATER_OBSERVED.store(value, Ordering::SeqCst);
    0
}

extern "C" fn configured_status(_out: *mut i32) -> i32 {
    STATUS.load(Ordering::SeqCst)
}

extern "C" fn failing_void() -> i32 {
    -73
}

extern "C" fn panic_proof_wrapper(out: *mut i32) -> i32 {
    let outcome = std::panic::catch_unwind(|| -> i32 { panic!("host failure") });
    match outcome {
        Ok(value) if !out.is_null() => {
            unsafe { out.write(value) };
            0
        }
        Ok(_) => 86,
        Err(_) => 87,
    }
}

extern "C" fn next(value: i32, out: *mut i32) -> i32 {
    let previous = ORDER.fetch_add(1, Ordering::SeqCst);
    if previous != value - 1 || out.is_null() {
        return 81;
    }
    unsafe { out.write(value) };
    0
}

extern "C" fn next_fail(_value: i32, _out: *mut i32) -> i32 {
    85
}

extern "C" fn combine(left: i32, right: i32, out: *mut i32) -> i32 {
    COMBINE_COUNT.fetch_add(1, Ordering::SeqCst);
    if out.is_null() {
        return 82;
    }
    unsafe { out.write(left * 10 + right) };
    0
}

extern "C" fn ieee_arguments(nan: f32, infinity: f64, negative_zero: f64, out: *mut i32) -> i32 {
    if out.is_null() {
        return 88;
    }
    let valid = nan.is_nan()
        && infinity == f64::INFINITY
        && negative_zero.to_bits() == (-0.0_f64).to_bits();
    unsafe { out.write(i32::from(valid)) };
    0
}

#[allow(clippy::too_many_arguments)]
extern "C" fn all_arguments(
    boolean: i8,
    signed_byte: i8,
    byte: u8,
    short: i16,
    unsigned_short: u16,
    character: u32,
    integer: i32,
    unsigned_integer: u32,
    long: i64,
    unsigned_long: u64,
    float: f32,
    double: f64,
    out: *mut i32,
) -> i32 {
    let valid = boolean == 1
        && signed_byte == i8::MIN
        && byte == u8::MAX
        && short == i16::MIN
        && unsigned_short == u16::MAX
        && character == u32::from('🙂')
        && integer == i32::MIN
        && unsigned_integer == u32::MAX
        && long == i64::MIN
        && unsigned_long == u64::MAX
        && float.to_bits() == (-0.0_f32).to_bits()
        && double.to_bits() == 1.25_f64.to_bits();
    if out.is_null() {
        return 91;
    }
    unsafe { out.write(i32::from(valid)) };
    0
}

fn registry(
    name: &str,
    parameters: impl Into<Vec<ForeignType>>,
    result: ForeignType,
    address: *const (),
) -> ForeignRegistry {
    let mut registry = ForeignRegistry::new();
    let signature = ForeignSignature::new(parameters, result).unwrap();
    unsafe { registry.register(name, signature, address).unwrap() };
    registry
}

#[test]
fn executes_registered_scalar_and_void_wrappers() {
    let compilation = compile(
        r"
        public unsafe foreign int NativeAdd(int left, int right);
        public unsafe foreign void Observe(int value);
        public int Run() {
            unsafe {
                Observe(9);
                return NativeAdd(20, 22);
            }
        }
        ",
    )
    .unwrap();
    let mut registry = ForeignRegistry::new();
    unsafe {
        registry
            .register(
                "NativeAdd",
                ForeignSignature::new([ForeignType::Int, ForeignType::Int], ForeignType::Int)
                    .unwrap(),
                add as *const (),
            )
            .unwrap();
        registry
            .register(
                "Observe",
                ForeignSignature::new([ForeignType::Int], ForeignType::Void).unwrap(),
                observe as *const (),
            )
            .unwrap();
    }
    assert_eq!(
        execute_with_foreign_registry(&compilation.mir, "Run", &registry),
        Ok(ExecutionValue::Int(42))
    );
    assert_eq!(OBSERVED.load(Ordering::SeqCst), 9);
}

#[test]
fn repeated_scalar_ffi_has_no_aster_dynamic_allocation() {
    let compilation = compile(
        r"
        public unsafe foreign int NativeAdd(int left, int right);
        public int Run() {
            int total = 0;
            for (int i = 0; i < 100000; i++) {
                unsafe { total = NativeAdd(total, 1); }
            }
            return total;
        }
        ",
    )
    .unwrap();
    let bindings = registry(
        "NativeAdd",
        [ForeignType::Int, ForeignType::Int],
        ForeignType::Int,
        add as *const (),
    );
    let (value, stats) =
        execute_with_foreign_registry_and_stats(&compilation.mir, "Run", &bindings).unwrap();
    assert_eq!(value, ExecutionValue::Int(100_000));
    assert_eq!(stats.total_allocations, 0);
    assert_eq!(stats.requested_bytes, 0);
}

#[test]
fn missing_and_mismatched_bindings_fail_before_execution() {
    let compilation = compile(
        r"
        public unsafe foreign int Native(int value);
        public int Run() { unsafe { return Native(1); } }
        ",
    )
    .unwrap();
    let missing = execute(&compilation.mir, "Run").unwrap_err();
    assert!(missing.message().contains("missing foreign binding"));
    let wrong = registry(
        "Native",
        [ForeignType::Long],
        ForeignType::Long,
        long_identity_for_mismatch as *const (),
    );
    let mismatch = execute_with_foreign_registry(&compilation.mir, "Run", &wrong).unwrap_err();
    assert!(mismatch.message().contains("signature mismatch"));
}

#[test]
fn preparation_resolves_every_module_foreign_declaration_before_user_code() {
    let compilation = compile(
        r"
        public unsafe foreign int Unused();
        public int Run() { return 42; }
        ",
    )
    .unwrap();
    let error = execute(&compilation.mir, "Run").unwrap_err();
    assert!(
        error
            .message()
            .contains("missing foreign binding for `Unused`")
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn registry_metadata_is_structural_not_machine_width_based() {
    let signature = ForeignSignature::new(
        [
            ForeignType::Bool,
            ForeignType::SByte,
            ForeignType::Short,
            ForeignType::Int,
            ForeignType::Long,
            ForeignType::Float,
            ForeignType::Char,
        ],
        ForeignType::Int,
    )
    .unwrap();
    let bindings = registry(
        "Native",
        signature.parameters(),
        signature.result(),
        metadata_wrapper as *const (),
    );

    let mismatches = [
        ForeignSignature::new(signature.parameters(), ForeignType::Void).unwrap(),
        ForeignSignature::new(
            [
                ForeignType::Byte,
                ForeignType::SByte,
                ForeignType::Short,
                ForeignType::Int,
                ForeignType::Long,
                ForeignType::Float,
                ForeignType::Char,
            ],
            ForeignType::Int,
        )
        .unwrap(),
        ForeignSignature::new(
            [
                ForeignType::Bool,
                ForeignType::Byte,
                ForeignType::Short,
                ForeignType::Int,
                ForeignType::Long,
                ForeignType::Float,
                ForeignType::Char,
            ],
            ForeignType::Int,
        )
        .unwrap(),
        ForeignSignature::new(
            [
                ForeignType::Bool,
                ForeignType::SByte,
                ForeignType::UShort,
                ForeignType::Int,
                ForeignType::Long,
                ForeignType::Float,
                ForeignType::Char,
            ],
            ForeignType::Int,
        )
        .unwrap(),
        ForeignSignature::new(
            [
                ForeignType::Bool,
                ForeignType::SByte,
                ForeignType::Short,
                ForeignType::UInt,
                ForeignType::Long,
                ForeignType::Float,
                ForeignType::Char,
            ],
            ForeignType::Int,
        )
        .unwrap(),
        ForeignSignature::new(
            [
                ForeignType::Bool,
                ForeignType::SByte,
                ForeignType::Short,
                ForeignType::Int,
                ForeignType::ULong,
                ForeignType::Float,
                ForeignType::Char,
            ],
            ForeignType::Int,
        )
        .unwrap(),
        ForeignSignature::new(
            [
                ForeignType::Bool,
                ForeignType::SByte,
                ForeignType::Short,
                ForeignType::Int,
                ForeignType::Long,
                ForeignType::Double,
                ForeignType::Char,
            ],
            ForeignType::Int,
        )
        .unwrap(),
        ForeignSignature::new(
            [
                ForeignType::Bool,
                ForeignType::SByte,
                ForeignType::Short,
                ForeignType::Int,
                ForeignType::Long,
                ForeignType::Float,
                ForeignType::UInt,
            ],
            ForeignType::Int,
        )
        .unwrap(),
        ForeignSignature::new(
            [
                ForeignType::Bool,
                ForeignType::SByte,
                ForeignType::Short,
                ForeignType::Int,
                ForeignType::Long,
                ForeignType::Float,
            ],
            ForeignType::Int,
        )
        .unwrap(),
    ];
    for mismatch in mismatches {
        assert!(
            bindings.resolve_address("Native", &mismatch).is_err(),
            "distinct descriptors must not match by ABI width: {mismatch:?}"
        );
    }
}

#[test]
fn nonzero_status_and_invalid_scalar_results_are_controlled_errors() {
    let status_module = compile(
        "public unsafe foreign int Native(int value); public int Run() { unsafe { return Native(1); } }",
    )
    .unwrap();
    let status_registry = registry(
        "Native",
        [ForeignType::Int],
        ForeignType::Int,
        status as *const (),
    );
    let error =
        execute_with_foreign_registry(&status_module.mir, "Run", &status_registry).unwrap_err();
    assert!(error.message().contains("native status 37"));

    let bool_module = compile(
        "public unsafe foreign bool Native(); public bool Run() { unsafe { return Native(); } }",
    )
    .unwrap();
    let bool_registry = registry("Native", [], ForeignType::Bool, invalid_bool as *const ());
    assert!(
        execute_with_foreign_registry(&bool_module.mir, "Run", &bool_registry)
            .unwrap_err()
            .message()
            .contains("must be 0 or 1")
    );

    let char_module = compile(
        "public unsafe foreign char Native(); public char Run() { unsafe { return Native(); } }",
    )
    .unwrap();
    let char_registry = registry("Native", [], ForeignType::Char, invalid_char as *const ());
    assert!(
        execute_with_foreign_registry(&char_module.mir, "Run", &char_registry)
            .unwrap_err()
            .message()
            .contains("Unicode scalar")
    );
    let large_char_registry = registry(
        "Native",
        [],
        ForeignType::Char,
        invalid_large_char as *const (),
    );
    assert!(
        execute_with_foreign_registry(&char_module.mir, "Run", &large_char_registry)
            .unwrap_err()
            .message()
            .contains("Unicode scalar")
    );

    for native_status in [1, -1, i32::MAX, i32::MIN] {
        STATUS.store(native_status, Ordering::SeqCst);
        let status_registry = registry(
            "Native",
            [],
            ForeignType::Int,
            configured_status as *const (),
        );
        let module = compile(
            "public unsafe foreign int Native(); public int Run() { unsafe { return Native(); } }",
        )
        .unwrap();
        let error =
            execute_with_foreign_registry(&module.mir, "Run", &status_registry).unwrap_err();
        assert!(error.message().contains(&native_status.to_string()));
    }

    let void_module =
        compile("public unsafe foreign void Native(); public void Run() { unsafe { Native(); } }")
            .unwrap();
    let void_registry = registry("Native", [], ForeignType::Void, failing_void as *const ());
    assert!(
        execute_with_foreign_registry(&void_module.mir, "Run", &void_registry)
            .unwrap_err()
            .message()
            .contains("-73")
    );
}

#[test]
fn validates_all_bool_and_char_boundary_results_before_publication() {
    let bool_module = compile(
        "public unsafe foreign bool Native(); public bool Run() { unsafe { return Native(); } }",
    )
    .unwrap();
    let bool_registry = registry(
        "Native",
        [],
        ForeignType::Bool,
        configured_bool as *const (),
    );
    for (raw, expected) in [(0, Some(false)), (1, Some(true)), (2, None), (255, None)] {
        FOREIGN_BOOL_RESULT.store(raw, Ordering::SeqCst);
        let result = execute_with_foreign_registry(&bool_module.mir, "Run", &bool_registry);
        match expected {
            Some(value) => assert_eq!(result, Ok(ExecutionValue::Bool(value))),
            None => assert!(
                result.unwrap_err().message().contains("must be 0 or 1"),
                "raw bool {raw} must remain invalid"
            ),
        }
    }

    let char_module = compile(
        "public unsafe foreign char Native(); public char Run() { unsafe { return Native(); } }",
    )
    .unwrap();
    let char_registry = registry(
        "Native",
        [],
        ForeignType::Char,
        configured_char as *const (),
    );
    for raw in [0, 0x7F, 0x80, 0xD7FF, 0xE000, 0x10_FFFF] {
        FOREIGN_CHAR_RESULT.store(raw, Ordering::SeqCst);
        assert!(
            matches!(
                execute_with_foreign_registry(&char_module.mir, "Run", &char_registry),
                Ok(ExecutionValue::Char(value)) if value as u32 == raw
            ),
            "valid scalar U+{raw:04X} must cross intact"
        );
    }
    for raw in [0xD800, 0xDFFF, 0x11_0000, u32::MAX] {
        FOREIGN_CHAR_RESULT.store(raw, Ordering::SeqCst);
        let error =
            execute_with_foreign_registry(&char_module.mir, "Run", &char_registry).unwrap_err();
        assert!(
            error.message().contains("Unicode scalar"),
            "invalid scalar U+{raw:04X} must be rejected"
        );
    }

    let failed_module = compile(
        "public unsafe foreign int Native(); public int Run() { unsafe { return Native(); } }",
    )
    .unwrap();
    let failed_registry = registry(
        "Native",
        [],
        ForeignType::Int,
        failed_after_writing as *const (),
    );
    assert!(
        execute_with_foreign_registry(&failed_module.mir, "Run", &failed_registry)
            .unwrap_err()
            .message()
            .contains("-91")
    );
}

#[test]
fn host_can_contain_a_rust_panic_and_return_status() {
    let module = compile(
        "public unsafe foreign int Native(); public int Run() { unsafe { return Native(); } }",
    )
    .unwrap();
    let bindings = registry(
        "Native",
        [],
        ForeignType::Int,
        panic_proof_wrapper as *const (),
    );
    let error = execute_with_foreign_registry(&module.mir, "Run", &bindings).unwrap_err();
    assert!(error.message().contains("87"));
}

#[test]
fn first_error_and_argument_failure_prevent_later_native_calls() {
    LATER_OBSERVED.store(0, Ordering::SeqCst);
    let foreign_failure = compile(
        r"
        public unsafe foreign int Fail(int value);
        public unsafe foreign void Observe(int value);
        public int Run() {
            unsafe { Fail(1); Observe(9); }
            return 0;
        }
        ",
    )
    .unwrap();
    let mut bindings = ForeignRegistry::new();
    unsafe {
        bindings
            .register(
                "Fail",
                ForeignSignature::new([ForeignType::Int], ForeignType::Int).unwrap(),
                status as *const (),
            )
            .unwrap();
        bindings
            .register(
                "Observe",
                ForeignSignature::new([ForeignType::Int], ForeignType::Void).unwrap(),
                observe_later as *const (),
            )
            .unwrap();
    }
    let error = execute_with_foreign_registry(&foreign_failure.mir, "Run", &bindings).unwrap_err();
    assert!(error.message().contains("native status 37"));
    assert_eq!(LATER_OBSERVED.load(Ordering::SeqCst), 0);

    let argument_failure = compile(
        r"
        public unsafe foreign void Observe(int value);
        public int Run() {
            int[] values = new int[0];
            unsafe { Observe(values[0]); }
            return 0;
        }
        ",
    )
    .unwrap();
    let observe_binding = registry(
        "Observe",
        [ForeignType::Int],
        ForeignType::Void,
        observe_later as *const (),
    );
    let error =
        execute_with_foreign_registry(&argument_failure.mir, "Run", &observe_binding).unwrap_err();
    assert!(error.message().contains("array index"));
    assert_eq!(LATER_OBSERVED.load(Ordering::SeqCst), 0);
}

#[test]
fn evaluates_foreign_arguments_once_from_left_to_right() {
    ORDER.store(0, Ordering::SeqCst);
    COMBINE_COUNT.store(0, Ordering::SeqCst);
    let compilation = compile(
        r"
        public unsafe foreign int Next(int value);
        public unsafe foreign int Combine(int left, int right);
        public int Run() {
            unsafe { return Combine(Next(1), Next(2)); }
        }
        ",
    )
    .unwrap();
    let mut bindings = ForeignRegistry::new();
    unsafe {
        bindings
            .register(
                "Next",
                ForeignSignature::new([ForeignType::Int], ForeignType::Int).unwrap(),
                next as *const (),
            )
            .unwrap();
        bindings
            .register(
                "Combine",
                ForeignSignature::new([ForeignType::Int, ForeignType::Int], ForeignType::Int)
                    .unwrap(),
                combine as *const (),
            )
            .unwrap();
    }
    assert_eq!(
        execute_with_foreign_registry(&compilation.mir, "Run", &bindings),
        Ok(ExecutionValue::Int(12))
    );
    assert_eq!(ORDER.load(Ordering::SeqCst), 2);
    assert_eq!(COMBINE_COUNT.load(Ordering::SeqCst), 1);

    COMBINE_COUNT.store(0, Ordering::SeqCst);
    let failing = compile(
        r"
        public unsafe foreign int NextFail(int value);
        public unsafe foreign int Combine(int left, int right);
        public int Run() { unsafe { return Combine(NextFail(1), 2); } }
        ",
    )
    .unwrap();
    let mut failing_bindings = ForeignRegistry::new();
    unsafe {
        failing_bindings
            .register(
                "NextFail",
                ForeignSignature::new([ForeignType::Int], ForeignType::Int).unwrap(),
                next_fail as *const (),
            )
            .unwrap();
        failing_bindings
            .register(
                "Combine",
                ForeignSignature::new([ForeignType::Int, ForeignType::Int], ForeignType::Int)
                    .unwrap(),
                combine as *const (),
            )
            .unwrap();
    }
    assert!(
        execute_with_foreign_registry(&failing.mir, "Run", &failing_bindings)
            .unwrap_err()
            .message()
            .contains("85")
    );
    assert_eq!(COMBINE_COUNT.load(Ordering::SeqCst), 0);
}

#[test]
fn resolves_overloads_by_linked_identity_and_exact_signature() {
    extern "C" fn int_identity(value: i32, out: *mut i32) -> i32 {
        if out.is_null() {
            return 83;
        }
        unsafe { out.write(value) };
        0
    }
    extern "C" fn long_identity(value: i64, out: *mut i64) -> i32 {
        if out.is_null() {
            return 84;
        }
        unsafe { out.write(value) };
        0
    }
    let compilation = compile(
        r"
        public unsafe foreign int Native(int value);
        public unsafe foreign long Native(long value);
        public long Run() { unsafe { return Native(7) + Native(9l); } }
        ",
    )
    .unwrap();
    let mut bindings = ForeignRegistry::new();
    unsafe {
        bindings
            .register(
                "Native",
                ForeignSignature::new([ForeignType::Int], ForeignType::Int).unwrap(),
                int_identity as *const (),
            )
            .unwrap();
        bindings
            .register(
                "Native",
                ForeignSignature::new([ForeignType::Long], ForeignType::Long).unwrap(),
                long_identity as *const (),
            )
            .unwrap();
    }
    assert_eq!(
        execute_with_foreign_registry(&compilation.mir, "Run", &bindings),
        Ok(ExecutionValue::Long(16))
    );
}

#[test]
fn independent_registries_bind_the_same_declaration_differently() {
    extern "C" fn one(out: *mut i32) -> i32 {
        unsafe { out.write(1) };
        0
    }
    extern "C" fn two(out: *mut i32) -> i32 {
        unsafe { out.write(2) };
        0
    }
    let compilation = compile(
        "public unsafe foreign int Native(); public int Run() { unsafe { return Native(); } }",
    )
    .unwrap();
    let first = registry("Native", [], ForeignType::Int, one as *const ());
    let second = registry("Native", [], ForeignType::Int, two as *const ());
    assert_eq!(
        execute_with_foreign_registry(&compilation.mir, "Run", &first),
        Ok(ExecutionValue::Int(1))
    );
    assert_eq!(
        execute_with_foreign_registry(&compilation.mir, "Run", &second),
        Ok(ExecutionValue::Int(2))
    );
}

#[test]
fn ordinary_async_resumes_with_its_execution_scoped_foreign_binding() {
    let compilation = compile(
        r"
        public unsafe foreign int Native(int value);
        public int One() { return 1; }
        public async Task<int> Work() {
            int value = await Task.Run(One);
            unsafe { return Native(value); }
        }
        public int Run() { return Work().Wait(); }
        ",
    )
    .unwrap();
    let bindings = registry(
        "Native",
        [ForeignType::Int],
        ForeignType::Int,
        add_one as *const (),
    );
    assert_eq!(
        execute_with_foreign_registry(&compilation.mir, "Run", &bindings),
        Ok(ExecutionValue::Int(2))
    );
}

#[test]
fn transports_every_accepted_scalar_argument_without_reinterpretation() {
    let compilation = compile(
        r"
        public unsafe foreign int Native(
            bool a, sbyte b, byte c, short d, ushort e, char f,
            int g, uint h, long i, ulong j, float k, double l);
        public int Run() {
            unsafe {
                return Native(
                    true, (sbyte)-128, (byte)255, (short)-32768, (ushort)65535, '🙂',
                    (int)-2147483648, 4294967295u, (-9223372036854775807l - 1l),
                    18446744073709551615ul,
                    -0f, 1.25d);
            }
        }
        ",
    )
    .unwrap();
    let registry = registry(
        "Native",
        [
            ForeignType::Bool,
            ForeignType::SByte,
            ForeignType::Byte,
            ForeignType::Short,
            ForeignType::UShort,
            ForeignType::Char,
            ForeignType::Int,
            ForeignType::UInt,
            ForeignType::Long,
            ForeignType::ULong,
            ForeignType::Float,
            ForeignType::Double,
        ],
        ForeignType::Int,
        all_arguments as *const (),
    );
    assert_eq!(
        execute_with_foreign_registry(&compilation.mir, "Run", &registry),
        Ok(ExecutionValue::Int(1))
    );
}

#[test]
fn transports_ieee_special_arguments_without_normalization() {
    let compilation = compile(
        r"
        public unsafe foreign int Native(float nan, double infinity, double negativeZero);
        public int Run() {
            float floatZero = 0f;
            float nan = floatZero / floatZero;
            double zero = 0d;
            double infinity = 1d / zero;
            unsafe { return Native(nan, infinity, -0d); }
        }
        ",
    )
    .unwrap();
    let bindings = registry(
        "Native",
        [ForeignType::Float, ForeignType::Double, ForeignType::Double],
        ForeignType::Int,
        ieee_arguments as *const (),
    );
    assert_eq!(
        execute_with_foreign_registry(&compilation.mir, "Run", &bindings),
        Ok(ExecutionValue::Int(1))
    );
}

#[test]
fn backend_rejects_adulterated_foreign_mir_before_codegen() {
    let mut compilation = compile(
        "public unsafe foreign int Native(int value); public int Run() { unsafe { return Native(1); } }",
    )
    .unwrap();
    let call = compilation
        .mir
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find_map(|instruction| match instruction {
            aster_compiler::mir::Instruction::ForeignCall { arguments, .. } => Some(arguments),
            _ => None,
        })
        .unwrap();
    call[0].type_ = aster_compiler::mir::Type::Long;
    let registry = registry(
        "Native",
        [ForeignType::Int],
        ForeignType::Int,
        add as *const (),
    );
    let error = execute_with_foreign_registry(&compilation.mir, "Run", &registry).unwrap_err();
    assert!(error.message().contains("invalid signature"));
}

#[test]
fn backend_rejects_every_foreign_call_shape_before_codegen() {
    let base = compile(
        "public unsafe foreign int Native(int value); public int Run() { unsafe { return Native(1); } }",
    )
    .unwrap()
    .mir;

    let mut undeclared = base.clone();
    undeclared.foreign_functions.clear();
    assert!(
        validate(&undeclared)
            .unwrap_err()
            .message()
            .contains("undeclared foreign")
    );

    let mut wrong_arity = base.clone();
    if let Some(aster_compiler::mir::Instruction::ForeignCall { arguments, .. }) = wrong_arity
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                aster_compiler::mir::Instruction::ForeignCall { .. }
            )
        })
    {
        arguments.clear();
    }
    assert!(
        validate(&wrong_arity)
            .unwrap_err()
            .message()
            .contains("invalid signature")
    );

    let mut wrong_result = base.clone();
    if let Some(aster_compiler::mir::Instruction::ForeignCall { return_type, .. }) = wrong_result
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                aster_compiler::mir::Instruction::ForeignCall { .. }
            )
        })
    {
        *return_type = aster_compiler::mir::Type::Long;
    }
    assert!(
        validate(&wrong_result)
            .unwrap_err()
            .message()
            .contains("invalid signature")
    );

    let mut missing_destination = base.clone();
    if let Some(aster_compiler::mir::Instruction::ForeignCall { destination, .. }) =
        missing_destination
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    aster_compiler::mir::Instruction::ForeignCall { .. }
                )
            })
    {
        *destination = None;
    }
    assert!(
        validate(&missing_destination)
            .unwrap_err()
            .message()
            .contains("result destination")
    );

    let mut unsupported_declaration = base;
    unsupported_declaration.foreign_functions[0].parameters[0] = aster_compiler::mir::Type::String;
    assert!(
        validate(&unsupported_declaration)
            .unwrap_err()
            .message()
            .contains("invalid scalar ABI")
    );

    let mut duplicate_binding = compile(
        "public unsafe foreign int Native(int value); public int Run() { unsafe { return Native(1); } }",
    )
    .unwrap()
    .mir;
    let mut duplicate = duplicate_binding.foreign_functions[0].clone();
    duplicate.symbol = aster_compiler::mir::SymbolId(u32::MAX);
    duplicate_binding.foreign_functions.push(duplicate);
    assert!(
        validate(&duplicate_binding)
            .unwrap_err()
            .message()
            .contains("duplicate foreign binding identity")
    );
}

#[test]
fn transports_every_accepted_scalar_result() {
    let cases = [
        (
            "bool",
            ForeignType::Bool,
            return_bool as *const (),
            ExecutionValue::Bool(true),
        ),
        (
            "sbyte",
            ForeignType::SByte,
            return_sbyte as *const (),
            ExecutionValue::SByte(-101),
        ),
        (
            "byte",
            ForeignType::Byte,
            return_byte as *const (),
            ExecutionValue::Byte(201),
        ),
        (
            "short",
            ForeignType::Short,
            return_short as *const (),
            ExecutionValue::Short(-30_001),
        ),
        (
            "ushort",
            ForeignType::UShort,
            return_ushort as *const (),
            ExecutionValue::UShort(60_001),
        ),
        (
            "char",
            ForeignType::Char,
            return_char as *const (),
            ExecutionValue::Char('🙂'),
        ),
        (
            "int",
            ForeignType::Int,
            return_int as *const (),
            ExecutionValue::Int(-2_000_000_001),
        ),
        (
            "uint",
            ForeignType::UInt,
            return_uint as *const (),
            ExecutionValue::UInt(4_000_000_001),
        ),
        (
            "long",
            ForeignType::Long,
            return_long as *const (),
            ExecutionValue::Long(-9_000_000_001),
        ),
        (
            "ulong",
            ForeignType::ULong,
            return_ulong as *const (),
            ExecutionValue::ULong(18_000_000_001),
        ),
        (
            "float",
            ForeignType::Float,
            return_float as *const (),
            ExecutionValue::Float(-0.0),
        ),
        (
            "double",
            ForeignType::Double,
            return_double as *const (),
            ExecutionValue::Double(1.25),
        ),
    ];
    for (name, type_, address, expected) in cases {
        let source = format!(
            "public unsafe foreign {name} Native(); public {name} Run() {{ unsafe {{ return Native(); }} }}"
        );
        let compilation = compile(&source).unwrap();
        let registry = registry("Native", [], type_, address);
        let actual = execute_with_foreign_registry(&compilation.mir, "Run", &registry).unwrap();
        if name == "float" {
            let ExecutionValue::Float(value) = actual else {
                panic!("wrong float result")
            };
            assert_eq!(value.to_bits(), (-0.0_f32).to_bits());
        } else {
            assert_eq!(actual, expected, "wrong {name} result");
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn preserves_integer_edges_and_ieee_special_results() {
    let integer_cases = [
        (
            "sbyte",
            ForeignType::SByte,
            return_sbyte_min as *const (),
            ExecutionValue::SByte(i8::MIN),
        ),
        (
            "byte",
            ForeignType::Byte,
            return_byte_max as *const (),
            ExecutionValue::Byte(u8::MAX),
        ),
        (
            "short",
            ForeignType::Short,
            return_short_min as *const (),
            ExecutionValue::Short(i16::MIN),
        ),
        (
            "ushort",
            ForeignType::UShort,
            return_ushort_max as *const (),
            ExecutionValue::UShort(u16::MAX),
        ),
        (
            "char",
            ForeignType::Char,
            return_char_max as *const (),
            ExecutionValue::Char('\u{10ffff}'),
        ),
        (
            "int",
            ForeignType::Int,
            return_int_min as *const (),
            ExecutionValue::Int(i32::MIN),
        ),
        (
            "uint",
            ForeignType::UInt,
            return_uint_max as *const (),
            ExecutionValue::UInt(u32::MAX),
        ),
        (
            "long",
            ForeignType::Long,
            return_long_min as *const (),
            ExecutionValue::Long(i64::MIN),
        ),
        (
            "ulong",
            ForeignType::ULong,
            return_ulong_max as *const (),
            ExecutionValue::ULong(u64::MAX),
        ),
    ];
    for (name, type_, address, expected) in integer_cases {
        let source = format!(
            "public unsafe foreign {name} Native(); public {name} Run() {{ unsafe {{ return Native(); }} }}"
        );
        let compilation = compile(&source).unwrap();
        let bindings = registry("Native", [], type_, address);
        assert_eq!(
            execute_with_foreign_registry(&compilation.mir, "Run", &bindings),
            Ok(expected),
            "wrong {name} edge result"
        );
    }

    let float_module = compile(
        "public unsafe foreign float Native(); public float Run() { unsafe { return Native(); } }",
    )
    .unwrap();
    let float_registry = registry(
        "Native",
        [],
        ForeignType::Float,
        return_float_nan as *const (),
    );
    let ExecutionValue::Float(float) =
        execute_with_foreign_registry(&float_module.mir, "Run", &float_registry).unwrap()
    else {
        panic!("wrong float result type");
    };
    assert!(float.is_nan());

    let subnormal_registry = registry(
        "Native",
        [],
        ForeignType::Float,
        return_float_subnormal as *const (),
    );
    let ExecutionValue::Float(subnormal) =
        execute_with_foreign_registry(&float_module.mir, "Run", &subnormal_registry).unwrap()
    else {
        panic!("wrong subnormal result type");
    };
    assert_eq!(subnormal.to_bits(), 1);

    let double_module = compile(
        "public unsafe foreign double Native(); public double Run() { unsafe { return Native(); } }",
    )
    .unwrap();
    let double_registry = registry(
        "Native",
        [],
        ForeignType::Double,
        return_double_infinity as *const (),
    );
    assert_eq!(
        execute_with_foreign_registry(&double_module.mir, "Run", &double_registry),
        Ok(ExecutionValue::Double(f64::INFINITY))
    );
}

#[test]
fn uses_linked_identity_across_files_and_a_path_dependency() {
    let root = std::env::temp_dir().join(format!(
        "aster-foreign-package-{}-{}",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::SeqCst)
    ));
    let app = root.join("app");
    let native = root.join("native");
    std::fs::create_dir_all(app.join("app")).unwrap();
    std::fs::create_dir_all(native.join("native")).unwrap();
    std::fs::write(
        app.join("Aster.toml"),
        "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n\n[dependencies]\nnative = { path = \"../native\" }\n",
    )
    .unwrap();
    std::fs::write(native.join("Aster.toml"), "[package]\nname = \"native\"\n").unwrap();
    std::fs::write(
        app.join("app/main.aster"),
        "namespace app; using native; public static class Program { public static int Main() { unsafe { return Native(); } } }",
    )
    .unwrap();
    std::fs::write(
        native.join("native/api.aster"),
        "namespace native; public unsafe foreign int Native();",
    )
    .unwrap();
    let root_file = app.join("app/main.aster");
    let project = compile_project(&root_file).unwrap();
    let entry = select_application_entry(&project, &root_file).unwrap();
    let declaration = &project.compilation.mir.foreign_functions[0];
    assert!(declaration.name.starts_with("native::"));
    let registry = registry(
        &declaration.name,
        [],
        ForeignType::Int,
        return_int as *const (),
    );
    let result =
        execute_symbol_with_foreign_registry(&project.compilation.mir, entry.symbol, &registry);
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(result, Ok(ExecutionValue::Int(-2_000_000_001)));
}
