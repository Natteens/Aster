//! Native execution of calls whose integer-constant arguments are converted to
//! the parameter type (see `crates/aster-compiler/tests/constant_call_arguments.rs`).

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run(source: &str, function: &str) -> ExecutionValue {
    let compilation = compile(source)
        .unwrap_or_else(|diagnostics| panic!("source must compile: {diagnostics:#?}"));
    execute(&compilation.mir, function).unwrap_or_else(|error| panic!("execution failed: {error}"))
}

#[test]
fn converted_constant_arguments_reach_the_callee_unchanged() {
    let source = "
        public ulong IdentityULong(ulong value) { return value; }
        public uint IdentityUInt(uint value) { return value; }
        public byte IdentityByte(byte value) { return value; }
        public sbyte IdentitySByte(sbyte value) { return value; }
        public ushort IdentityUShort(ushort value) { return value; }
        public long IdentityLong(long value) { return value; }
        public ulong Decimal() { return IdentityULong(123); }
        public ulong Hexadecimal() { return IdentityULong(0x7B); }
        public ulong Binary() { return IdentityULong(0b1111011); }
        public ulong Widest() { return IdentityULong(0xFFFFFFFFFFFFFFFF); }
        public uint UIntParameter() { return IdentityUInt(4000000); }
        public uint UIntHex() { return IdentityUInt(0xFFFFFFFF); }
        public byte ByteMax() { return IdentityByte(255); }
        public sbyte SByteMin() { return IdentitySByte(-128); }
        public ushort UShortMax() { return IdentityUShort(0xFFFF); }
        public long LongParameter() { return IdentityLong(7); }
    ";
    for (function, expected) in [
        ("Decimal", ExecutionValue::ULong(123)),
        ("Hexadecimal", ExecutionValue::ULong(123)),
        ("Binary", ExecutionValue::ULong(123)),
        ("Widest", ExecutionValue::ULong(u64::MAX)),
        ("UIntParameter", ExecutionValue::UInt(4_000_000)),
        ("UIntHex", ExecutionValue::UInt(u32::MAX)),
        ("ByteMax", ExecutionValue::Byte(255)),
        ("SByteMin", ExecutionValue::SByte(-128)),
        ("UShortMax", ExecutionValue::UShort(65_535)),
        ("LongParameter", ExecutionValue::Long(7)),
    ] {
        assert_eq!(run(source, function), expected, "{function}");
    }
}

#[test]
fn constructors_methods_named_arguments_and_nesting_work_at_runtime() {
    let source = "
        public class Counter
        {
            private ulong total;
            public Counter(ulong start) { total = start; }
            public ulong Add(ulong amount) { total = total + amount; return total; }
            public static uint Twice(uint value) { return value * 2U; }
        }
        public uint Pick(uint first, uint second) { return first * 10U + second; }
        public ulong Scale(ulong value, byte factor) { return value * factor; }
        public ulong Total()
        {
            Counter counter = new Counter(10);
            ulong nested = counter.Add(Scale(3, 4));
            return nested + counter.Add(0x10);
        }
        public uint Named() { return Pick(second: 5, first: 7); }
        public uint StaticCall() { return Counter.Twice(21); }
    ";
    // 10 + 12 = 22; then 22 + 16 = 38; 22 + 38 = 60.
    assert_eq!(run(source, "Total"), ExecutionValue::ULong(60));
    assert_eq!(run(source, "Named"), ExecutionValue::UInt(75));
    assert_eq!(run(source, "StaticCall"), ExecutionValue::UInt(42));
}

#[test]
fn overload_choice_and_evaluation_order_are_unchanged() {
    let source = "
        public class Trace
        {
            public string Log;
            public Trace() { Log = \"\"; }
            public ulong Mark(string tag, ulong value) { Log = Log + tag; return value; }
        }
        public ulong Combine(ulong first, ulong second) { return first * 100UL + second; }
        public long Pick(long value) { return 1L; }
        public long Pick(byte value) { return 2L; }
        public long Chosen() { return Pick(5); }
        public ulong Value() { Trace t = new Trace(); return Combine(t.Mark(\"a\", 1), t.Mark(\"b\", 2)); }
        public string Order() { Trace t = new Trace(); ulong v = Combine(t.Mark(\"a\", 1), t.Mark(\"b\", 2)); return t.Log; }
    ";
    // `int` converts implicitly to `long`, so the constant conversion to `byte` never competes.
    assert_eq!(run(source, "Chosen"), ExecutionValue::Long(1));
    assert_eq!(run(source, "Value"), ExecutionValue::ULong(102));
    assert_eq!(
        run(source, "Order"),
        ExecutionValue::String("ab".to_owned())
    );
}
