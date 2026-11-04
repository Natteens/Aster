//! Native execution evidence for the bitwise and shift operators.
//!
//! Expected values come from an independent two's-complement emulation written
//! from `docs/compiler/bitwise-shift-decision.md`, never from the compiler.
//! Each generated program compares every result with a literal and returns the
//! 1-based index of the first mismatch (0 when everything agrees), so one JIT
//! compilation covers a whole matrix.

#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::decimal_bitwise_operands,
    clippy::identity_op
)] // The expected values are written as plain arithmetic on purpose.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_stats};
use aster_compiler::{compile, compile_project, mir};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Ty {
    name: &'static str,
    bits: u32,
    signed: bool,
}

const TYPES: [Ty; 8] = [
    Ty {
        name: "sbyte",
        bits: 8,
        signed: true,
    },
    Ty {
        name: "byte",
        bits: 8,
        signed: false,
    },
    Ty {
        name: "short",
        bits: 16,
        signed: true,
    },
    Ty {
        name: "ushort",
        bits: 16,
        signed: false,
    },
    Ty {
        name: "int",
        bits: 32,
        signed: true,
    },
    Ty {
        name: "uint",
        bits: 32,
        signed: false,
    },
    Ty {
        name: "long",
        bits: 64,
        signed: true,
    },
    Ty {
        name: "ulong",
        bits: 64,
        signed: false,
    },
];

const INT: Ty = TYPES[4];
const COUNTS: [i128; 10] = [0, 1, 31, 32, 33, 63, 64, 65, -1, -32];

impl Ty {
    fn range(self) -> (i128, i128) {
        if self.signed {
            (
                -(1_i128 << (self.bits - 1)),
                (1_i128 << (self.bits - 1)) - 1,
            )
        } else {
            (0, (1_i128 << self.bits) - 1)
        }
    }

    fn holds(self, from: Ty) -> bool {
        self == from
            || match (from.signed, self.signed) {
                (true, true) | (false, false) => self.bits >= from.bits,
                (false, true) => self.bits > from.bits,
                (true, false) => false,
            }
    }

    /// Unary promotion: small integers become `int`.
    fn unary(self) -> Ty {
        if self.bits < 32 { INT } else { self }
    }

    /// Binary promotion, `None` when there is no common type.
    fn common(self, other: Ty) -> Option<Ty> {
        if self.bits < 32 && other.bits < 32 {
            return Some(INT);
        }
        [TYPES[4], TYPES[5], TYPES[6], TYPES[7]]
            .into_iter()
            .find(|candidate| candidate.holds(self) && candidate.holds(other))
    }

    /// Operand values worth testing: the extremes, a bit pattern, and `-1`.
    fn samples(self) -> Vec<i128> {
        let (minimum, maximum) = self.range();
        let mut values = vec![minimum, maximum, 90];
        if self.signed {
            values.push(-1);
        }
        values
    }

    fn wrap(self, value: i128) -> i128 {
        let modulus = 1_i128 << self.bits;
        let wrapped = value.rem_euclid(modulus);
        if self.signed && wrapped >= modulus / 2 {
            wrapped - modulus
        } else {
            wrapped
        }
    }

    /// An ASTER expression of exactly this type with the given value.
    fn literal(self, value: i128) -> String {
        let (minimum, _) = self.range();
        match self.name {
            "sbyte" | "byte" | "short" | "ushort" => format!("(({})({value}))", self.name),
            "int" if value == minimum => "(-2147483647 - 1)".to_owned(),
            "int" => format!("({value})"),
            "uint" => format!("{value}U"),
            "long" if value == minimum => "(-9223372036854775807L - 1L)".to_owned(),
            "long" => format!("({value}L)"),
            _ => format!("{value}UL"),
        }
    }
}

fn logical(operator: &str, left: i128, right: i128) -> i128 {
    match operator {
        "&" => left & right,
        "|" => left | right,
        _ => left ^ right,
    }
}

/// `<<`, `>>` and `>>>` on a value of the promoted type `promoted`.
fn shift(operator: &str, value: i128, count: i128, promoted: Ty) -> i128 {
    let bits = i128::from(promoted.bits);
    let count = count.rem_euclid(bits);
    let pattern = value.rem_euclid(1_i128 << promoted.bits);
    promoted.wrap(match operator {
        "<<" => pattern << count,
        ">>" if promoted.signed => value >> count,
        _ => pattern >> count,
    })
}

