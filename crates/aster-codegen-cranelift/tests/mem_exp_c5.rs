//! MEM-EXP-1 C5: ownership pass, descriptors, and reference-count lowering.
#![cfg(feature = "mem-exp-c5")]

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute_with_stats};
use aster_compiler::compile;

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
