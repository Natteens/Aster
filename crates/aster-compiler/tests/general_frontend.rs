use aster_compiler::compile;
use aster_syntax::{Item, Member, Visibility};

fn assert_valid(source: &str) {
    if let Err(diagnostics) = compile(source) {
        panic!("expected valid source, got: {diagnostics:#?}");
    }
}

fn assert_error(source: &str, expected: &str) {
    let diagnostics = compile(source).expect_err("source should be rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(expected)),
        "expected a diagnostic containing {expected:?}, got {diagnostics:#?}"
    );
}

#[test]
fn preserves_call_diagnostic_order_spans_and_count() {
    let source = "public int Use() { return receiver.Missing(argument); }";
    let diagnostics = compile(source).expect_err("source should be rejected");
    let argument_start = source.find("argument").expect("argument is present");
    let receiver_start = source.find("receiver").expect("receiver is present");

    assert_eq!(
        diagnostics.len(),
        2,
        "unexpected diagnostics: {diagnostics:#?}"
    );
    assert_eq!(diagnostics[0].message, "unknown name `argument`");
    assert_eq!(
        diagnostics[0].span,
        aster_diagnostics::Span::new(argument_start, argument_start + "argument".len())
    );
    assert_eq!(diagnostics[1].message, "unknown name `receiver`");
    assert_eq!(
        diagnostics[1].span,
        aster_diagnostics::Span::new(receiver_start, receiver_start + "receiver".len())
    );
}

#[test]
fn accepts_class_with_method() {
    assert_valid("public class Calculator { public int Add(int a, int b) { return a + b; } }");
}

#[test]
fn static_classes_are_non_instantiable_method_containers() {
    assert_valid(
        "public static class Math { public static int One() { return 1; } } public int Run() { return Math.One(); }",
    );
    assert_error(
        "public static class Bad { public int Value() { return 1; } }",
        "members of a static class must be static",
    );
    assert_error(
        "public static class Bad { public static int One() { return 1; } } public void Run() { Bad value = new Bad(); }",
        "static class `Bad` cannot be instantiated",
    );
}

#[test]
fn accepts_struct_with_fields() {
    assert_valid("public struct Position { public float x; public float y; }");
}

#[test]
fn accepts_interface_with_methods() {
    assert_valid("public interface IDamageable { void Damage(int amount); bool IsAlive(); }");
}

#[test]
fn accepts_module_function() {
    assert_valid("public int Add(int a, int b) { return a + b; }");
}

#[test]
fn infers_var_and_accepts_mutable_assignment() {
    assert_valid(
        r#"public void Work() { var name = "Natte"; name = "Aster"; int score = 0; score += 1; }"#,
    );
}

#[test]
fn accepts_initialized_constant() {
    assert_valid("public int Score() { const int MaxScore = 100; return MaxScore; }");
}

#[test]
fn rejects_assignment_to_constant() {
    assert_error(
        "public void Work() { const int MaxScore = 100; MaxScore = 200; }",
        "cannot assign to constant",
    );
}

#[test]
fn rejects_var_without_initializer() {
    assert_error(
        "public void Work() { var value; }",
        "`var` requires an initializer",
    );
}

#[test]
fn rejects_constant_without_initializer() {
    assert_error(
        "public void Work() { const int MaxScore; }",
        "constants require an initializer",
    );
}

#[test]
fn accepts_compatible_return() {
    assert_valid("public bool IsAlive() { return true; }");
}

#[test]
fn rejects_incompatible_return() {
    assert_error(
        "public int Score() { return false; }",
        "expected `int`, found `bool`",
    );
}

#[test]
fn rejects_value_from_void_function() {
    assert_error("public void Work() { return 1; }", "cannot return a value");
}

#[test]
fn rejects_missing_non_void_return() {
    assert_error("public int Score() { int value = 1; }", "must return `int`");
}

#[test]
fn validates_function_argument_count() {
    assert_error(
        "public int Add(int a, int b) { return a + b; } public int Use() { return Add(1); }",
        "expected 2 argument(s), found 1",
    );
}

#[test]
fn validates_function_argument_types() {
    assert_error(
        r#"public int Add(int a, int b) { return a + b; } public int Use() { return Add(1, "two"); }"#,
        "expected `int`, found `string`",
    );
}

#[test]
fn records_default_visibility() {
    let compilation = compile("class Sample { int value; void Reset() {} }").expect("valid source");
    let Item::Class(class) = &compilation.module.items[0] else {
        panic!("expected class");
    };
    assert_eq!(class.visibility, Visibility::Internal);
    let Member::Field(field) = &class.members[0] else {
        panic!("expected field");
    };
    assert_eq!(field.visibility, Visibility::Private);
}

