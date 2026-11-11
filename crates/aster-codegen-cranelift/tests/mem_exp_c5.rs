//! MEM-EXP-1 C5: ownership pass, descriptors, and reference-count lowering.
#![cfg(feature = "mem-exp-rc-core")]

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::{compile, compile_project};

/// Like `run`, for sources that import the standard library (`using aster.*`).
fn run_project(source: &str, expected: i32) -> MemoryStats {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-mem-exp-c5-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compiled = compile_project(&path);
    std::fs::remove_file(&path).expect("remove temporary project");
    let compilation = compiled.expect("project compiles").compilation;
    let (value, stats) = execute_with_stats(&compilation.mir, "Run").expect("source executes");
    assert_eq!(value, ExecutionValue::Int(expected));
    stats
}

fn run(source: &str, expected: i32) -> MemoryStats {
    let compilation = compile(source).expect("source compiles");
    let (value, stats) = execute_with_stats(&compilation.mir, "Run").expect("source executes");
    assert_eq!(value, ExecutionValue::Int(expected));
    stats
}

const NODE: &str = "public class Node { public int value; \
    public Node(int value) { this.value = value; } } \
    public class Holder { public Node node; public Holder(Node node) { this.node = node; } } \
    internal Node Make(int value) { return new Node(value); } ";

#[test]
fn interface_dispatch_and_object_identity_stay_correct() {
    run(
        "public interface ICounter { void Add(int amount); int Get(); } \
         public class Counter : ICounter { private int value; \
         public Counter(int value) { this.value = value; } \
         public void Add(int amount) { value += amount; } public int Get() { return value; } } \
         internal int Change(ICounter counter) { counter.Add(32); return counter.Get(); } \
         public int Run() { Counter concrete = new Counter(10); ICounter contract = concrete; \
         return Change(contract) + concrete.Get(); }",
        84,
    );
}

#[test]
fn interface_values_reassigned_keep_the_right_object() {
    run(
        "public interface IValue { int Get(); } \
         public class First : IValue { public First() {} public int Get() { return 1; } } \
         public class Second : IValue { public Second() {} public int Get() { return 42; } } \
         public int Run() { IValue value = new First(); value = new Second(); return value.Get(); }",
        42,
    );
}

#[test]
fn a_shared_object_survives_one_holder_dropping_it() {
    run(
        &format!(
            "{NODE} public int Run() {{ Node shared = Make(42); Holder a = new Holder(shared); Holder b = new Holder(shared); \
             a.node = Make(1); \
             return b.node.value; }}"
        ),
        42,
    );
}

#[test]
fn nested_holders_are_traversable_after_intermediate_locals_die() {
    run(
        &format!(
            "{NODE} public class Outer {{ public Holder holder;              public Outer(Holder holder) {{ this.holder = holder; }} }}              internal Outer Wrap(int value) {{ Holder inner = new Holder(Make(value));              return new Outer(inner); }}              public int Run() {{ int total = 0;              for (int i = 0; i < 10; i += 1) {{ Outer outer = Wrap(i); total += outer.holder.node.value; }}              return total; }}"
        ),
        45,
    );
}

#[test]
fn replacing_a_field_in_a_loop_releases_the_old_object() {
    let stats = run(
        &format!(
            "{NODE} public int Run() {{ Holder holder = new Holder(Make(0)); \
             for (int i = 0; i < 100; i += 1) {{ holder.node = Make(i); }} \
             return holder.node.value; }}"
        ),
        99,
    );
    assert!(
        stats.object_heap.releases > 0,
        "no release executed: {:?}",
        stats.object_heap
    );
    assert!(
        stats.object_heap.released_blocks >= 99,
        "old field values were not released: {:?}",
        stats.object_heap
    );
    assert!(
        stats.object_heap.fresh_blocks < 20,
        "{:?}",
        stats.object_heap
    );
}