#[derive(Default)]
struct Program {
    source: String,
    checks: Vec<(String, String)>,
}

impl Program {
    fn line(&mut self, text: &str) {
        self.source.push_str(text);
        self.source.push('\n');
    }

    /// `expression` must equal `expected`; `expected` is an ASTER literal of the same type.
    fn check(&mut self, description: String, expression: &str, expected: &str) {
        self.checks
            .push((description, format!("({expression}) != {expected}")));
    }

    fn finish(mut self) -> (String, Vec<String>) {
        let mut entry = String::from("public int Check()\n{\n");
        for (index, (_, condition)) in self.checks.iter().enumerate() {
            writeln!(entry, "    if ({condition}) {{ return {}; }}", index + 1).expect("write");
        }
        entry.push_str("    return 0;\n}\n");
        self.source.push_str(&entry);
        (
            self.source,
            self.checks
                .into_iter()
                .map(|(description, _)| description)
                .collect(),
        )
    }
}

fn run_program(program: Program) {
    let (source, descriptions) = program.finish();
    let compilation = compile(&source)
        .unwrap_or_else(|diagnostics| panic!("generated source must compile: {diagnostics:#?}"));
    match execute(&compilation.mir, "Check") {
        Ok(ExecutionValue::Int(0)) => {}
        Ok(ExecutionValue::Int(index)) => {
            let index = usize::try_from(index).expect("positive index");
            panic!("mismatch in: {}", descriptions[index - 1]);
        }
        other => panic!("unexpected result {other:?}"),
    }
}