#[test]
fn rejects_multiple_visibility_modifiers() {
    assert_error(
        "public private class Sample {}",
        "only one visibility modifier",
    );
}

#[test]
fn rejects_private_module_function() {
    assert_error(
        "private void Hidden() {}",
        "`private` is not valid on a namespace-level",
    );
}

#[test]
fn rejects_protected_module_declaration() {
    assert_error(
        "protected class Hidden {}",
        "`protected` is not valid on a namespace-level",
    );
}

#[test]
fn explains_protected_member_limitation() {
    assert_error(
        "public class Entity { protected int id; }",
        "future inheritance or extension model",
    );
}

#[test]
fn rejects_interface_field() {
    assert_error(
        "public interface Invalid { int state; }",
        "interfaces cannot declare instance fields",
    );
}

#[test]
fn rejects_duplicate_field() {
    assert_error(
        "public struct Pair { public int value; public int value; }",
        "duplicate member `value`",
    );
}

#[test]
fn rejects_duplicate_local_name() {
    assert_error(
        "public void Work() { int value = 1; int value = 2; }",
        "duplicate name `value`",
    );
}

#[test]
fn accepts_standard_logging_calls() {
    assert_valid(
        r#"public void Report() { Log("normal"); Log.Warning("warning"); Log.Error("error"); }"#,
    );
}

#[test]
fn logging_requires_string() {
    assert_error(
        "public void Report() { Log(42); }",
        "expected `string`, found `int`",
    );
}

#[test]
fn rejects_unavailable_logging_methods() {
    assert_error(
        r#"public void Report() { Log.Info("info"); }"#,
        "`Log.Info` does not exist",
    );
    assert_error(
        r#"public void Report() { Log.Debug("debug"); }"#,
        "`Log.Debug` does not exist",
    );
}

#[test]
fn accepts_user_declared_field_type() {
    assert_valid("public class User {} public class Session { private User user; }");
}

#[test]
fn rejects_unknown_field_type() {
    assert_error(
        "public class Session { private Missing user; }",
        "unknown type `Missing`",
    );
}

#[test]
fn accepts_increment_decrement_on_mutable_numeric_variables() {
    assert_valid("public int Count() { int i = 0; i++; ++i; i--; --i; return i; }");
    assert_valid("public void Measure() { double d = 0.5; d++; }");
}

#[test]
fn rejects_increment_of_constant() {
    assert_error(
        "public void Test() { const int Max = 1; Max++; }",
        "cannot apply `++` to constant `Max`",
    );
}

#[test]
fn rejects_increment_of_literal_and_temporary() {
    assert_error(
        "public void Test() { 5++; }",
        "the operand of `++` must be a mutable variable",
    );
    assert_error(
        "public void Test() { int a = 1; int b = 2; (a + b)--; }",
        "the operand of `--` must be a mutable variable",
    );
}

#[test]
fn rejects_increment_of_non_numeric_type() {
    assert_error(
        "public void Test() { bool flag = true; flag++; }",
        "`++` is not valid for `bool`",
    );
    assert_error(
        r#"public void Test() { string s = "x"; s--; }"#,
        "`--` is not valid for `string`",
    );
}

#[test]
fn accepts_conditional_expression() {
    assert_valid("public int Choose(bool enabled) { return enabled ? 10 : 20; }");
    assert_valid("public double Mix(bool flag) { return flag ? 1 : 2.5; }");
}

#[test]
fn rejects_non_boolean_conditional_condition() {
    assert_error(
        "public int Test() { int i = 1; return i ? 1 : 2; }",
        "`?:` condition must be `bool`, found `int`",
    );
}

#[test]
fn rejects_incompatible_conditional_branches() {
    assert_error(
        r#"public void Test(bool flag) { var value = flag ? 1 : "text"; }"#,
        "`?:` branches have incompatible types `int` and `string`",
    );
}

#[test]
fn types_integer_literals_by_width() {
    assert_valid("public long Wide() { return 4000000000; }");
    assert_error(
        "public int Narrow() { return 4000000000; }",
        "constant value `4000000000` does not fit `int`",
    );
    assert_error(
        "public long Huge() { return 9223372036854775808; }",
        "out of range for `long`",
    );
}

#[test]
fn accepts_literal_suffixes() {
    assert_valid("public long A() { return 10L; }");
    assert_valid("public float B() { return 2f; }");
    assert_valid("public double C() { return 2.5d; }");
}

#[test]
fn accepts_safe_implicit_conversions_and_rejects_narrowing() {
    assert_valid(
        "public double Widen() { short a = 1; float b = a; int c = 2; double d = c; return b + d; }",
    );
    assert_error(
        "public float LosePrecision() { long value = 16777217L; return value; }",
        "expected `float`, found `long`",
    );
    assert_error(
        "public int Narrow() { long a = 1; return a; }",
        "expected `int`, found `long`",
    );
    assert_error(
        "public float Narrow() { double a = 1.5d; return a; }",
        "expected `float`, found `double`",
    );
}