#[test]
fn reassigning_a_local_in_a_loop_releases_the_old_object() {
    let stats = run(
        &format!(
            "{NODE} public int Run() {{ Node node = Make(0); \
             for (int i = 1; i < 100; i += 1) {{ node = Make(i); }} return node.value; }}"
        ),
        99,
    );
    assert!(
        stats.object_heap.released_blocks >= 99,
        "old local values were not released: {:?}",
        stats.object_heap
    );
}

#[test]
fn an_object_dropped_by_a_callee_frame_is_released() {
    let stats = run(
        &format!(
            "{NODE} internal int Use(int i) {{ Node scratch = Make(i); Holder h = new Holder(Make(0)); \
             h.node = scratch; return h.node.value; }} \
             public int Run() {{ int total = 0; \
             for (int i = 0; i < 50; i += 1) {{ total += Use(i) - i; }} return total + 7; }}"
        ),
        7,
    );
    assert!(
        stats.object_heap.released_blocks >= 49,
        "callee frame objects leaked: {:?}",
        stats.object_heap
    );
}

#[test]
fn list_set_in_a_loop_releases_replaced_objects() {
    let stats = run(
        &format!(
            "{NODE} public int Run() {{ List<Node> list = new List<Node>(); list.Add(Make(0));              for (int i = 1; i < 100; i += 1) {{ list[0] = Make(i); }}              return list[0].value; }}"
        ),
        99,
    );
    assert!(
        stats.object_heap.released_blocks >= 99,
        "replaced list elements were not released: {:?}",
        stats.object_heap
    );
    assert!(
        stats.object_heap.reused_blocks > 0,
        "{:?}",
        stats.object_heap
    );
}

#[test]
fn list_remove_at_and_clear_release_their_elements() {
    let stats = run(
        &format!(
            "{NODE} public int Run() {{ int total = 0;              for (int round = 0; round < 20; round += 1) {{              List<Node> list = new List<Node>();              for (int i = 0; i < 5; i += 1) {{ list.Add(Make(i)); }}              list.RemoveAt(0); total += list[0].value;              list.Clear(); total += list.Length; }}              return total; }}"
        ),
        20,
    );
    // Each round allocates 5 nodes and releases all of them through the list.
    assert!(
        stats.object_heap.released_blocks >= 100,
        "removed list elements were not released: {:?}",
        stats.object_heap
    );
    assert!(
        stats.object_heap.reused_blocks > 0,
        "{:?}",
        stats.object_heap
    );
}

#[test]
fn to_array_copy_survives_the_list_replacing_the_element() {
    run(
        &format!(
            "{NODE} public int Run() {{ List<Node> list = new List<Node>(); list.Add(Make(41));              Node[] copy = list.ToArray();              list[0] = Make(1); Node scratch = Make(2);              return copy[0].value + list[0].value + scratch.value - 2; }}"
        ),
        42,
    );
}

#[test]
fn foreach_over_a_list_reads_every_element() {
    run(
        &format!(
            "{NODE} public int Run() {{ List<Node> list = new List<Node>();              for (int i = 0; i < 10; i += 1) {{ list.Add(Make(i)); }}              int total = 0;              foreach (Node node in list) {{ total += node.value; Node other = Make(100); total += other.value - 100; }}              return total; }}"
        ),
        45,
    );
}

// ---- Counted arrays (design section 8) ----

#[test]
fn a_tree_rebuilt_in_a_loop_releases_its_nodes() {
    let stats = run(
        &format!(
            "{NODE} public class Tree {{ public Node[] nodes; \
             public Tree(Node[] nodes) {{ this.nodes = nodes; }} }} \
             internal Tree Build(int seed) {{ Node[] nodes = [Make(seed), Make(seed + 1), Make(seed + 2), Make(seed + 3)]; \
             return new Tree(nodes); }} \
             public int Run() {{ int checksum = 0; \
             for (int round = 0; round < 50; round += 1) {{ Tree tree = Build(round); \
             checksum += tree.nodes[3].value; }} \
             return checksum; }}"
        ),
        // sum(round + 3) for round in 0..50
        1_225 + 150,
    );
    assert!(
        stats.object_heap.released_blocks > 0,
        "no block released: {:?}",
        stats.object_heap
    );
}

