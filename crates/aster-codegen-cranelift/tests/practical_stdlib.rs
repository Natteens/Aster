use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{
    ExecutionValue, execute, execute_with_filesystem, execute_with_stats,
};
use aster_compiler::compile_project;
use aster_runtime::MemoryFileSystemBackend;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn compile(source: &str) -> aster_mir::Module {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-practical-stdlib-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write ASTER fixture");
    let result = compile_project(&path);
    std::fs::remove_file(&path).expect("remove ASTER fixture");
    result.expect("fixture compiles").compilation.mir
}

#[test]
fn practical_text_math_random_and_collection_surface_executes() {
    let module = compile(
        r#"
using aster.collections;
using aster.core;
using aster.math;
using aster.random;
using aster.text;

public int Main()
{
    switch ("🙂".TryParseChar()) { case Some(value): if (value != '🙂') { return 1; } case None: return 2; }
    if ("128".TryParseSByte() != Option<sbyte>.None) { return 3; }
    if (String.IndexOf("a🙂b🙂", "🙂") != Option<int>.Some(1)) { return 4; }
    if (String.LastIndexOf("a🙂b🙂", "🙂") != Option<int>.Some(3)) { return 5; }
    if (String.Join("-", ["a", "b", "c"]) != "a-b-c") { return 6; }
    if (String.Repeat("á", 3) != "ááá") { return 7; }
    if (String.FromChars(String.ToChars("a🙂")) != "a🙂") { return 8; }

    StringBuilder builder = new StringBuilder();
    builder.Append('A'); builder.Append(42); builder.AppendLine("x");
    if (builder.Length != 5) { return 9; }
    builder.Clear(); if (builder.Length != 0) { return 10; }

    List<int> values = new List<int>();
    if (values.EnsureCapacity(8) < 8) { return 11; }
    values.AddRange([1, 2, 3]); values.Insert(1, 9); values.RemoveRange(2, 1); values.Reverse();
    int[] range = values.GetRange(0, values.Length);
    if (range.Length != 3 || range[0] != 3 || range[2] != 1) { return 12; }
    int[] overlap = [1, 2, 3, 4]; Array.CopyRange<int>(overlap, 0, overlap, 1, 3);
    if (overlap[1] != 1 || overlap[3] != 3) { return 13; }

    Dictionary<int, string> names = new Dictionary<int, string>();
    names.EnsureCapacity(8);
    if (names.GetOr(1, "fallback") != "fallback") { return 14; }
    if (names.GetOrAdd(1, "one") != "one" || names.GetOrAdd(1, "two") != "one") { return 15; }

    Random left = new Random(123UL); Random right = new Random(123UL);
    for (int i = 0; i < 100; i++) { if (left.NextULong() != right.NextULong()) { return 16; } }
    if (!Math.IsNaN(Math.Log(-1.0)) || !Math.IsFinite(Math.Pi())) { return 17; }
    return 42;
}
"#,
    );
    assert_eq!(execute(&module, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn practical_filesystem_surface_is_operation_scoped_and_deterministic() {
    let module = compile(
        r#"
using aster.core;
using aster.io;

public int Main()
{
    switch (ReadAllLines("root/input.txt")) { case Ok(lines): if (lines.Length != 2 || lines[0] != "one" || lines[1] != "two") { return 1; } case Error(error): return 2; }
    switch (WriteAllLines("root/output.txt", ["a", "b"])) { case Ok(bytes): if (bytes != 3) { return 3; } case Error(error): return 4; }
    switch (AppendAllText("root/output.txt", "\nc")) { case Ok(bytes): if (bytes != 2) { return 5; } case Error(error): return 6; }
    switch (CreateDirectory("root/child")) { case Ok(created): if (!created) { return 7; } case Error(error): return 8; }
    switch (ListDirectories("root")) { case Ok(paths): if (paths.Length != 1 || paths[0] != "root/child") { return 9; } case Error(error): return 10; }
    switch (FileExists("root/output.txt")) { case Ok(found): if (!found) { return 11; } case Error(error): return 12; }
    switch (DirectoryExists("root/child")) { case Ok(found): if (!found) { return 13; } case Error(error): return 14; }
    if (Path.GetFileName("a/b.txt") != "b.txt" || Path.GetExtension("a/b.txt") != ".txt" || Path.ChangeExtension("a/b.txt", "log") != "a/b.log") { return 15; }
    switch (DeleteDirectory("root/child")) { case Ok(deleted): if (!deleted) { return 16; } case Error(error): return 17; }
    switch (DeleteFile("root/output.txt")) { case Ok(deleted): if (!deleted) { return 18; } case Error(error): return 19; }
    return 42;
}
"#,
    );
    let backend = MemoryFileSystemBackend::new()
        .with_directory("root")
        .with_file("root/input.txt", "one\r\ntwo\r\n");
    let inspect = backend.clone();
    assert_eq!(
        execute_with_filesystem(&module, "Main", Box::new(backend)),
        Ok(ExecutionValue::Int(42))
    );
    assert!(inspect.read("root/output.txt").is_none());
}

#[test]
fn invalid_ranges_and_random_bounds_remain_controlled_failures() {
    let list = compile(
        "using aster.core; public int Main() { List<int> x = new List<int>(); x.RemoveRange(0, 1); return 1; }",
    );
    assert!(
        execute(&list, "Main")
            .expect_err("range fails")
            .to_string()
            .contains("range")
    );

    let random = compile(
        "using aster.random; public int Main() { Random r = new Random(1UL); return r.NextInt(4, 4); }",
    );
    assert!(
        execute(&random, "Main")
            .expect_err("range fails")
            .to_string()
            .contains("range")
    );
}

#[test]
fn strict_parsing_unicode_text_math_and_assertions_cover_edge_contracts() {
    let module = compile(
        r#"
using aster.core;
using aster.math;
using aster.testing;
using aster.text;

public int Main()
{
    if ("-128".TryParseSByte() != Option<sbyte>.Some((sbyte)-128)) { return 1; }
    if ("127".TryParseSByte() != Option<sbyte>.Some((sbyte)127)) { return 2; }
    if ("128".TryParseSByte() != Option<sbyte>.None) { return 3; }
    if ("255".TryParseByte() != Option<byte>.Some((byte)255)) { return 4; }
    if ("-1".TryParseByte() != Option<byte>.None) { return 5; }
    if ("-32768".TryParseShort() != Option<short>.Some((short)-32768)) { return 6; }
    if ("65535".TryParseUShort() != Option<ushort>.Some((ushort)65535)) { return 7; }
    if ("12x".TryParseInt() != Option<int>.None || " 12".TryParseInt() != Option<int>.None) { return 8; }
    if ("１２".TryParseInt() != Option<int>.None || "ab".TryParseChar() != Option<char>.None) { return 9; }
    if (String.IndexOf("a🙂b", "🙂") != Option<int>.Some(1)) { return 10; }
    if (String.LastIndexOf("aaa", "aa") != Option<int>.Some(0)) { return 11; }
    if (String.LastIndexOf("a🙂", "") != Option<int>.Some(2)) { return 12; }
    if (String.TrimStart("　x ") != "x " || String.TrimEnd(" x　") != " x") { return 13; }
    if (String.Repeat("🙂", 1024).Length != 1024) { return 14; }
    if (Math.Abs(-0.0d) != 0.0d || Math.Sign(-0.0d) != 0) { return 15; }
    Assert.NotEqual(1, 2);
    Assert.ApproximatelyEqual(1.0d, 1.0001d, 0.001d);
    return 42;
}
"#,
    );
    assert_eq!(execute(&module, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn direct_next_uint_is_sequence_compatible_across_seeds_and_interleaving() {
    let module = compile(
        r"
using aster.random;

public bool VerifyNextUIntSequence(ulong seed, int count)
{
    Random direct = new Random(seed);
    Random reference = new Random(seed);
    for (int i = 0; i < count; i++)
    {
        if (direct.NextUInt() != (uint)(reference.NextULong() / 4294967296UL)) { return false; }
    }
    return true;
}

public bool VerifyInterleavedSequence(ulong seed, int count)
{
    Random direct = new Random(seed);
    Random reference = new Random(seed);
    for (int i = 0; i < count; i++)
    {
        if (direct.NextUInt() != (uint)(reference.NextULong() / 4294967296UL)) { return false; }
        bool expectedBool = reference.NextULong() / 9223372036854775808UL != 0UL;
        if (direct.NextBool() != expectedBool) { return false; }
        ulong expectedDoubleWord = reference.NextULong();
        double expectedDouble = (double)(expectedDoubleWord / 2048UL) / 9007199254740992.0d;
        if (direct.NextDouble() != expectedDouble) { return false; }
        if (direct.NextULong() != reference.NextULong()) { return false; }
    }
    return true;
}

public int Main()
{
    ulong[] sequenceSeeds = [0UL, 1UL, 123UL, 987654321UL, 18446744073709551615UL];
    for (int i = 0; i < sequenceSeeds.Length; i++)
    {
        if (!VerifyNextUIntSequence(sequenceSeeds[i], 512)) { return 1; }
        if (!VerifyInterleavedSequence(sequenceSeeds[i], 128)) { return 2; }
    }
    return 42;
}
",
    );
    assert_eq!(execute(&module, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn deterministic_random_collections_and_clock_invariants_hold() {
    let module = compile(
        r#"
using aster.collections;
using aster.core;
using aster.random;
using aster.time;

public int Main()
{
    Random random = new Random(123UL);
    if (random.NextULong() != 13032462758197477675UL) { return 1; }
    if (random.NextULong() != 18015028434894305148UL) { return 2; }

    Random zero = new Random(0UL);
    if (zero.NextULong() != 16294208416658607535UL) { return 16; }
    Random booleans = new Random(0UL);
    if (!booleans.NextBool() || booleans.NextBool() || booleans.NextBool() || !booleans.NextBool()) { return 20; }
    Random sameLeft = new Random(77UL);
    Random sameRight = new Random(77UL);
    Random different = new Random(78UL);
    ulong sameValue = sameLeft.NextULong();
    if (sameValue != sameRight.NextULong() || sameValue == different.NextULong()) { return 3; }
    int[] buckets = new int[10];
    for (int i = 0; i < 10000; i++)
    {
        int bucket = random.NextInt(0, 10);
        buckets[bucket] = buckets[bucket] + 1;
    }
    for (int i = 0; i < buckets.Length; i++)
    {
        if (buckets[i] < 800 || buckets[i] > 1200) { return 4; }
    }
    for (int i = 0; i < 1000; i++)
    {
        if (random.NextInt(4, 5) != 4) { return 17; }
        int three = random.NextInt(0, 3);
        if (three < 0 || three >= 3) { return 18; }
        int power = random.NextInt(0, 256);
        if (power < 0 || power >= 256) { return 19; }
        int bounded = random.NextInt(-7, 13);
        if (bounded < -7 || bounded >= 13) { return 5; }
        int high = random.NextInt(2147483600, 2147483647);
        if (high < 2147483600 || high >= 2147483647) { return 6; }
        int low = random.NextInt(-2147483647 - 1, -2147483600);
        if (low < -2147483647 - 1 || low >= -2147483600) { return 7; }
        long wide = random.NextLong(-9223372036854775807L - 1L, 9223372036854775807L);
        if (wide < -9223372036854775807L - 1L || wide >= 9223372036854775807L) { return 8; }
        float unitFloat = random.NextFloat();
        if (unitFloat < 0.0f || unitFloat >= 1.0f) { return 9; }
        double unit = random.NextDouble();
        if (unit < 0.0d || unit >= 1.0d) { return 10; }
    }

    string[] source = ["a", "b", "c"];
    string[] copy = Array.Copy<string>(source);
    Array.CopyRange<string>(copy, 0, copy, 1, 2);
    if (copy[0] != "a" || copy[1] != "a" || copy[2] != "b") { return 11; }
    Array.Fill<string>(copy, "x"); Array.Reverse<string>(copy);
    if (copy[0] != "x" || copy[2] != "x") { return 12; }

    List<string> list = new List<string>(); list.AddRange(source);
    string[] range = list.GetRange(1, 2);
    if (range[0] != "b" || range[1] != "c") { return 13; }
    Dictionary<string, string> map = new Dictionary<string, string>();
    if (map.EnsureCapacity(16) < 16 || map.GetOrAdd("k", "v") != "v" || map.GetOr("k", "x") != "v") { return 14; }

    long first = Clock.MonotonicMilliseconds();
    long second = Clock.MonotonicMilliseconds();
    if (second < first || Clock.UnixMilliseconds() <= 0L) { return 15; }
    return 42;
}
"#,
    );
    assert_eq!(execute(&module, "Main"), Ok(ExecutionValue::Int(42)));
}

#[test]
fn assertion_failures_and_worker_clock_reads_remain_controlled() {
    let assertion = compile(
        "using aster.testing; public int Main() { Assert.Fail(\"deliberate\"); return 1; }",
    );
    assert!(
        execute(&assertion, "Main")
            .expect_err("assertion fails")
            .to_string()
            .contains("assertion")
    );

    for source in [
        "using aster.testing; public int Main() { Assert.ApproximatelyEqual(1.0d, 1.0d, 0.0d / 0.0d); return 1; }",
        "using aster.testing; public int Main() { Assert.ApproximatelyEqual(0.0d / 0.0d, 1.0d, 1.0d); return 1; }",
        "using aster.testing; public int Main() { Assert.ApproximatelyEqual(1.0d, 1.0d, -1.0d); return 1; }",
    ] {
        let module = compile(source);
        assert!(
            execute(&module, "Main")
                .expect_err("invalid approximate comparison must fail")
                .to_string()
                .contains("assertion")
        );
    }

    let worker = compile(
        "using aster.core; using aster.time; public long Read() { return Clock.MonotonicMilliseconds(); } public int Main() { Task<long> task = Task.Run(Read); task.Wait(); return 1; }",
    );
    assert!(
        execute(&worker, "Main")
            .expect_err("clock host operation is rejected in a worker")
            .to_string()
            .contains("clock host operation")
    );
}

#[test]
fn scalar_builder_append_does_not_allocate_intermediate_aster_strings() {
    let module = compile(
        r"
using aster.core;
public int Main()
{
    StringBuilder builder = new StringBuilder();
    for (int i = 0; i < 1000; i++) { builder.Append(i); }
    return builder.Length;
}
",
    );
    let (value, stats) = execute_with_stats(&module, "Main").expect("builder executes");
    assert_eq!(value, ExecutionValue::Int(2890));
    assert_eq!(stats.string_allocations, 0);
}

#[test]
fn malformed_random_mixer_mir_is_rejected_before_codegen() {
    let mut module = compile(
        "using aster.random; public int Main() { Random random = new(1UL); return random.NextULong() == 0UL ? 0 : 42; }",
    );
    let call = module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                aster_mir::Instruction::CallIntrinsic {
                    intrinsic: aster_mir::Intrinsic::RandomMix,
                    ..
                }
            )
        })
        .expect("Random.NextULong lowers through the mixer intrinsic");
    let aster_mir::Instruction::CallIntrinsic { return_type, .. } = call else {
        unreachable!();
    };
    *return_type = aster_mir::Type::Long;
    assert!(
        execute(&module, "Main")
            .expect_err("malformed random mixer MIR must fail validation")
            .to_string()
            .contains("malformed")
    );
}