#[test]
fn validates_explicit_casts() {
    assert_valid("public int Truncate() { return (int)9.7d; }");
    assert_valid("public char Letter() { return (char)65; }");
    assert_valid("public int Scalar() { return (int)'x'; }");
    assert_error(
        "public char Bad() { return (char)2.5f; }",
        "cannot cast `float` to `char`",
    );
    assert_error(
        r#"public int Bad() { return (int)"text"; }"#,
        "cannot cast `string` to `int`",
    );
    assert_error(
        "public int Bad() { bool flag = true; return (int)flag; }",
        "cannot cast `bool` to `int`",
    );
}

#[test]
fn rejects_non_constant_initializer_for_constants() {
    assert_error(
        "public int Compute() { return 1; } public void Test() { const int Value = Compute(); }",
        "constant initializers must be compile-time constant expressions",
    );
    assert_error(
        "public void Test() { int variable = 1; const int Value = variable; }",
        "constant initializers must be compile-time constant expressions",
    );
}

#[test]
fn reports_overflow_in_constant_expressions() {
    assert_error(
        "public void Test() { const int Value = 2147483647 + 1; }",
        "constant expression overflows `int`",
    );
}

#[test]
fn reports_division_by_zero_in_constant_expressions() {
    assert_error(
        "public void Test() { const int Value = 10 / 0; }",
        "constant expression divides by zero",
    );
    assert_error(
        "public void Test() { const int Value = 10 % 0; }",
        "constant expression divides by zero",
    );
}

#[test]
fn accepts_constant_expressions_with_references_and_casts() {
    assert_valid(
        "public int Test() { const int Base = 10; const int Doubled = Base * 2; const long Wide = (long)Doubled; return (int)Wide; }",
    );
}

#[test]
fn rejects_duplicate_function_declarations() {
    assert_error(
        "public int Value() { return 1; } public int Value() { return 2; }",
        "duplicate overload `Value`",
    );
}

#[test]
fn types_unsigned_literals_by_width() {
    assert_valid("public uint Small() { return 5u; }");
    assert_valid("public ulong Wide() { return 5000000000u; }");
    assert_valid("public ulong Suffixed() { return 5ul; }");
    assert_error(
        "public ulong Huge() { return 18446744073709551616u; }",
        "out of range for `ulong`",
    );
}

#[test]
fn accepts_small_integer_declarations_with_fitting_constants() {
    assert_valid("public sbyte A() { sbyte v = -128; return v; }");
    assert_valid("public byte B() { byte v = 255; return v; }");
    assert_valid("public short C() { short v = -32768; return v; }");
    assert_valid("public ushort D() { ushort v = 65535; return v; }");
    assert_valid("public int E() { const short Half = 100; int v = Half; return v; }");
}

#[test]
fn rejects_out_of_range_constants_for_small_types() {
    assert_error(
        "public void Test() { byte v = 256; }",
        "constant value `256` does not fit `byte`",
    );
    assert_error(
        "public void Test() { sbyte v = -129; }",
        "constant value `-129` does not fit `sbyte`",
    );
    assert_error(
        "public void Test() { int source = 1; byte v = source; }",
        "expected `byte`, found `int`",
    );
}

#[test]
fn mixing_ulong_with_signed_types_requires_a_cast() {
    assert_error(
        "public void Test() { ulong a = 1ul; int b = 2; bool c = a > b; }",
        "`ulong` and `int` have no implicit common type",
    );
    assert_valid("public bool Test() { ulong a = 1ul; int b = 2; return a > (ulong)b; }");
}

#[test]
fn rejects_negating_unsigned_values() {
    assert_error(
        "public void Test() { uint value = 5u; uint negated = -value; }",
        "cannot negate a value of the unsigned type `uint`",
    );
}

#[test]
fn unsigned_widening_is_implicit_and_sign_changes_are_not() {
    assert_valid("public ulong Widen() { byte small = 200; uint middle = small; return middle; }");
    assert_error(
        "public void Test() { int signed = 1; uint unsigned = signed; }",
        "expected `uint`, found `int`",
    );
    assert_error(
        "public void Test() { ulong wide = 1ul; long narrow = wide; }",
        "expected `long`, found `ulong`",
    );
}

#[test]
fn decimal_is_deliberately_deferred_with_a_precise_diagnostic() {
    assert_error(
        "public decimal Money() { decimal price = 10.50m; return price; }",
        "`decimal` is reserved but not supported",
    );
    assert_error(
        "public decimal Sum(decimal a, decimal b) { return a + b; }",
        "`decimal` is reserved but not supported",
    );
}