#[test]
fn overwriting_array_elements_in_a_loop_releases_old_objects() {
    let stats = run(
        &format!(
            "{NODE} public int Run() {{ Node[] nodes = [Make(0), Make(0)]; \
             for (int i = 0; i < 100; i += 1) {{ nodes[i % 2] = Make(i); }} \
             return nodes[0].value + nodes[1].value; }}"
        ),
        98 + 99,
    );
    assert!(
        stats.object_heap.released_blocks > 90,
        "old elements were not released: {:?}",
        stats.object_heap
    );
}

#[test]
fn int_arrays_allocated_in_a_loop_are_reclaimed() {
    let stats = run(
        "internal int[] Fill(int i) { int[] values = new int[8]; values[7] = i; return values; } \
         public int Run() { int total = 0; \
         for (int i = 0; i < 100; i += 1) { int[] values = Fill(i); total += values[7]; } \
         return total; }",
        4_950,
    );
    assert!(
        stats.object_heap.reused_blocks > 0,
        "no block reused: {:?}",
        stats.object_heap
    );
}

// Counted strings (design section 9). A string built and consumed in one frame
// can be region-owned Temporary storage (immortal, never counted), so the
// tests below build their strings in a callee that returns them: those are
// Persistent, counted, and owned by the caller.

const TEXT_BOX: &str = "public class TextBox { public string text; \
    public TextBox(string text) { this.text = text; } } \
    internal string Label(int i) { return $\"owned {i}\"; }     internal TextBox MakeBox(int i) { return new TextBox(Label(i)); } ";

fn label_length(i: i32) -> i32 {
    i32::try_from(format!("owned {i}").len()).unwrap()
}

#[test]
fn string_concat_through_a_callee_in_a_loop_reclaims_blocks() {
    let stats = run(
        "internal string Grow(string value) { return value + \"x\"; } \
         public int Run() { string value = \"\"; \
         for (int i = 0; i < 200; i += 1) { value = Grow(value); } \
         return value.Length; }",
        200,
    );
    assert!(
        stats.object_heap.released_strings >= 190,
        "old strings were not released: {:?}",
        stats.object_heap
    );
    assert!(
        stats.object_heap.reused_blocks > 100,
        "{:?}",
        stats.object_heap
    );
}

#[test]
fn interpolation_in_a_loop_reclaims_blocks() {
    let stats = run(
        &format!(
            "{TEXT_BOX} public int Run() {{ int total = 0; \
             for (int i = 0; i < 100; i += 1) {{ string text = Label(i); total += text.Length; }} \
             return total; }}"
        ),
        (0..100).map(label_length).sum(),
    );
    assert!(
        stats.object_heap.released_strings >= 90,
        "interpolated strings were not released: {:?}",
        stats.object_heap
    );
    assert!(
        stats.object_heap.reused_blocks > 0,
        "{:?}",
        stats.object_heap
    );
}

#[test]
fn replacing_a_string_field_in_a_loop_releases_the_old_string() {
    let stats = run(
        &format!(
            "{TEXT_BOX} public int Run() {{ TextBox box = new TextBox(\"start\"); \
             for (int i = 0; i < 100; i += 1) {{ box.text = Label(i); }} \
             return box.text.Length; }}"
        ),
        label_length(99),
    );
    assert!(
        stats.object_heap.released_strings >= 98,
        "old field strings were not released: {:?}",
        stats.object_heap
    );
}