fn run_value(source: &str, function: &str) -> ExecutionValue {
    let compilation = compile(source)
        .unwrap_or_else(|diagnostics| panic!("source must compile: {diagnostics:#?}\n{source}"));
    execute(&compilation.mir, function).unwrap_or_else(|error| panic!("execution failed: {error}"))
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn run_project(source: &str, function: &str) -> ExecutionValue {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("aster-bitwise-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary project source");
    let compilation = compile_project(&path);
    std::fs::remove_file(&path).expect("remove temporary project source");
    let compilation = compilation.unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    execute(&compilation.compilation.mir, function).unwrap_or_else(|error| panic!("{error}"))
}

#[test]
fn bitwise_logic_matches_two_complement_for_every_integer_pair() {
    for left in TYPES {
        let mut program = Program::default();
        for right in TYPES {
            let Some(result) = left.common(right) else {
                continue;
            };
            for (name, operator) in [("And", "&"), ("Or", "|"), ("Xor", "^")] {
                let helper = format!("{name}_{}_{}", left.name, right.name);
                program.line(&format!(
                    "public {} {helper}({} a, {} b) {{ return a {operator} b; }}",
                    result.name, left.name, right.name
                ));
                for a in left.samples() {
                    for b in right.samples() {
                        let expected = result.wrap(logical(operator, a, b));
                        program.check(
                            format!("{} {operator} {} with {a} and {b}", left.name, right.name),
                            &format!("{helper}({}, {})", left.literal(a), right.literal(b)),
                            &result.literal(expected),
                        );
                    }
                }
            }
        }
        run_program(program);
    }
}

#[test]
fn shifts_mask_their_count_and_keep_the_promoted_left_type() {
    for left in TYPES {
        let result = left.unary();
        let mut program = Program::default();
        for (name, operator) in [("Shl", "<<"), ("Shr", ">>"), ("Ushr", ">>>")] {
            let helper = format!("{name}_{}", left.name);
            program.line(&format!(
                "public {} {helper}({} v, int n) {{ return v {operator} n; }}",
                result.name, left.name
            ));
            for value in left.samples() {
                for count in COUNTS {
                    let expected = shift(operator, value, count, result);
                    program.check(
                        format!("{} {operator} {count} with {value}", left.name),
                        &format!("{helper}({}, {})", left.literal(value), INT.literal(count)),
                        &result.literal(expected),
                    );
                }
            }
        }
        run_program(program);
    }
}

#[test]
fn shift_counts_of_every_int_convertible_type_behave_like_int_counts() {
    let mut program = Program::default();
    for count in [TYPES[0], TYPES[1], TYPES[2], TYPES[3], INT] {
        for left in [INT, TYPES[5], TYPES[6], TYPES[7]] {
            for (name, operator) in [("Shl", "<<"), ("Shr", ">>"), ("Ushr", ">>>")] {
                let helper = format!("{name}_{}_by_{}", left.name, count.name);
                program.line(&format!(
                    "public {} {helper}({} v, {} n) {{ return v {operator} n; }}",
                    left.name, left.name, count.name
                ));
                let (count_minimum, count_maximum) = count.range();
                for value in left.samples() {
                    for n in [
                        0,
                        1,
                        31,
                        32,
                        33,
                        63,
                        64,
                        65,
                        -1,
                        -32,
                        count_minimum,
                        count_maximum,
                    ] {
                        if n < count_minimum || n > count_maximum {
                            continue;
                        }
                        let expected = shift(operator, value, n, left);
                        program.check(
                            format!("{} {operator} {} {n}", left.name, count.name),
                            &format!("{helper}({}, {})", left.literal(value), count.literal(n)),
                            &left.literal(expected),
                        );
                    }
                }
            }
        }
    }
    run_program(program);
}

#[test]
fn constant_evaluation_agrees_with_runtime_evaluation() {
    for left in TYPES {
        let result = left.unary();
        let mut program = Program::default();
        let mut index = 0;
        for (name, operator) in [("Shl", "<<"), ("Shr", ">>"), ("Ushr", ">>>")] {
            let helper = format!("{name}_{}", left.name);
            program.line(&format!(
                "public {} {helper}({} v, int n) {{ return v {operator} n; }}",
                result.name, left.name
            ));
            for value in left.samples() {
                for count in COUNTS {
                    index += 1;
                    let constant = format!("K{index}");
                    program.line(&format!(
                        "public const {} {constant} = {} {operator} {};",
                        result.name,
                        left.literal(value),
                        INT.literal(count)
                    ));
                    let expected = result.literal(shift(operator, value, count, result));
                    program.check(
                        format!("constant {} {operator} {count} with {value}", left.name),
                        &constant,
                        &expected,
                    );
                    // The same case through the JIT must agree with the constant.
                    program.check(
                        format!("runtime {} {operator} {count} with {value}", left.name),
                        &format!("{helper}({}, {})", left.literal(value), INT.literal(count)),
                        &constant,
                    );
                }
            }
        }
        for right in TYPES {
            let Some(common) = left.common(right) else {
                continue;
            };
            for (name, operator) in [("And", "&"), ("Or", "|"), ("Xor", "^")] {
                for (a, b) in left.samples().into_iter().zip(right.samples()) {
                    index += 1;
                    let constant = format!("K{index}");
                    program.line(&format!(
                        "public const {} {constant} = {} {operator} {};",
                        common.name,
                        left.literal(a),
                        right.literal(b)
                    ));
                    program.check(
                        format!(
                            "constant {} {operator} {} with {a} and {b}",
                            left.name, right.name
                        ),
                        &constant,
                        &common.literal(common.wrap(logical(operator, a, b))),
                    );
                    let _ = name;
                }
            }
        }
        run_program(program);
    }
}

#[test]
fn complement_matches_two_complement_and_promotes_small_types() {
    let mut program = Program::default();
    let mut index = 0;
    for ty in TYPES {
        let result = ty.unary();
        let helper = format!("Not_{}", ty.name);
        program.line(&format!(
            "public {} {helper}({} v) {{ return ~v; }}",
            result.name, ty.name
        ));
        for value in ty.samples() {
            let expected = result.literal(result.wrap(!value));
            program.check(
                format!("~{} with {value}", ty.name),
                &format!("{helper}({})", ty.literal(value)),
                &expected,
            );
            index += 1;
            let constant = format!("N{index}");
            program.line(&format!(
                "public const {} {constant} = ~{};",
                result.name,
                ty.literal(value)
            ));
            program.check(
                format!("constant ~{} with {value}", ty.name),
                &constant,
                &expected,
            );
        }
    }
    run_program(program);
}

#[test]
fn boolean_bitwise_operators_are_eager_and_have_a_truth_table() {
    let mut program = Program::default();
    program.line("public bool And(bool a, bool b) { return a & b; }");
    program.line("public bool Or(bool a, bool b) { return a | b; }");
    program.line("public bool Xor(bool a, bool b) { return a ^ b; }");
    for a in [false, true] {
        for b in [false, true] {
            program.check(
                format!("{a} & {b}"),
                &format!("And({a}, {b})"),
                &(a & b).to_string(),
            );
            program.check(
                format!("{a} | {b}"),
                &format!("Or({a}, {b})"),
                &(a | b).to_string(),
            );
            program.check(
                format!("{a} ^ {b}"),
                &format!("Xor({a}, {b})"),
                &(a ^ b).to_string(),
            );
        }
    }
    run_program(program);

    // Both operands are evaluated even when the left one decides the result;
    // `&&` and `||` still short-circuit.
    let source = "
        public class Probe
        {
            public int Count;
            public Probe() {}
            public bool Hit(bool value) { Count = Count + 1; return value; }
        }
        public int Eager()
        {
            Probe p = new Probe();
            bool a = p.Hit(false) & p.Hit(true);
            bool b = p.Hit(true) | p.Hit(false);
            bool c = p.Hit(true) ^ p.Hit(true);
            return p.Count;
        }
        public int ShortCircuit()
        {
            Probe p = new Probe();
            bool a = p.Hit(false) && p.Hit(true);
            bool b = p.Hit(true) || p.Hit(false);
            return p.Count;
        }
        public int CompoundBool()
        {
            Probe p = new Probe();
            bool x = true;
            x &= p.Hit(false);
            x |= p.Hit(true);
            x ^= p.Hit(true);
            if (x) { return -1; }
            return p.Count;
        }
    ";
    assert_eq!(run_value(source, "Eager"), ExecutionValue::Int(6));
    assert_eq!(run_value(source, "ShortCircuit"), ExecutionValue::Int(2));
    assert_eq!(run_value(source, "CompoundBool"), ExecutionValue::Int(3));
}

#[test]
fn operands_and_counts_are_evaluated_left_to_right_exactly_once() {
    let source = "
        public class Trace
        {
            public string Log;
            public Trace() { Log = \"\"; }
            public int Mark(string tag, int value) { Log = Log + tag; return value; }
        }
        public string Order()
        {
            Trace t = new Trace();
            int a = t.Mark(\"a\", 1) << t.Mark(\"b\", 2);
            int b = t.Mark(\"c\", 5) & t.Mark(\"d\", 3) | t.Mark(\"e\", 8);
            int c = t.Mark(\"f\", 64) >> t.Mark(\"g\", 1) >>> t.Mark(\"h\", 1);
            int d = ~t.Mark(\"i\", 0) ^ t.Mark(\"j\", 1);
            int x = 7;
            x <<= t.Mark(\"k\", 1);
            return t.Log;
        }
        public int Values()
        {
            Trace t = new Trace();
            int a = t.Mark(\"a\", 1) << t.Mark(\"b\", 2);
            int b = t.Mark(\"c\", 5) & t.Mark(\"d\", 3) | t.Mark(\"e\", 8);
            return a * 100 + b;
        }
    ";
    assert_eq!(
        run_value(source, "Order"),
        ExecutionValue::String("abcdefghijk".to_owned())
    );
    assert_eq!(
        run_value(source, "Values"),
        ExecutionValue::Int(4 * 100 + (5 & 3 | 8))
    );
}

#[test]
fn precedence_and_associativity_hold_at_runtime() {
    let mut program = Program::default();
    program.line("public int Id(int v) { return v; }");
    for (expression, expected) in [
        ("1 + Id(2) << 3", (1 + 2) << 3),
        ("Id(1) << 2 + 1", 1 << (2 + 1)),
        ("Id(1) | 2 ^ 3 & 4", 1 | (2 ^ (3 & 4))),
        ("Id(6) & 3 | 8", (6 & 3) | 8),
        ("(Id(6) & 3) ^ 1", (6 & 3) ^ 1),
        ("-Id(8) >> 1 + 1", -8 >> (1 + 1)),
        ("Id(256) >> 2 >> 2", (256 >> 2) >> 2),
        ("~Id(1) + 1", !1 + 1),
        ("~Id(5) & 7", !5 & 7),
        ("Id(5) - 1 << 1", (5 - 1) << 1),
        ("Id(3) * 2 << 1", (3 * 2) << 1),
        ("Id(16) >> 2 > 3 ? 1 : 0", i32::from((16 >> 2) > 3)),
        ("Id(1) << 3 == 8 ? 1 : 0", i32::from((1 << 3) == 8)),
        ("(Id(5) & 4) != 0 ? 1 : 0", 1),
    ] {
        program.check(expression.to_owned(), expression, &format!("({expected})"));
    }
    run_program(program);
}

#[test]
fn compound_assignments_work_on_locals_fields_arrays_and_lists() {
    let source = "
        public class Box
        {
            public int Field;
            public ulong Wide;
            public Box() {}
        }
        public int Check()
        {
            int x = 6;
            x &= 3;   if (x != 2) { return 1; }
            x |= 8;   if (x != 10) { return 2; }
            x ^= 5;   if (x != 15) { return 3; }
            x <<= 2;  if (x != 60) { return 4; }
            x >>= 1;  if (x != 30) { return 5; }
            x >>>= 1; if (x != 15) { return 6; }
            int n = -16;
            n >>= 2;  if (n != -4) { return 7; }
            n = -16;
            n >>>= 28; if (n != 15) { return 8; }
            n = 1;
            n <<= 33; if (n != 2) { return 9; }
            n <<= -1; if (n != 0) { return 10; }
            ulong w = 1UL;
            w <<= 63;  if (w != 9223372036854775808UL) { return 11; }
            w >>= 63;  if (w != 1UL) { return 12; }
            w <<= 64;  if (w != 1UL) { return 13; }
            long l = -1L;
            l >>>= 60; if (l != 15L) { return 14; }
            l = -1L;
            l >>= 60;  if (l != -1L) { return 15; }
            uint u = 4294967295U;
            u >>= 31;  if (u != 1U) { return 16; }
            u <<= byte_count();
            if (u != 2U) { return 17; }
            Box b = new Box();
            b.Field = 12;
            b.Field &= 10; if (b.Field != 8) { return 18; }
            b.Field |= 1;  if (b.Field != 9) { return 19; }
            b.Field ^= 3;  if (b.Field != 10) { return 20; }
            b.Field <<= 2; if (b.Field != 40) { return 21; }
            b.Wide = 3UL;
            b.Wide <<= 40; b.Wide >>>= 8; if (b.Wide != 3UL << 32) { return 22; }
            int[] a = [1, 2, 3];
            a[0] |= 4;   if (a[0] != 5) { return 23; }
            a[1] <<= 3;  if (a[1] != 16) { return 24; }
            a[2] ^= 7;   if (a[2] != 4) { return 25; }
            List<int> list = new List<int>();
            list.Add(9);
            list.Add(-8);
            list[0] &= 12; if (list[0] != 8) { return 26; }
            list[1] >>= 1; if (list[1] != -4) { return 27; }
            list[1] >>>= 30; if (list[1] != 3) { return 28; }
            bool t = true;
            t &= false; if (t) { return 29; }
            t |= true;  if (!t) { return 30; }
            t ^= true;  if (t) { return 31; }
            return 0;
        }
        public int byte_count() { byte c = 1; return c; }
    ";
    assert_eq!(run_value(source, "Check"), ExecutionValue::Int(0));
}

#[test]
fn postfix_propagation_combines_with_bitwise_expressions() {
    let source = "
        using aster.core;
        public Result<int, string> Fetch(bool ok)
        {
            if (ok) { return Result<int, string>.Ok(12); }
            return Result<int, string>.Error(\"bad\");
        }
        public Result<int, string> Combine(bool ok)
        {
            int masked = Fetch(ok)? & 6;
            int shifted = Fetch(ok)? << 1;
            int mixed = (Fetch(ok)? | 1) ^ ~0;
            return Result<int, string>.Ok(masked | shifted | (mixed >>> 28));
        }
        public int Good()
        {
            return Combine(true) switch { Ok(v) => v, Error(e) => -1, };
        }
        public int Bad()
        {
            return Combine(false) switch { Ok(v) => v, Error(e) => -1, };
        }
    ";
    let masked = 12 & 6;
    let shifted = 12 << 1;
    let mixed = (12 | 1) ^ !0_i32;
    let expected = masked | shifted | (((mixed as u32) >> 28) as i32);
    assert_eq!(run_project(source, "Good"), ExecutionValue::Int(expected));
    assert_eq!(run_project(source, "Bad"), ExecutionValue::Int(-1));
}

#[test]
fn nested_generic_syntax_coexists_with_shift_expressions_at_runtime() {
    let source = "
        public int Run()
        {
            List<List<int>> rows = new List<List<int>>();
            List<int> first = new List<int>();
            first.Add(40);
            rows.Add(first);
            Dictionary<string, List<int>> lookup = new Dictionary<string, List<int>>();
            lookup.Add(\"a\", first);
            int shifted = rows[0][0] >> 2;
            int more = rows.Length << 3 >>> 1;
            bool smaller = rows.Length < shifted >> 1;
            rows[0][0] >>= 3;
            return shifted * 100 + more * 10 + (smaller ? 1 : 0) + rows[0][0] * 1000;
        }
    ";
    assert_eq!(
        run_value(source, "Run"),
        ExecutionValue::Int(10 * 100 + 4 * 10 + 1 + 5 * 1000)
    );
}

#[test]
fn scalar_bit_operations_lower_to_plain_arithmetic_without_allocation() {
    let source = "
        public ulong Mix(ulong seed, int rounds)
        {
            ulong state = seed;
            for (int i = 0; i < rounds; i++)
            {
                state = (state ^ (state >> 30)) * 13787848793156543929UL;
                state = (state ^ (state >> 27)) * 10723151780598845931UL;
                state = state ^ (state >> 31);
                state = (state << 7) | (state >>> 57);
                state &= ~0UL;
            }
            return state;
        }
        public ulong Run() { return Mix(88172645463325252UL, 1000); }
    ";
    let compilation = compile(source).expect("compiles");
    let mix = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Mix")
        .expect("Mix");
    let mut operators = std::collections::HashSet::new();
    for instruction in mix.blocks.iter().flat_map(|block| &block.instructions) {
        match instruction {
            mir::Instruction::Assign { value, .. } => match &value.kind {
                mir::RvalueKind::Use(_)
                | mir::RvalueKind::Cast(_)
                | mir::RvalueKind::Unary { .. } => {}
                mir::RvalueKind::Binary { operator, .. } => {
                    operators.insert(format!("{operator:?}"));
                }
                other => panic!("scalar bit operations must not need {other:?}"),
            },
            other => panic!("scalar bit operations must not emit {other:?}"),
        }
    }
    for operator in [
        "BitwiseXor",
        "BitwiseOr",
        "BitwiseAnd",
        "ShiftLeft",
        "ShiftRight",
        "UnsignedShiftRight",
        "Multiply",
    ] {
        assert!(
            operators.contains(operator),
            "{operator} stays a MIR operation"
        );
    }

    let (value, stats) = execute_with_stats(&compilation.mir, "Run").expect("runs");
    assert_eq!(stats.total_allocations, 0, "no allocation");
    // Independent reference implementation.
    let mut reference: u64 = 88_172_645_463_325_252;
    for _ in 0..1000 {
        reference = (reference ^ (reference >> 30)).wrapping_mul(13_787_848_793_156_543_929);
        reference = (reference ^ (reference >> 27)).wrapping_mul(10_723_151_780_598_845_931);
        reference ^= reference >> 31;
        reference = reference.rotate_left(7);
    }
    assert_eq!(value, ExecutionValue::ULong(reference));
}

#[test]
fn splitmix64_is_expressible_and_matches_the_library_generator() {
    // The proposed `Random` migration: the same output function written with
    // the new operators must reproduce the current `Random` sequence exactly.
    let source = "
        using aster.random;
        public ulong Mix(ulong value)
        {
            value = (value ^ (value >> 30)) * 13787848793156543929UL;
            value = (value ^ (value >> 27)) * 10723151780598845931UL;
            return value ^ (value >> 31);
        }
        public int Compare(ulong seed)
        {
            Random random = new Random(seed);
            ulong state = seed;
            for (int i = 0; i < 16; i++)
            {
                state = state + 11400714819323198485UL;
                if (random.NextULong() != Mix(state)) { return i + 1; }
            }
            return 0;
        }
        public int Zero() { return Compare(0UL); }
        public int One() { return Compare(1UL); }
        public int Pattern() { return Compare(123UL); }
        public int Max() { return Compare(18446744073709551615UL); }
    ";
    for function in ["Zero", "One", "Pattern", "Max"] {
        assert_eq!(
            run_project(source, function),
            ExecutionValue::Int(0),
            "{function}"
        );
    }
}