#[test]
fn strings_in_class_fields_are_released_with_their_owner() {
    let stats = run(
        &format!(
            "{TEXT_BOX} public int Run() {{ int total = 0; \
             for (int i = 0; i < 50; i += 1) {{ TextBox box = MakeBox(i); \
             total += box.text.Length; }} return total; }}"
        ),
        (0..50).map(label_length).sum(),
    );
    assert!(
        stats.object_heap.released_strings >= 45,
        "field strings were not released with their owner: {:?}",
        stats.object_heap
    );
}

#[test]
fn strings_in_lists_are_released_by_clear_and_set() {
    let stats = run(
        &format!(
            "{TEXT_BOX} public int Run() {{ int total = 0; \
             for (int round = 0; round < 20; round += 1) {{ \
             List<string> list = new List<string>(); \
             for (int i = 0; i < 5; i += 1) {{ list.Add(Label(i)); }} \
             list[0] = \"replaced\"; total += list[0].Length + list[4].Length; \
             list.Clear(); total += list.Length; }} \
             return total; }}"
        ),
        20 * (8 + label_length(4)),
    );
    // Five strings per round, all released through the list glue.
    assert!(
        stats.object_heap.released_strings >= 80,
        "list strings were not released: {:?}",
        stats.object_heap
    );
}

#[test]
fn substring_and_trim_results_stay_valid_after_the_source_is_released() {
    let stats = run_project(
        "using aster.text; \
         internal string Label(int i) { return $\"  item {i}  \"; } \
         internal string Cut(int i) { string source = Label(i); \
         string trimmed = String.Trim(source); return String.Substring(trimmed, 0, 4); } \
         public int Run() { int matches = 0; \
         for (int i = 0; i < 60; i += 1) { string cut = Cut(i); \
         string noise = Label(i); \
         if (cut == \"item\") { matches += 1; } } \
         return matches; }",
        60,
    );
    assert!(
        stats.object_heap.released_strings >= 100,
        "{:?}",
        stats.object_heap
    );
}

#[test]
fn split_arrays_release_their_strings() {
    let stats = run_project(
        "using aster.text; \
         internal string[] Parts(int i) { return String.Split(\"a,bb,ccc,dddd\", \",\"); } \
         public int Run() { int total = 0; \
         for (int i = 0; i < 30; i += 1) { string[] parts = Parts(i); \
         total += parts[3].Length; } return total; }",
        120,
    );
    assert!(
        stats.object_heap.released_strings >= 100,
        "split strings were not released with their array: {:?}",
        stats.object_heap
    );
}

// Counted lists (design section 10): the header, the element buffer and the
// elements are released with the last holder.

const REQUEST: &str = "public class Request { public int id; public List<int> items; \
    public Request(int id, List<int> items) { this.id = id; this.items = items; } } \
    internal Request Build(int id) { List<int> items = new List<int>(); \
    int count = id % 8 + 1; \
    for (int index = 0; index < count; index += 1) { items.Add(id + index); } \
    return new Request(id, items); } ";

fn request_loop(requests: i32) -> MemoryStats {
    let expected: i32 = (0..requests).map(|id| id + (id % 8 + 1)).sum();
    run(
        &format!(
            "{REQUEST} public int Run() {{ int total = 0; \
             for (int i = 0; i < {requests}; i += 1) {{ Request request = Build(i); \
             total += request.id + request.items.Length; }} return total; }}"
        ),
        expected,
    )
}

#[test]
fn request_like_list_per_iteration_is_reclaimed() {
    let small = request_loop(200);
    let large = request_loop(2_000);
    assert!(
        large.object_heap.reused_blocks > small.object_heap.reused_blocks,
        "reuse did not grow: {:?} vs {:?}",
        small.object_heap,
        large.object_heap
    );
    assert!(
        large.object_heap.released_lists >= 1_900,
        "list headers were not released: {:?}",
        large.object_heap
    );
    // Ten times the requests must not need ten times the memory.
    assert!(
        large.used_bytes < small.used_bytes.saturating_mul(3),
        "used bytes grew with the request count: {} vs {}",
        small.used_bytes,
        large.used_bytes
    );
    assert!(
        large.object_heap.fresh_blocks < 40,
        "{:?}",
        large.object_heap
    );
}

#[test]
fn list_fields_are_released_with_their_owner() {
    let stats = run(
        "public class Bag { public List<int> values; \
         public Bag() { this.values = new List<int>(); } } \
         internal Bag MakeBag(int n) { Bag bag = new Bag(); \
         for (int i = 0; i < n; i += 1) { bag.values.Add(i); } return bag; } \
         public int Run() { int total = 0; \
         for (int i = 0; i < 100; i += 1) { Bag bag = MakeBag(6); total += bag.values.Length; } \
         return total; }",
        600,
    );
    assert!(
        stats.object_heap.released_lists >= 95,
        "field lists were not released with their owner: {:?}",
        stats.object_heap
    );
}

#[test]
fn a_list_replaced_in_a_field_releases_the_old_list() {
    let stats = run(
        "public class Bag { public List<int> values; \
         public Bag(List<int> values) { this.values = values; } } \
         internal List<int> Fresh(int n) { List<int> list = new List<int>(); \
         list.Add(n); return list; } \
         public int Run() { Bag bag = new Bag(Fresh(0)); \
         for (int i = 1; i < 100; i += 1) { bag.values = Fresh(i); } \
         return bag.values[0]; }",
        99,
    );
    assert!(
        stats.object_heap.released_lists >= 98,
        "replaced lists were not released: {:?}",
        stats.object_heap
    );
}

#[test]
fn lists_of_lists_release_their_inner_lists() {
    let stats = run(
        "internal List<int> Row(int n) { List<int> row = new List<int>(); \
         row.Add(n); row.Add(n + 1); return row; } \
         internal List<List<int>> Grid(int n) { List<List<int>> grid = new List<List<int>>(); \
         for (int i = 0; i < 3; i += 1) { grid.Add(Row(n + i)); } return grid; } \
         public int Run() { int total = 0; \
         for (int i = 0; i < 50; i += 1) { List<List<int>> grid = Grid(i); \
         total += grid[2][1]; } return total; }",
        (0..50).map(|i| i + 3).sum(),
    );
    // Each grid owns three rows: four lists per iteration.
    assert!(
        stats.object_heap.released_lists >= 4 * 45,
        "nested lists were not released: {:?}",
        stats.object_heap
    );
}

// Counted dictionaries (design section 11): the dictionary owns the key and
// the value of every live entry and is released with its last holder.

const ENTRY: &str = "using aster.core; \
    public class Entry { public int id; public Entry(int id) { this.id = id; } } ";

/// A sliding window of 20 live keys (storage stays within the size classes)
/// with steady `Set`, `Remove` and `TryGet`.
fn cache_churn(operations: i32) -> MemoryStats {
    let expected: i32 = (10..operations).map(|key| (key - 10) % 7).sum::<i32>() + 20;
    run_project(
        &format!(
            "{ENTRY} internal int Churn(int operations) {{ \
             Dictionary<int, Entry> cache = new Dictionary<int, Entry>(); int total = 0; \
             for (int key = 0; key < operations; key += 1) {{ \
             cache.Set(key, new Entry(key)); \
             if (key >= 20) {{ cache.Remove(key - 20); }} \
             switch (cache.TryGet(key - 10)) {{ \
             case Some(entry): total += entry.id % 7; case None: }} }} \
             return total + cache.Length; }} \
             public int Run() {{ return Churn({operations}); }}"
        ),
        expected,
    )
}

#[test]
fn dictionary_cache_churn_stays_bounded_between_200_and_2000_iterations() {
    let small = cache_churn(200);
    let large = cache_churn(2_000);
    assert!(
        large.object_heap.released_blocks > small.object_heap.released_blocks,
        "{:?} vs {:?}",
        small.object_heap,
        large.object_heap
    );
    assert!(
        large.object_heap.reused_blocks > small.object_heap.reused_blocks,
        "reuse did not grow: {:?} vs {:?}",
        small.object_heap,
        large.object_heap
    );
    // Ten times the iterations must not need ten times the memory.
    assert!(
        large.used_bytes < small.used_bytes.saturating_mul(3),
        "used bytes grew with the iteration count: {} vs {}",
        small.used_bytes,
        large.used_bytes
    );
    assert!(
        large.object_heap.fresh_blocks < 80,
        "{:?}",
        large.object_heap
    );
}

#[test]
fn dictionary_fields_are_released_with_their_owner() {
    let stats = run_project(
        &format!(
            "{ENTRY} public class Registry {{ public Dictionary<int, Entry> map; \
             public Registry() {{ this.map = new Dictionary<int, Entry>(); }} }} \
             internal Registry Make(int n) {{ Registry registry = new Registry(); \
             for (int i = 0; i < n; i += 1) {{ registry.map.Add(i, new Entry(i)); }} \
             return registry; }} \
             public int Run() {{ int total = 0; \
             for (int i = 0; i < 60; i += 1) {{ Registry registry = Make(5); \
             total += registry.map.Length; }} return total; }}"
        ),
        300,
    );
    assert!(
        stats.object_heap.released_dictionaries >= 55,
        "field dictionaries were not released with their owner: {:?}",
        stats.object_heap
    );
    // Five entries per registry were released with their dictionary.
    assert!(
        stats.object_heap.released_blocks >= 55 * 7,
        "{:?}",
        stats.object_heap
    );
}

#[test]
fn try_get_value_is_valid_until_set_replaces_the_key() {
    let stats = run_project(
        &format!(
            "{ENTRY} public int Run() {{ \
             Dictionary<int, Entry> map = new Dictionary<int, Entry>(); \
             map.Add(1, new Entry(10)); int total = 0; \
             switch (map.TryGet(1)) {{ case Some(entry): total += entry.id; case None: return -1; }} \
             map.Set(1, new Entry(20)); \
             switch (map.TryGet(1)) {{ case Some(entry): total += entry.id; case None: return -2; }} \
             Entry fallback = new Entry(7); \
             total += map.GetOr(2, fallback).id; \
             total += map.GetOrAdd(2, new Entry(3)).id; \
             total += map.GetOrAdd(2, new Entry(100)).id; \
             return total * 100 + map.Length; }}"
        ),
        (10 + 20 + 7 + 3 + 3) * 100 + 2,
    );
    // The replaced value and the rejected GetOrAdd value are released.
    assert!(
        stats.object_heap.released_blocks >= 2,
        "{:?}",
        stats.object_heap
    );
}

#[test]
fn string_keys_and_values_are_released_by_set_remove_and_clear() {
    let stats = run_project(
        &format!(
            "{ENTRY} public int Run() {{ \
             Dictionary<string, Entry> map = new Dictionary<string, Entry>(); int total = 0; \
             for (int i = 0; i < 50; i += 1) {{ map.Set($\"key {{i % 5}}\", new Entry(i)); }} \
             for (int i = 0; i < 50; i += 1) {{ map.Add($\"key {{i % 5}}\", new Entry(i)); }} \
             total += map.Length; \
             if (map.Remove(\"key 0\")) {{ total += 1; }} \
             switch (map.TryGet(\"key 4\")) {{ case Some(entry): total += entry.id; case None: }} \
             map.Clear(); total += map.Length; \
             return total; }}"
        ),
        5 + 1 + 49,
    );
    // 45 passed keys on Set and 50 on Add are released, plus the stored keys.
    assert!(
        stats.object_heap.released_strings >= 95,
        "{:?}",
        stats.object_heap
    );
    assert!(
        stats.object_heap.released_blocks >= 150,
        "{:?}",
        stats.object_heap
    );
}
