//! Release-only AARM allocator observability matrix.
//!
//! The metrics describe ASTER arena state and allocator events. Process RSS is
//! reported separately and includes the whole process, not only arena pages.

use std::{
    fmt::Write as _,
    mem::size_of,
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Instant,
};

use aster_codegen_cranelift::{
    AarmAsyncMemoryDomainTelemetry, AarmMemoryTelemetry, AarmParallelPlanningTelemetry,
    AarmTaskMemoryDomainTelemetry, ExecutionValue, execute_with_aarm_async_governor,
    execute_with_aarm_parallel_governor, execute_with_aarm_parallel_workers,
    execute_with_aarm_task_governor, execute_with_aarm_telemetry, parallel_chunk_budgets,
};
use aster_compiler::compile;
use aster_runtime::{
    AarmAllocatorEvents, AarmRegionTelemetry, ExecutionContext, MemoryGovernor,
    MemoryGovernorTelemetry,
    context::{
        aster_rt_array_element, aster_rt_array_new, aster_rt_array_new_temporary,
        aster_rt_temporary_scope_enter, aster_rt_temporary_scope_leave,
    },
};

const GOVERNOR_PAGE_BYTES: usize = 4 * 1024;
const PAGE_GROWTH_ALLOCATION_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    Small,
    Medium,
    Large,
}

impl Scale {
    fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "small" => Some(Self::Small),
            "medium" => Some(Self::Medium),
            "large" => Some(Self::Large),
            _ => None,
        }
    }

    fn tiny_iterations(self) -> usize {
        match self {
            Self::Small => 50_000,
            Self::Medium => 250_000,
            Self::Large => 1_000_000,
        }
    }

    fn scope_iterations(self) -> usize {
        match self {
            Self::Small => 10_000,
            Self::Medium => 50_000,
            Self::Large => 250_000,
        }
    }

    fn burst_bytes(self) -> usize {
        match self {
            Self::Small => 8 * 1024 * 1024,
            Self::Medium => 32 * 1024 * 1024,
            Self::Large => 128 * 1024 * 1024,
        }
    }

    fn worker_payload_bytes(self) -> usize {
        match self {
            Self::Small => 512 * 1024,
            Self::Medium => 2 * 1024 * 1024,
            Self::Large => 8 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessMemory {
    pub rss_bytes: Option<u64>,
    pub peak_rss_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseResult {
    pub workload: &'static str,
    pub scale: Scale,
    pub iterations: u64,
    pub workers: Option<u64>,
    pub checksum: i64,
    pub elapsed_micros: u128,
    pub telemetry: AarmMemoryTelemetry,
    pub parallel_plans: Vec<AarmParallelPlanningTelemetry>,
    pub task_domain: Option<AarmTaskMemoryDomainTelemetry>,
    pub async_domain: Option<AarmAsyncMemoryDomainTelemetry>,
    pub rss_before_bytes: Option<u64>,
    pub rss_at_peak_bytes: Option<u64>,
    pub rss_after_bytes: Option<u64>,
    pub process_peak_rss_bytes: Option<u64>,
}

#[must_use]
pub fn run_matrix(scales: &[Scale]) -> Vec<CaseResult> {
    let mut results = Vec::new();
    for &scale in scales {
        results.push(compiled_object_case("tiny_allocations", scale, false, true));
        results.push(compiled_object_case(
            "long_scope_temporary",
            scale,
            false,
            false,
        ));
        results.push(compiled_object_case(
            "helper_scoped_temporary",
            scale,
            true,
            false,
        ));
        results.push(direct_burst_case(scale, 1));
        results.push(direct_burst_case(scale, 4));
        results.push(persistent_control_case(scale));
        for workers in [1, 4, 16] {
            results.push(worker_context_case(scale, workers));
        }
        results.push(direct_tiny_allocation_case(scale, false));
        results.push(direct_tiny_allocation_case(scale, true));
        results.push(page_growth_case(scale, false));
        results.push(page_growth_case(scale, true));
        for contexts in [1, 4, 16] {
            results.push(governed_contexts_case(scale, contexts));
        }
        results.push(shared_governor_denial_case(scale));
        results.push(governor_teardown_reuse_case(scale));
        for workers in [1, 4, 16] {
            for kind in [
                ParallelKind::For,
                ParallelKind::ForEach,
                ParallelKind::Reduce,
            ] {
                results.push(parallel_case(scale, workers, kind, false));
                results.push(parallel_case(scale, workers, kind, true));
            }
        }
        results.push(tight_parallel_partition_case(scale));
        results.push(uneven_parallel_partition_case(scale));
        results.push(deterministic_parallel_denial_case(scale));
        for workers in [1, 2, 4, 16] {
            for kind in [
                TaskWorkload::Empty,
                TaskWorkload::SmallAllocation,
                TaskWorkload::ModerateAllocation,
                TaskWorkload::Swarm,
            ] {
                results.push(task_run_case(scale, workers, kind, false));
                results.push(task_run_case(scale, workers, kind, true));
            }
        }
        results.push(task_more_than_workers_case(scale));
        results.push(task_main_growth_case(scale));
        results.push(task_teardown_reuse_case(scale));
        results.push(task_tight_page_domain_case(scale));
        results.push(task_deterministic_denial_case(scale));
        for workers in [1, 4, 16] {
            for kind in [
                AsyncWorkload::Trivial,
                AsyncWorkload::BeforeAwait,
                AsyncWorkload::Inner,
                AsyncWorkload::AfterAwait,
                AsyncWorkload::MultipleHandles,
            ] {
                results.push(async_case(scale, workers, kind, false));
                results.push(async_case(scale, workers, kind, true));
            }
        }
        results.push(async_tight_domain_case(scale));
        results.push(async_inner_denial_case(scale));
        results.push(async_move_next_denial_case(scale));
        results.push(async_repeated_wait_case(scale));
        results.push(async_temporal_before_await_case(scale));
        results.push(async_temporal_inner_case(scale));
        results.push(async_temporal_after_await_case(scale));
    }
    results
}

fn compiled_object_case(
    workload: &'static str,
    scale: Scale,
    helper: bool,
    tiny: bool,
) -> CaseResult {
    let iterations = if tiny {
        scale.tiny_iterations()
    } else {
        scale.scope_iterations()
    };
    let source = if tiny {
        format!(
            "public class Tiny {{ public byte value; }} \
             public int Main() {{ int total = 0; for (int i = 0; i < {iterations}; i++) {{ \
             Tiny value = new Tiny(); value.value = 1; total += value.value; }} return total; }}"
        )
    } else {
        let body = if helper {
            "total += Build();"
        } else {
            "long[] values = new long[8]; values[0] = 1L; total += values[0];"
        };
        format!(
            "internal long Build() {{ long[] values = new long[8]; values[0] = 1L; return values[0]; }} \
             public long Main() {{ long total = 0L; for (int i = 0; i < {iterations}; i++) {{ \
             {body} }} return total; }}"
        )
    };
    execute_compiled_case(
        workload,
        scale,
        u64::try_from(iterations).expect("iterations fit u64"),
        &source,
        &if tiny {
            ExecutionValue::Int(i32::try_from(iterations).expect("iterations fit int"))
        } else {
            ExecutionValue::Long(i64::try_from(iterations).expect("iterations fit long"))
        },
    )
}

fn execute_compiled_case(
    workload: &'static str,
    scale: Scale,
    iterations: u64,
    source: &str,
    expected: &ExecutionValue,
) -> CaseResult {
    let module = compile(source).expect("AARM matrix source compiles").mir;
    let before = process_memory();
    let started = Instant::now();
    let (value, telemetry) =
        execute_with_aarm_telemetry(&module, "Main").expect("AARM matrix source executes");
    let elapsed_micros = started.elapsed().as_micros();
    assert_eq!(&value, expected);
    let after = process_memory();
    CaseResult {
        workload,
        scale,
        iterations,
        workers: None,
        checksum: execution_checksum(&value),
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: None,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn direct_burst_case(scale: Scale, repeats: usize) -> CaseResult {
    let bytes = scale.burst_bytes();
    let mut context = ExecutionContext::with_stats();
    let before = process_memory();
    let started = Instant::now();
    let mut checksum = 0_i64;
    let mut rss_at_peak = None;
    for _ in 0..repeats {
        let context_pointer = &raw mut context;
        aster_rt_temporary_scope_enter(context_pointer);
        let array = aster_rt_array_new_temporary(
            context_pointer,
            i32::try_from(bytes).expect("burst fits int"),
            1,
        );
        assert!(!array.is_null());
        checksum += touch_array(context_pointer, array, bytes);
        rss_at_peak = max_option(rss_at_peak, process_memory().rss_bytes);
        aster_rt_temporary_scope_leave(context_pointer);
        assert!(context.take_error().is_none());
    }
    let elapsed_micros = started.elapsed().as_micros();
    let telemetry = context
        .aarm_memory_telemetry()
        .expect("statistics mode enables telemetry");
    let after = process_memory();
    CaseResult {
        workload: if repeats == 1 {
            "temporary_burst_rewind"
        } else {
            "temporary_burst_rewind_reuse"
        },
        scale,
        iterations: u64::try_from(repeats).expect("repeats fit u64"),
        workers: None,
        checksum,
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: rss_at_peak,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn persistent_control_case(scale: Scale) -> CaseResult {
    let elements = scale.burst_bytes() / 8;
    let source = format!(
        "public long[] Build() {{ return new long[{elements}]; }} \
         public long Main() {{ long[] values = Build(); values[0] = 7L; \
         values[{last}] = 11L; return values[0] + values[{last}] + values.Length; }}",
        last = elements - 1
    );
    execute_compiled_case(
        "persistent_control",
        scale,
        u64::try_from(elements).expect("elements fit u64"),
        &source,
        &ExecutionValue::Long(i64::try_from(elements).expect("elements fit long") + 18),
    )
}

fn worker_context_case(scale: Scale, workers: usize) -> CaseResult {
    let payload = scale.worker_payload_bytes();
    let before = process_memory();
    let barrier = Arc::new(Barrier::new(workers + 1));
    let (sender, receiver) = mpsc::channel();
    let started = Instant::now();
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let barrier = Arc::clone(&barrier);
        let sender = sender.clone();
        handles.push(thread::spawn(move || {
            let mut context = ExecutionContext::with_stats();
            let context_pointer = &raw mut context;
            aster_rt_temporary_scope_enter(context_pointer);
            let array = aster_rt_array_new_temporary(
                context_pointer,
                i32::try_from(payload).expect("worker payload fits int"),
                1,
            );
            assert!(!array.is_null());
            let checksum = touch_array(context_pointer, array, payload);
            let peak = context
                .aarm_memory_telemetry()
                .expect("statistics mode enables telemetry");
            sender
                .send((checksum, peak))
                .expect("receiver remains live");
            barrier.wait();
            aster_rt_temporary_scope_leave(context_pointer);
            assert!(context.take_error().is_none());
            context
                .aarm_memory_telemetry()
                .expect("statistics mode enables telemetry")
        }));
    }
    drop(sender);
    let mut checksum = 0_i64;
    let mut active = Vec::with_capacity(workers);
    for _ in 0..workers {
        let (worker_checksum, telemetry) = receiver.recv().expect("worker reports telemetry");
        checksum += worker_checksum;
        active.push(telemetry);
    }
    let rss_at_peak = process_memory().rss_bytes;
    barrier.wait();
    let mut final_snapshots = Vec::with_capacity(workers);
    for handle in handles {
        final_snapshots.push(handle.join().expect("worker does not panic"));
    }
    let elapsed_micros = started.elapsed().as_micros();
    let mut telemetry = sum_telemetry(&final_snapshots);
    let active_sum = sum_telemetry(&active);
    telemetry.total.peak_live_used_bytes = active_sum.total.live_used_bytes;
    telemetry.total.peak_arena_capacity_bytes = active_sum.total.arena_capacity_bytes;
    let after = process_memory();
    CaseResult {
        workload: "worker_contexts",
        scale,
        iterations: u64::try_from(payload).expect("payload fits u64"),
        workers: Some(u64::try_from(workers).expect("workers fit u64")),
        checksum,
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: rss_at_peak,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn direct_tiny_allocation_case(scale: Scale, governed: bool) -> CaseResult {
    let iterations = scale.tiny_iterations();
    let governor = governed.then(|| Arc::new(MemoryGovernor::new(scale.burst_bytes())));
    let mut context = governor
        .as_ref()
        .map_or_else(ExecutionContext::with_stats, |governor| {
            ExecutionContext::with_memory_governor(Arc::clone(governor))
        });
    let before = process_memory();
    let started = Instant::now();
    let mut checksum = 0_i64;
    for _ in 0..iterations {
        let array = aster_rt_array_new(&raw mut context, 1, 1);
        assert!(!array.is_null());
        checksum += 1;
    }
    let elapsed_micros = started.elapsed().as_micros();
    assert!(context.take_error().is_none());
    let telemetry = context
        .aarm_memory_telemetry()
        .expect("statistics mode enables telemetry");
    let after = process_memory();
    CaseResult {
        workload: if governed {
            "governed_tiny_allocations"
        } else {
            "direct_tiny_allocations_control"
        },
        scale,
        iterations: u64::try_from(iterations).expect("iterations fit u64"),
        workers: None,
        checksum,
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: after.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn page_growth_case(scale: Scale, governed: bool) -> CaseResult {
    let pages = scale.burst_bytes() / PAGE_GROWTH_ALLOCATION_BYTES;
    let payload_bytes = PAGE_GROWTH_ALLOCATION_BYTES - size_of::<aster_runtime::AsterArray>();
    let governor = governed.then(|| Arc::new(MemoryGovernor::new(scale.burst_bytes())));
    let mut context = governor
        .as_ref()
        .map_or_else(ExecutionContext::with_stats, |governor| {
            ExecutionContext::with_memory_governor(Arc::clone(governor))
        });
    let before = process_memory();
    let started = Instant::now();
    for _ in 0..pages {
        let array = aster_rt_array_new(
            &raw mut context,
            i32::try_from(payload_bytes).expect("page-growth payload fits i32"),
            1,
        );
        assert!(!array.is_null());
    }
    let elapsed_micros = started.elapsed().as_micros();
    assert!(context.take_error().is_none());
    let telemetry = context
        .aarm_memory_telemetry()
        .expect("statistics mode enables telemetry");
    let at_peak = process_memory();
    CaseResult {
        workload: if governed {
            "governed_page_growth"
        } else {
            "page_growth_control"
        },
        scale,
        iterations: u64::try_from(pages).expect("page count fits u64"),
        workers: None,
        checksum: i64::try_from(pages).expect("page count fits i64"),
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: at_peak.rss_bytes,
        rss_after_bytes: at_peak.rss_bytes,
        process_peak_rss_bytes: at_peak.peak_rss_bytes,
    }
}

fn governed_contexts_case(scale: Scale, context_count: usize) -> CaseResult {
    let payload = scale.worker_payload_bytes();
    let retained_page_capacity = payload + size_of::<aster_runtime::AsterArray>();
    let governor = Arc::new(MemoryGovernor::new(
        retained_page_capacity
            .checked_mul(context_count)
            .expect("governor matrix budget is addressable"),
    ));
    let before = process_memory();
    let started = Instant::now();
    let mut contexts = Vec::with_capacity(context_count);
    let mut checksum = 0_i64;
    for _ in 0..context_count {
        let mut context = ExecutionContext::with_memory_governor(Arc::clone(&governor));
        let context_pointer = &raw mut context;
        let array = aster_rt_array_new(
            context_pointer,
            i32::try_from(payload).expect("context payload fits i32"),
            1,
        );
        assert!(!array.is_null());
        checksum += touch_array(context_pointer, array, payload);
        assert!(context.take_error().is_none());
        contexts.push(context);
    }
    let elapsed_micros = started.elapsed().as_micros();
    let at_peak = process_memory();
    let snapshots = contexts
        .iter()
        .map(|context| {
            context
                .aarm_memory_telemetry()
                .expect("governed contexts collect telemetry")
        })
        .collect::<Vec<_>>();
    let telemetry = sum_telemetry(&snapshots);
    drop(contexts);
    let after = process_memory();
    CaseResult {
        workload: match context_count {
            1 => "governed_contexts_1",
            4 => "governed_contexts_4",
            16 => "governed_contexts_16",
            _ => unreachable!("matrix uses fixed context counts"),
        },
        scale,
        iterations: u64::try_from(payload).expect("payload fits u64"),
        workers: None,
        checksum,
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: at_peak.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn shared_governor_denial_case(scale: Scale) -> CaseResult {
    let governor = Arc::new(MemoryGovernor::new(GOVERNOR_PAGE_BYTES));
    let mut first = ExecutionContext::with_memory_governor(Arc::clone(&governor));
    let mut second = ExecutionContext::with_memory_governor(Arc::clone(&governor));
    let before = process_memory();
    let started = Instant::now();
    assert!(!aster_rt_array_new(&raw mut first, 1, 1).is_null());
    assert!(aster_rt_array_new(&raw mut second, 1, 1).is_null());
    assert_eq!(
        second.take_error().as_deref(),
        Some("allocation exceeds the shared execution memory budget of 4096 bytes")
    );
    let elapsed_micros = started.elapsed().as_micros();
    let at_peak = process_memory();
    let mut telemetry = sum_telemetry(&[
        first
            .aarm_memory_telemetry()
            .expect("governed contexts collect telemetry"),
        second
            .aarm_memory_telemetry()
            .expect("governed contexts collect telemetry"),
    ]);
    telemetry.governor = Some(governor.telemetry());
    drop((first, second));
    let after = process_memory();
    CaseResult {
        workload: "shared_governor_denial",
        scale,
        iterations: 2,
        workers: None,
        checksum: 1,
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: at_peak.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn governor_teardown_reuse_case(scale: Scale) -> CaseResult {
    let governor = Arc::new(MemoryGovernor::new(GOVERNOR_PAGE_BYTES));
    let before = process_memory();
    let started = Instant::now();
    let mut first = ExecutionContext::with_memory_governor(Arc::clone(&governor));
    assert!(!aster_rt_array_new(&raw mut first, 1, 1).is_null());
    drop(first);
    let mut second = ExecutionContext::with_memory_governor(Arc::clone(&governor));
    assert!(!aster_rt_array_new(&raw mut second, 1, 1).is_null());
    assert!(second.take_error().is_none());
    let elapsed_micros = started.elapsed().as_micros();
    let at_peak = process_memory();
    let telemetry = second
        .aarm_memory_telemetry()
        .expect("governed contexts collect telemetry");
    drop(second);
    let after = process_memory();
    CaseResult {
        workload: "governor_teardown_reuse",
        scale,
        iterations: 2,
        workers: None,
        checksum: 2,
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: at_peak.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

#[derive(Clone, Copy)]
enum ParallelKind {
    For,
    ForEach,
    Reduce,
}

impl ParallelKind {
    fn source(self, iterations: usize) -> (String, ExecutionValue) {
        match self {
            Self::For => (
                format!(
                    "public void Body(int index) {{ int[] first = new int[1]; int[] second = new int[1]; first[0] = index; second[0] = first[0]; }} \
                     public int Main() {{ Parallel.For(0, {iterations}, Body); return {iterations}; }}"
                ),
                ExecutionValue::Int(i32::try_from(iterations).expect("iterations fit int")),
            ),
            Self::ForEach => (
                format!(
                    "public void Body(int value) {{ int[] first = new int[1]; int[] second = new int[1]; first[0] = value; second[0] = first[0]; }} \
                     public int Main() {{ int[] values = new int[{iterations}]; Parallel.ForEach(values, Body); return values.Length; }}"
                ),
                ExecutionValue::Int(i32::try_from(iterations).expect("iterations fit int")),
            ),
            Self::Reduce => (
                format!(
                    "public int AddValue(int total, int value) {{ int[] first = new int[1]; int[] second = new int[1]; return total + 1 + first[0] + second[0]; }} \
                     public int AddPartial(int left, int right) {{ int[] first = new int[1]; int[] second = new int[1]; return left + right + first[0] + second[0]; }} \
                     public int Main() {{ int[] values = new int[{iterations}]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }}"
                ),
                ExecutionValue::Int(i32::try_from(iterations).expect("iterations fit int")),
            ),
        }
    }

    fn workload(self, governed: bool) -> &'static str {
        match (self, governed) {
            (Self::For, false) => "parallel_for_control",
            (Self::For, true) => "governed_parallel_for",
            (Self::ForEach, false) => "parallel_for_each_control",
            (Self::ForEach, true) => "governed_parallel_for_each",
            (Self::Reduce, false) => "parallel_reduce_control",
            (Self::Reduce, true) => "governed_parallel_reduce",
        }
    }
}

fn parallel_case(scale: Scale, workers: usize, kind: ParallelKind, governed: bool) -> CaseResult {
    let iterations = scale.scope_iterations();
    let (source, expected) = kind.source(iterations);
    let module = compile(&source)
        .expect("Parallel AARM matrix source compiles")
        .mir;
    let before = process_memory();
    let started = Instant::now();
    let (value, telemetry, parallel_plans) = if governed {
        let governor = Arc::new(MemoryGovernor::new(scale.burst_bytes()));
        let (value, main, plans, worker_snapshots) =
            execute_with_aarm_parallel_governor(&module, "Main", workers, Arc::clone(&governor))
                .expect("governed Parallel matrix case executes");
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
        for plan in &plans {
            assert_eq!(
                plan.chunk_budgets_bytes.iter().sum::<u64>(),
                plan.available_headroom_bytes
            );
        }
        let mut snapshots = Vec::with_capacity(worker_snapshots.len() + 1);
        snapshots.push(main);
        snapshots.extend(worker_snapshots);
        let mut combined = sum_telemetry(&snapshots);
        combined.governor = main.governor;
        (value, combined, plans)
    } else {
        let (value, telemetry) = execute_with_aarm_parallel_workers(&module, "Main", workers)
            .expect("ordinary Parallel matrix control executes");
        (value, telemetry, Vec::new())
    };
    let elapsed_micros = started.elapsed().as_micros();
    assert_eq!(value, expected);
    let after = process_memory();
    CaseResult {
        workload: kind.workload(governed),
        scale,
        iterations: u64::try_from(iterations).expect("iterations fit u64"),
        workers: Some(u64::try_from(workers).expect("workers fit u64")),
        checksum: execution_checksum(&value),
        elapsed_micros,
        telemetry,
        parallel_plans,
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: after.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn tight_parallel_partition_case(scale: Scale) -> CaseResult {
    governed_parallel_shape_case("governed_parallel_tight_partition", scale, 4, 4, 16 * 1024)
}

fn uneven_parallel_partition_case(scale: Scale) -> CaseResult {
    governed_parallel_shape_case(
        "governed_parallel_uneven_chunks",
        scale,
        10,
        4,
        40 * 1024 + 2,
    )
}

fn governed_parallel_shape_case(
    workload: &'static str,
    scale: Scale,
    iterations: usize,
    workers: usize,
    hard_limit_bytes: usize,
) -> CaseResult {
    let source = format!(
        "public void Body(int index) {{ int[] scratch = new int[1]; scratch[0] = index; }} \
         public int Main() {{ Parallel.For(0, {iterations}, Body); return {iterations}; }}"
    );
    let module = compile(&source)
        .expect("Parallel shape source compiles")
        .mir;
    let governor = Arc::new(MemoryGovernor::new(hard_limit_bytes));
    let before = process_memory();
    let started = Instant::now();
    let (value, main, parallel_plans, worker_snapshots) =
        execute_with_aarm_parallel_governor(&module, "Main", workers, Arc::clone(&governor))
            .expect("governed Parallel shape executes");
    let elapsed_micros = started.elapsed().as_micros();
    assert_eq!(
        value,
        ExecutionValue::Int(i32::try_from(iterations).expect("iterations fit int"))
    );
    assert_eq!(parallel_plans.len(), 1);
    assert_eq!(
        parallel_plans[0].chunk_budgets_bytes.iter().sum::<u64>(),
        parallel_plans[0].available_headroom_bytes
    );
    let mut snapshots = Vec::with_capacity(worker_snapshots.len() + 1);
    snapshots.push(main);
    snapshots.extend(worker_snapshots);
    let mut telemetry = sum_telemetry(&snapshots);
    telemetry.governor = main.governor;
    assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    let after = process_memory();
    CaseResult {
        workload,
        scale,
        iterations: u64::try_from(iterations).expect("iterations fit u64"),
        workers: Some(u64::try_from(workers).expect("workers fit u64")),
        checksum: i64::try_from(iterations).expect("iterations fit i64"),
        elapsed_micros,
        telemetry,
        parallel_plans,
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: after.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn deterministic_parallel_denial_case(scale: Scale) -> CaseResult {
    const REPETITIONS: usize = 20;
    const WORKERS: usize = 4;
    const HARD_LIMIT_BYTES: usize = 16 * 1024;
    let module = compile(
        "public void Body(int index) { int size = index == 2 || index == 5 || index == 9 ? 20000 : 1; int[] scratch = new int[size]; } \
         public int Main() { Parallel.For(0, 16, Body); return 16; }",
    )
    .expect("deterministic denial source compiles")
    .mir;
    let governor = Arc::new(MemoryGovernor::new(HARD_LIMIT_BYTES));
    let before = process_memory();
    let started = Instant::now();
    let mut expected_error = None;
    for repetition in 0..REPETITIONS {
        let error =
            execute_with_aarm_parallel_governor(&module, "Main", WORKERS, Arc::clone(&governor))
                .expect_err("logical index 2 must exceed its deterministic local ceiling");
        assert!(error.message().contains("Parallel logical index 2"));
        assert_eq!(
            expected_error.get_or_insert_with(|| error.message().to_owned()),
            error.message(),
            "denial diagnostic changed at repetition {repetition}"
        );
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    }
    let elapsed_micros = started.elapsed().as_micros();
    let governor_telemetry = governor.telemetry();
    assert!(governor_telemetry.peak_capacity_bytes <= governor_telemetry.hard_limit_bytes);
    assert_eq!(
        governor_telemetry.grant_events,
        governor_telemetry.release_events
    );
    let budgets = parallel_chunk_budgets(HARD_LIMIT_BYTES as u64, WORKERS)
        .expect("fixed denial plan is representable");
    let telemetry = AarmMemoryTelemetry {
        governor: Some(governor_telemetry),
        ..AarmMemoryTelemetry::default()
    };
    let after = process_memory();
    CaseResult {
        workload: "governed_parallel_deterministic_denial",
        scale,
        iterations: REPETITIONS as u64,
        workers: Some(WORKERS as u64),
        checksum: 2,
        elapsed_micros,
        telemetry,
        parallel_plans: vec![AarmParallelPlanningTelemetry {
            operation: "Parallel.For",
            initial_governor_capacity_bytes: 0,
            available_headroom_bytes: HARD_LIMIT_BYTES as u64,
            chunk_budgets_bytes: budgets,
        }],
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: after.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn task_run_source() -> &'static str {
    "public int Small() { int[] scratch = new int[1]; scratch[0] = 1; return scratch[0]; } \
     public int Main() { \
       Task<int> a = Task.Run(Small); Task<int> b = Task.Run(Small); \
       Task<int> c = Task.Run(Small); Task<int> d = Task.Run(Small); \
       Task<int> e = Task.Run(Small); Task<int> f = Task.Run(Small); \
       Task<int> g = Task.Run(Small); Task<int> h = Task.Run(Small); \
       return a.Wait() + b.Wait() + c.Wait() + d.Wait() \
         + e.Wait() + f.Wait() + g.Wait() + h.Wait(); \
     }"
}

#[derive(Clone, Copy)]
enum TaskWorkload {
    Empty,
    SmallAllocation,
    ModerateAllocation,
    Swarm,
}

impl TaskWorkload {
    fn source(self) -> (String, i32, u64) {
        let (body, result, task_count): (&str, i32, usize) = match self {
            Self::Empty => ("return 1;", 1, 8),
            Self::SmallAllocation => (
                "int total = 0; for (int i = 0; i < 10000; i++) { int[] scratch = new int[1]; scratch[0] = 1; total += scratch[0]; } return total;",
                10_000,
                8,
            ),
            Self::ModerateAllocation => (
                "int[] scratch = new int[16000]; scratch[0] = 1; return scratch.Length;",
                16_000,
                8,
            ),
            Self::Swarm => (
                "int[] scratch = new int[1]; scratch[0] = 1; return scratch[0];",
                1,
                32,
            ),
        };
        let mut source = format!("public int Work() {{ {body} }} public int Main() {{ ");
        for index in 0..task_count {
            write!(source, "Task<int> task{index} = Task.Run(Work); ")
                .expect("writing into a String cannot fail");
        }
        source.push_str("return 0");
        for index in 0..task_count {
            write!(source, " + task{index}.Wait()").expect("writing into a String cannot fail");
        }
        source.push_str("; }");
        (
            source,
            result * i32::try_from(task_count).expect("task count fits i32"),
            u64::try_from(task_count).expect("task count fits u64"),
        )
    }

    fn workload(self, governed: bool) -> &'static str {
        match (self, governed) {
            (Self::Empty, false) => "task_empty_control",
            (Self::Empty, true) => "governed_task_empty",
            (Self::SmallAllocation, false) => "task_small_allocation_control",
            (Self::SmallAllocation, true) => "governed_task_small_allocation",
            (Self::ModerateAllocation, false) => "task_moderate_allocation_control",
            (Self::ModerateAllocation, true) => "governed_task_moderate_allocation",
            (Self::Swarm, false) => "task_swarm_control",
            (Self::Swarm, true) => "governed_task_swarm",
        }
    }
}

fn task_run_case(scale: Scale, workers: usize, kind: TaskWorkload, governed: bool) -> CaseResult {
    let (source, expected, task_count) = kind.source();
    let module = compile(&source)
        .expect("Task.Run matrix source compiles")
        .mir;
    let before = process_memory();
    let started = Instant::now();
    let (value, telemetry, task_domain) = if governed {
        let governor = Arc::new(MemoryGovernor::new(scale.burst_bytes()));
        let (value, mut telemetry, domain) =
            execute_with_aarm_task_governor(&module, "Main", workers, Arc::clone(&governor))
                .expect("governed Task.Run matrix control executes");
        telemetry.governor = Some(governor.telemetry());
        (value, telemetry, domain)
    } else {
        let (value, telemetry) = execute_with_aarm_parallel_workers(&module, "Main", workers)
            .expect("ordinary Task.Run matrix control executes");
        (value, telemetry, None)
    };
    let elapsed_micros = started.elapsed().as_micros();
    assert_eq!(value, ExecutionValue::Int(expected));
    let after = process_memory();
    CaseResult {
        workload: kind.workload(governed),
        scale,
        iterations: task_count,
        workers: Some(u64::try_from(workers).expect("workers fit u64")),
        checksum: i64::from(expected),
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: after.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn governed_task_shape_case(
    workload: &'static str,
    scale: Scale,
    source: &str,
    expected: &ExecutionValue,
    workers: usize,
    hard_limit_bytes: usize,
) -> CaseResult {
    let module = compile(source)
        .expect("governed Task.Run shape source compiles")
        .mir;
    let governor = Arc::new(MemoryGovernor::new(hard_limit_bytes));
    let before = process_memory();
    let started = Instant::now();
    let (value, mut telemetry, task_domain) =
        execute_with_aarm_task_governor(&module, "Main", workers, Arc::clone(&governor))
            .expect("governed Task.Run shape executes");
    let elapsed_micros = started.elapsed().as_micros();
    assert_eq!(&value, expected);
    telemetry.governor = Some(governor.telemetry());
    let after = process_memory();
    CaseResult {
        workload,
        scale,
        iterations: task_domain.map_or(0, |domain| domain.task_submissions),
        workers: Some(u64::try_from(workers).expect("workers fit u64")),
        checksum: execution_checksum(&value),
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: after.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn task_more_than_workers_case(scale: Scale) -> CaseResult {
    governed_task_shape_case(
        "governed_task_more_than_workers",
        scale,
        task_run_source(),
        &ExecutionValue::Int(8),
        2,
        64 * 1024,
    )
}

fn task_main_growth_case(scale: Scale) -> CaseResult {
    governed_task_shape_case(
        "governed_task_main_worker_growth",
        scale,
        "public int Work() { int[] scratch = new int[4000]; scratch[0] = 3; return scratch.Length; } \
         public int Main() { \
           int[] retained = new int[1000]; retained[0] = 1; \
           Task<int> a = Task.Run(Work); Task<int> b = Task.Run(Work); \
           int[] growth = new int[4000]; growth[0] = 2; \
           return retained.Length + growth.Length + a.Wait() + b.Wait(); \
         }",
        &ExecutionValue::Int(13_000),
        4,
        512 * 1024,
    )
}

fn task_teardown_reuse_case(scale: Scale) -> CaseResult {
    governed_task_shape_case(
        "governed_task_teardown_reuse",
        scale,
        "public int Small() { int[] scratch = new int[1]; return scratch.Length; } \
         public int Main() { int total = 0; \
           total += Task.Run(Small).Wait(); total += Task.Run(Small).Wait(); \
           total += Task.Run(Small).Wait(); total += Task.Run(Small).Wait(); \
           return total; }",
        &ExecutionValue::Int(4),
        4,
        64 * 1024,
    )
}

fn task_tight_page_domain_case(scale: Scale) -> CaseResult {
    governed_task_shape_case(
        "governed_task_tight_page_domain",
        scale,
        task_run_source(),
        &ExecutionValue::Int(8),
        16,
        2 * ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
    )
}

fn task_deterministic_denial_case(scale: Scale) -> CaseResult {
    const REPETITIONS: usize = 20;
    const WORKERS: usize = 4;
    const HARD_LIMIT_BYTES: usize = 64 * 1024;
    let module = compile(
        "public int Large() { int[] scratch = new int[20000]; return scratch.Length; } \
         public int Main() { Task<int> failure = Task.Run(Large); return 7; }",
    )
    .expect("task denial source compiles")
    .mir;
    let governor = Arc::new(MemoryGovernor::new(HARD_LIMIT_BYTES));
    let before = process_memory();
    let started = Instant::now();
    let mut task_domain = None;
    for _ in 0..REPETITIONS {
        let (value, _, observed) =
            execute_with_aarm_task_governor(&module, "Main", WORKERS, Arc::clone(&governor))
                .expect("unwaited failed task remains cached per handle");
        assert_eq!(value, ExecutionValue::Int(7));
        let observed = observed.expect("submission freezes the task domain");
        assert_eq!(observed.task_context_memory_failures, 1);
        merge_task_domain(&mut task_domain, observed);
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    }
    let elapsed_micros = started.elapsed().as_micros();
    let governor_telemetry = governor.telemetry();
    assert_eq!(governor_telemetry.grant_events, 0);
    assert_eq!(governor_telemetry.current_capacity_bytes, 0);
    let after = process_memory();
    CaseResult {
        workload: "governed_task_deterministic_denial",
        scale,
        iterations: REPETITIONS as u64,
        workers: Some(WORKERS as u64),
        checksum: 7,
        elapsed_micros,
        telemetry: AarmMemoryTelemetry {
            governor: Some(governor_telemetry),
            ..AarmMemoryTelemetry::default()
        },
        parallel_plans: Vec::new(),
        task_domain,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: after.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn merge_task_domain(
    aggregate: &mut Option<AarmTaskMemoryDomainTelemetry>,
    observed: AarmTaskMemoryDomainTelemetry,
) {
    if let Some(aggregate) = aggregate {
        assert_eq!(
            aggregate.task_context_ceiling_bytes,
            observed.task_context_ceiling_bytes
        );
        assert_eq!(
            aggregate.task_memory_concurrency_limit,
            observed.task_memory_concurrency_limit
        );
        aggregate.task_submissions += observed.task_submissions;
        aggregate.task_contexts_started += observed.task_contexts_started;
        aggregate.task_contexts_completed += observed.task_contexts_completed;
        aggregate.task_context_memory_failures += observed.task_context_memory_failures;
        aggregate.active_page_fast_path_allocations += observed.active_page_fast_path_allocations;
        aggregate.fresh_page_allocations += observed.fresh_page_allocations;
    } else {
        *aggregate = Some(observed);
    }
}

#[derive(Clone, Copy)]
enum AsyncWorkload {
    Trivial,
    BeforeAwait,
    Inner,
    AfterAwait,
    MultipleHandles,
}

impl AsyncWorkload {
    fn source(self) -> (&'static str, i32) {
        match self {
            Self::Trivial => (
                "public int Inner() { return 7; } public async Task<int> Later() { int value = await Task.Run(Inner); return value; } public int Main() { return Later().Wait(); }",
                7,
            ),
            Self::BeforeAwait => (
                "public int Scratch() { int[] values = new int[1000]; return values.Length; } public int Inner() { return 7; } public async Task<int> Later() { int before = Scratch(); int value = await Task.Run(Inner); return before + value; } public int Main() { return Later().Wait(); }",
                1007,
            ),
            Self::Inner => (
                "public int Inner() { int[] values = new int[4000]; return values.Length; } public async Task<int> Later() { int value = await Task.Run(Inner); return value; } public int Main() { return Later().Wait(); }",
                4000,
            ),
            Self::AfterAwait => (
                "public int Inner() { return 7; } public async Task<int> Later() { int value = await Task.Run(Inner); int[] after = new int[1000]; return value + after.Length; } public int Main() { return Later().Wait(); }",
                1007,
            ),
            Self::MultipleHandles => (
                "public int A() { return 10; } public int B() { return 20; } public async Task<int> LaterA() { int value = await Task.Run(A); return value; } public async Task<int> LaterB() { int value = await Task.Run(B); return value; } public int Main() { Task<int> a = LaterA(); Task<int> b = LaterB(); return b.Wait() + a.Wait(); }",
                30,
            ),
        }
    }

    fn workload(self, governed: bool) -> &'static str {
        match (self, governed) {
            (Self::Trivial, false) => "async_trivial_control",
            (Self::Trivial, true) => "governed_async_trivial",
            (Self::BeforeAwait, false) => "async_before_await_control",
            (Self::BeforeAwait, true) => "governed_async_before_await",
            (Self::Inner, false) => "async_inner_control",
            (Self::Inner, true) => "governed_async_inner",
            (Self::AfterAwait, false) => "async_after_await_control",
            (Self::AfterAwait, true) => "governed_async_after_await",
            (Self::MultipleHandles, false) => "async_multiple_handles_control",
            (Self::MultipleHandles, true) => "governed_async_multiple_handles",
        }
    }
}

fn async_case(scale: Scale, workers: usize, kind: AsyncWorkload, governed: bool) -> CaseResult {
    let (source, expected) = kind.source();
    let module = compile(source).expect("async matrix source compiles").mir;
    let before = process_memory();
    let started = Instant::now();
    let (value, telemetry, async_domain) = if governed {
        let governor = Arc::new(MemoryGovernor::new(scale.burst_bytes()));
        let (value, mut telemetry, domain) =
            execute_with_aarm_async_governor(&module, "Main", workers, Arc::clone(&governor))
                .expect("governed async matrix control executes");
        telemetry.governor = Some(governor.telemetry());
        (value, telemetry, domain)
    } else {
        let (value, telemetry) = execute_with_aarm_parallel_workers(&module, "Main", workers)
            .expect("ordinary async matrix control executes");
        (value, telemetry, None)
    };
    let elapsed_micros = started.elapsed().as_micros();
    assert_eq!(value, ExecutionValue::Int(expected));
    let after = process_memory();
    CaseResult {
        workload: kind.workload(governed),
        scale,
        iterations: async_domain.map_or(1, |domain| domain.move_next_contexts_started),
        workers: Some(u64::try_from(workers).expect("workers fit u64")),
        checksum: i64::from(expected),
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain: None,
        async_domain,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: after.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn governed_async_shape_case(
    workload: &'static str,
    scale: Scale,
    source: &str,
    expected: i32,
    hard_limit_bytes: usize,
) -> CaseResult {
    let module = compile(source).expect("async shape source compiles").mir;
    let governor = Arc::new(MemoryGovernor::new(hard_limit_bytes));
    let before = process_memory();
    let started = Instant::now();
    let (value, mut telemetry, async_domain) =
        execute_with_aarm_async_governor(&module, "Main", 4, Arc::clone(&governor))
            .expect("governed async shape executes");
    let elapsed_micros = started.elapsed().as_micros();
    assert_eq!(value, ExecutionValue::Int(expected));
    telemetry.governor = Some(governor.telemetry());
    let after = process_memory();
    CaseResult {
        workload,
        scale,
        iterations: async_domain.map_or(0, |domain| domain.move_next_contexts_started),
        workers: Some(4),
        checksum: i64::from(expected),
        elapsed_micros,
        telemetry,
        parallel_plans: Vec::new(),
        task_domain: None,
        async_domain,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: after.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn async_tight_domain_case(scale: Scale) -> CaseResult {
    governed_async_shape_case(
        "governed_async_tight_three_page_domain",
        scale,
        AsyncWorkload::Trivial.source().0,
        7,
        3 * ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
    )
}

fn async_repeated_wait_case(scale: Scale) -> CaseResult {
    governed_async_shape_case(
        "governed_async_repeated_wait",
        scale,
        "public int Inner() { int[] values = new int[1]; return values.Length; } public async Task<int> Later() { int value = await Task.Run(Inner); return value; } public int Main() { Task<int> task = Later(); return task.Wait() + task.Wait(); }",
        2,
        256 * 1024,
    )
}

fn async_temporal_before_await_case(scale: Scale) -> CaseResult {
    governed_async_shape_case(
        "governed_async_temporal_before_await",
        scale,
        "public int Scratch() { int[] values = new int[2000]; return values.Length; } public int Inner() { return 1; } public async Task<int> Later() { int before = Scratch(); int value = await Task.Run(Inner); return before + value; } public int Main() { return Later().Wait(); }",
        2_001,
        3 * ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
    )
}

fn async_temporal_inner_case(scale: Scale) -> CaseResult {
    governed_async_shape_case(
        "governed_async_temporal_inner",
        scale,
        "public int Inner() { int[] values = new int[2000]; return values.Length; } public async Task<int> Later() { int value = await Task.Run(Inner); return value; } public int Main() { return Later().Wait(); }",
        2_000,
        3 * ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
    )
}

fn async_temporal_after_await_case(scale: Scale) -> CaseResult {
    governed_async_shape_case(
        "governed_async_temporal_after_await",
        scale,
        "public int Inner() { return 1; } public async Task<int> Later() { int value = await Task.Run(Inner); int[] after = new int[2000]; return value + after.Length; } public int Main() { return Later().Wait(); }",
        2_001,
        3 * ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES,
    )
}

fn async_denial_case(
    workload: &'static str,
    scale: Scale,
    source: &str,
    hard_limit_bytes: usize,
    expected: &str,
) -> CaseResult {
    const REPETITIONS: usize = 20;
    let module = compile(source).expect("async denial source compiles").mir;
    let governor = Arc::new(MemoryGovernor::new(hard_limit_bytes));
    let before = process_memory();
    let started = Instant::now();
    let mut error_checksum = 0_i64;
    for _ in 0..REPETITIONS {
        let error = execute_with_aarm_async_governor(&module, "Main", 4, Arc::clone(&governor))
            .expect_err("fixed async entitlement rejects the allocation");
        assert!(error.message().contains(expected));
        error_checksum = i64::try_from(error.message().len()).expect("error length fits i64");
        assert_eq!(governor.telemetry().current_capacity_bytes, 0);
    }
    let elapsed_micros = started.elapsed().as_micros();
    let after = process_memory();
    CaseResult {
        workload,
        scale,
        iterations: REPETITIONS as u64,
        workers: Some(4),
        checksum: error_checksum,
        elapsed_micros,
        telemetry: AarmMemoryTelemetry {
            governor: Some(governor.telemetry()),
            ..AarmMemoryTelemetry::default()
        },
        parallel_plans: Vec::new(),
        task_domain: None,
        async_domain: None,
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: after.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
}

fn async_inner_denial_case(scale: Scale) -> CaseResult {
    async_denial_case(
        "governed_async_inner_denial",
        scale,
        "public int Inner() { int[] values = new int[1]; return values.Length; } public async Task<int> Later() { int value = await Task.Run(Inner); return value; } public int Main() { return Later().Wait(); }",
        0,
        "deterministic async awaited-inner memory entitlement",
    )
}

fn async_move_next_denial_case(scale: Scale) -> CaseResult {
    async_denial_case(
        "governed_async_move_next_denial",
        scale,
        "public int Scratch() { int[] values = new int[1]; return values.Length; } public int Inner() { return 1; } public async Task<int> Later() { int before = Scratch(); int value = await Task.Run(Inner); return value + before; } public int Main() { return Later().Wait(); }",
        0,
        "deterministic async MoveNext memory entitlement",
    )
}

fn touch_array(
    context: *mut ExecutionContext,
    array: *mut aster_runtime::AsterArray,
    bytes: usize,
) -> i64 {
    let mut touched = 0_i64;
    for index in (0..bytes).step_by(4096) {
        let element = aster_rt_array_element(
            context,
            array,
            i32::try_from(index).expect("array index fits int"),
        );
        assert!(!element.is_null());
        // SAFETY: checked array lookup returned one live byte owned by context.
        #[allow(unsafe_code)]
        unsafe {
            element.write(1);
        }
        touched += 1;
    }
    touched
}

fn execution_checksum(value: &ExecutionValue) -> i64 {
    match value {
        ExecutionValue::Int(value) => i64::from(*value),
        ExecutionValue::Long(value) => *value,
        other => panic!("unexpected checksum value {other}"),
    }
}

fn max_option(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn sum_telemetry(snapshots: &[AarmMemoryTelemetry]) -> AarmMemoryTelemetry {
    let mut result = AarmMemoryTelemetry::default();
    for snapshot in snapshots {
        result.requested_bytes += snapshot.requested_bytes;
        add_region(&mut result.temporary, snapshot.temporary);
        add_region(&mut result.persistent, snapshot.persistent);
        add_region(&mut result.total, snapshot.total);
    }
    result.governor = snapshots
        .iter()
        .rev()
        .find_map(|snapshot| snapshot.governor);
    result
}

fn add_region(total: &mut AarmRegionTelemetry, value: AarmRegionTelemetry) {
    total.live_used_bytes += value.live_used_bytes;
    total.arena_capacity_bytes += value.arena_capacity_bytes;
    total.active_page_capacity_bytes += value.active_page_capacity_bytes;
    total.inactive_page_capacity_bytes += value.inactive_page_capacity_bytes;
    total.page_count += value.page_count;
    total.active_page_count += value.active_page_count;
    total.inactive_page_count += value.inactive_page_count;
    total.peak_live_used_bytes += value.peak_live_used_bytes;
    total.peak_arena_capacity_bytes += value.peak_arena_capacity_bytes;
    add_events(&mut total.events, value.events);
    total.last_rewind = value.last_rewind.or(total.last_rewind);
}

fn add_events(total: &mut AarmAllocatorEvents, value: AarmAllocatorEvents) {
    total.active_page_fast_path_allocations += value.active_page_fast_path_allocations;
    total.slow_path_allocations += value.slow_path_allocations;
    total.inactive_page_reuse_events += value.inactive_page_reuse_events;
    total.fresh_regular_page_allocations += value.fresh_regular_page_allocations;
    total.fresh_oversized_page_allocations += value.fresh_oversized_page_allocations;
    total.rewind_events += value.rewind_events;
    total.rewound_bytes += value.rewound_bytes;
    total.allocation_limit_denials += value.allocation_limit_denials;
}

#[cfg(windows)]
#[must_use]
pub fn process_memory() -> ProcessMemory {
    use std::mem::{MaybeUninit, size_of};
    use windows_sys::Win32::System::{
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::GetCurrentProcess,
    };

    let mut counters = MaybeUninit::<PROCESS_MEMORY_COUNTERS>::zeroed();
    let counter_size = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS>()).unwrap_or(u32::MAX);
    // SAFETY: the process pseudo-handle is valid in this process, the output
    // pointer references writable storage of the exact advertised size, and
    // the value is read only after the API reports success.
    #[allow(unsafe_code)]
    let success =
        unsafe { GetProcessMemoryInfo(GetCurrentProcess(), counters.as_mut_ptr(), counter_size) };
    if success == 0 {
        return ProcessMemory::default();
    }
    // SAFETY: successful `GetProcessMemoryInfo` initialized the structure.
    #[allow(unsafe_code)]
    let counters = unsafe { counters.assume_init() };
    ProcessMemory {
        rss_bytes: Some(counters.WorkingSetSize as u64),
        peak_rss_bytes: Some(counters.PeakWorkingSetSize as u64),
    }
}

#[cfg(target_os = "linux")]
#[must_use]
pub fn process_memory() -> ProcessMemory {
    std::fs::read_to_string("/proc/self/status").map_or_else(
        |_| ProcessMemory::default(),
        |status| parse_linux_status(&status),
    )
}

#[cfg(target_os = "linux")]
fn parse_linux_status(status: &str) -> ProcessMemory {
    fn field(status: &str, name: &str) -> Option<u64> {
        let line = status.lines().find(|line| line.starts_with(name))?;
        let kib = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
        kib.checked_mul(1024)
    }
    ProcessMemory {
        rss_bytes: field(status, "VmRSS:"),
        peak_rss_bytes: field(status, "VmHWM:"),
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
#[must_use]
pub fn process_memory() -> ProcessMemory {
    ProcessMemory::default()
}

fn json_option(value: Option<u64>) -> String {
    value.map_or_else(|| "null".to_string(), |value| value.to_string())
}

fn json_events(events: AarmAllocatorEvents) -> String {
    format!(
        "{{\"active_page_fast_path_allocations\":{},\"slow_path_allocations\":{},\
         \"inactive_page_reuse_events\":{},\"fresh_regular_page_allocations\":{},\
         \"fresh_oversized_page_allocations\":{},\"rewind_events\":{},\
         \"rewound_bytes\":{},\"allocation_limit_denials\":{}}}",
        events.active_page_fast_path_allocations,
        events.slow_path_allocations,
        events.inactive_page_reuse_events,
        events.fresh_regular_page_allocations,
        events.fresh_oversized_page_allocations,
        events.rewind_events,
        events.rewound_bytes,
        events.allocation_limit_denials,
    )
}

fn json_rewind(rewind: Option<aster_runtime::AarmRewindTelemetry>) -> String {
    rewind.map_or_else(
        || "null".to_string(),
        |rewind| {
            format!(
                "{{\"live_used_bytes_before\":{},\"live_used_bytes_after\":{},\
                 \"arena_capacity_bytes_before\":{},\"arena_capacity_bytes_after\":{},\
                 \"active_page_capacity_bytes_after\":{},\
                 \"inactive_page_capacity_bytes_after\":{}}}",
                rewind.live_used_bytes_before,
                rewind.live_used_bytes_after,
                rewind.arena_capacity_bytes_before,
                rewind.arena_capacity_bytes_after,
                rewind.active_page_capacity_bytes_after,
                rewind.inactive_page_capacity_bytes_after,
            )
        },
    )
}

fn json_region(region: AarmRegionTelemetry) -> String {
    format!(
        "{{\"live_used_bytes\":{},\"arena_capacity_bytes\":{},\
         \"peak_live_used_bytes\":{},\"peak_arena_capacity_bytes\":{},\
         \"active_page_capacity_bytes\":{},\"inactive_page_capacity_bytes\":{},\
         \"page_count\":{},\"active_page_count\":{},\"inactive_page_count\":{},\
         \"events\":{},\"last_rewind\":{}}}",
        region.live_used_bytes,
        region.arena_capacity_bytes,
        region.peak_live_used_bytes,
        region.peak_arena_capacity_bytes,
        region.active_page_capacity_bytes,
        region.inactive_page_capacity_bytes,
        region.page_count,
        region.active_page_count,
        region.inactive_page_count,
        json_events(region.events),
        json_rewind(region.last_rewind),
    )
}

fn json_governor(governor: Option<MemoryGovernorTelemetry>) -> String {
    governor.map_or_else(
        || "null".to_string(),
        |governor| {
            format!(
                "{{\"hard_limit_bytes\":{},\"current_capacity_bytes\":{},\
                 \"peak_capacity_bytes\":{},\"grant_events\":{},\"denial_events\":{},\
                 \"release_events\":{},\"granted_bytes_cumulative\":{},\
                 \"released_bytes_cumulative\":{}}}",
                governor.hard_limit_bytes,
                governor.current_capacity_bytes,
                governor.peak_capacity_bytes,
                governor.grant_events,
                governor.denial_events,
                governor.release_events,
                governor.granted_bytes_cumulative,
                governor.released_bytes_cumulative,
            )
        },
    )
}

fn json_parallel_plans(plans: &[AarmParallelPlanningTelemetry]) -> String {
    let mut output = String::from("[");
    for (index, plan) in plans.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        let budgets = plan
            .chunk_budgets_bytes
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        write!(
            output,
            "{{\"operation\":\"{}\",\"initial_governor_capacity_bytes\":{},\
             \"available_headroom_bytes\":{},\"chunk_count\":{},\
             \"min_chunk_budget_bytes\":{},\"max_chunk_budget_bytes\":{},\
             \"chunk_budgets_bytes\":[{}]}}",
            plan.operation,
            plan.initial_governor_capacity_bytes,
            plan.available_headroom_bytes,
            plan.chunk_budgets_bytes.len(),
            json_option(plan.chunk_budgets_bytes.iter().min().copied()),
            json_option(plan.chunk_budgets_bytes.iter().max().copied()),
            budgets,
        )
        .expect("writing into a String cannot fail");
    }
    output.push(']');
    output
}

fn json_task_domain(domain: Option<AarmTaskMemoryDomainTelemetry>) -> String {
    domain.map_or_else(
        || "null".to_string(),
        |domain| {
            format!(
                "{{\"initial_governor_capacity_bytes\":{},\
                 \"available_headroom_bytes\":{},\"main_retained_capacity_bytes\":{},\
                 \"main_future_growth_bytes\":{},\"main_local_capacity_ceiling_bytes\":{},\
                 \"task_context_ceiling_bytes\":{},\"task_memory_concurrency_limit\":{},\
                 \"task_submissions\":{},\"task_contexts_started\":{},\
                 \"task_contexts_completed\":{},\"task_context_memory_failures\":{},\
                 \"active_page_fast_path_allocations\":{},\"fresh_page_allocations\":{}}}",
                domain.initial_governor_capacity_bytes,
                domain.available_headroom_bytes,
                domain.main_retained_capacity_bytes,
                domain.main_future_growth_bytes,
                domain.main_local_capacity_ceiling_bytes,
                domain.task_context_ceiling_bytes,
                domain.task_memory_concurrency_limit,
                domain.task_submissions,
                domain.task_contexts_started,
                domain.task_contexts_completed,
                domain.task_context_memory_failures,
                domain.active_page_fast_path_allocations,
                domain.fresh_page_allocations,
            )
        },
    )
}

fn json_async_domain(domain: Option<AarmAsyncMemoryDomainTelemetry>) -> String {
    domain.map_or_else(
        || "null".to_string(),
        |domain| {
            format!(
                "{{\"initial_governor_capacity_bytes\":{},\
                 \"available_headroom_bytes\":{},\"main_retained_capacity_bytes\":{},\
                 \"main_future_growth_bytes\":{},\"main_local_capacity_ceiling_bytes\":{},\
                 \"move_next_context_ceiling_bytes\":{},\
                 \"awaited_inner_context_ceiling_bytes\":{},\
                 \"temporal_borrowing_enabled\":{},\"phase_context_ceiling_bytes\":{},\
                 \"async_handles_created\":{},\"move_next_contexts_started\":{},\
                 \"move_next_contexts_completed\":{},\"inner_contexts_started\":{},\
                 \"inner_contexts_completed\":{},\"move_next_memory_failures\":{},\
                 \"inner_memory_failures\":{},\"move_next_fast_path_allocations\":{},\
                 \"move_next_fresh_page_allocations\":{},\
                 \"inner_fast_path_allocations\":{},\"inner_fresh_page_allocations\":{},\
                 \"phase_wait_events\":{},\"phase_borrowed_contexts\":{},\
                 \"peak_simultaneous_governed_async_contexts\":{}}}",
                domain.initial_governor_capacity_bytes,
                domain.available_headroom_bytes,
                domain.main_retained_capacity_bytes,
                domain.main_future_growth_bytes,
                domain.main_local_capacity_ceiling_bytes,
                domain.move_next_context_ceiling_bytes,
                domain.awaited_inner_context_ceiling_bytes,
                domain.temporal_borrowing_enabled,
                domain.phase_context_ceiling_bytes,
                domain.async_handles_created,
                domain.move_next_contexts_started,
                domain.move_next_contexts_completed,
                domain.inner_contexts_started,
                domain.inner_contexts_completed,
                domain.move_next_memory_failures,
                domain.inner_memory_failures,
                domain.move_next_fast_path_allocations,
                domain.move_next_fresh_page_allocations,
                domain.inner_fast_path_allocations,
                domain.inner_fresh_page_allocations,
                domain.phase_wait_events,
                domain.phase_borrowed_contexts,
                domain.peak_simultaneous_governed_async_contexts,
            )
        },
    )
}

fn json_case(result: &CaseResult) -> String {
    let workers = result
        .workers
        .map_or_else(|| "null".to_string(), |workers| workers.to_string());
    format!(
        "{{\"workload\":\"{}\",\"scale\":\"{}\",\"iterations\":{},\"workers\":{},\
         \"checksum\":{},\"elapsed_micros\":{},\"requested_bytes\":{},\
         \"temporary\":{},\"persistent\":{},\"total\":{},\"governor\":{},\
         \"parallel_plans\":{},\"task_memory_domain\":{},\"async_memory_domain\":{},\
         \"process_rss_bytes\":{{\"before\":{},\"at_peak\":{},\"after\":{},\
         \"process_peak\":{}}}}}",
        result.workload,
        result.scale.as_str(),
        result.iterations,
        workers,
        result.checksum,
        result.elapsed_micros,
        result.telemetry.requested_bytes,
        json_region(result.telemetry.temporary),
        json_region(result.telemetry.persistent),
        json_region(result.telemetry.total),
        json_governor(result.telemetry.governor),
        json_parallel_plans(&result.parallel_plans),
        json_task_domain(result.task_domain),
        json_async_domain(result.async_domain),
        json_option(result.rss_before_bytes),
        json_option(result.rss_at_peak_bytes),
        json_option(result.rss_after_bytes),
        json_option(result.process_peak_rss_bytes),
    )
}

#[must_use]
pub fn serialize_results(results: &[CaseResult]) -> String {
    let mut output = String::from("{\"schema_version\":6,\"results\":[");
    for (index, result) in results.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        output.push_str(&json_case(result));
    }
    output.push_str("]}");
    output
}

fn selected_scales() -> Vec<Scale> {
    let mut selected = Vec::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let value = if let Some(value) = argument.strip_prefix("--scale=") {
            Some(value.to_string())
        } else if argument == "--scale" {
            arguments.next()
        } else {
            None
        };
        if let Some(value) = value {
            for token in value.split(',') {
                if token == "all" {
                    return vec![Scale::Small, Scale::Medium, Scale::Large];
                }
                if let Some(scale) = Scale::parse(token) {
                    if !selected.contains(&scale) {
                        selected.push(scale);
                    }
                }
            }
        }
    }
    if selected.is_empty() {
        vec![Scale::Small, Scale::Medium]
    } else {
        selected
    }
}

fn main() {
    let results = run_matrix(&selected_scales());
    if std::env::args().any(|argument| argument == "--json") {
        println!("{}", serialize_results(&results));
        return;
    }
    for result in &results {
        let mut label = result.workload.to_string();
        if let Some(workers) = result.workers {
            write!(label, "_{workers}").expect("writing a String cannot fail");
        }
        let governor = result.telemetry.governor.map_or_else(
            || "none".to_string(),
            |governor| {
                format!(
                    "{}/{} grants={} denials={} releases={}",
                    governor.current_capacity_bytes,
                    governor.hard_limit_bytes,
                    governor.grant_events,
                    governor.denial_events,
                    governor.release_events
                )
            },
        );
        let planning = result.parallel_plans.first().map_or_else(
            || "none".to_string(),
            |plan| {
                format!(
                    "headroom={} chunks={} min={} max={}",
                    plan.available_headroom_bytes,
                    plan.chunk_budgets_bytes.len(),
                    plan.chunk_budgets_bytes.iter().min().copied().unwrap_or(0),
                    plan.chunk_budgets_bytes.iter().max().copied().unwrap_or(0),
                )
            },
        );
        println!(
            "workload={label:<36} scale={:<6} elapsed_ms={:>5}.{:03} requested={:>10} \
             final_used={:>10} peak_used={:>10} capacity={:>10} fast={:>8} slow={:>8} \
             reuse={:>5} rewinds={:>5} governor={governor} plan={planning} rss_peak={}",
            result.scale.as_str(),
            result.elapsed_micros / 1000,
            result.elapsed_micros % 1000,
            result.telemetry.requested_bytes,
            result.telemetry.total.live_used_bytes,
            result.telemetry.total.peak_live_used_bytes,
            result.telemetry.total.arena_capacity_bytes,
            result
                .telemetry
                .total
                .events
                .active_page_fast_path_allocations,
            result.telemetry.total.events.slow_path_allocations,
            result.telemetry.total.events.inactive_page_reuse_events,
            result.telemetry.total.events.rewind_events,
            result
                .rss_at_peak_bytes
                .or(result.process_peak_rss_bytes)
                .map_or_else(|| "unavailable".to_string(), |value| value.to_string()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_status_parser_keeps_current_and_peak_rss_distinct() {
        let memory = parse_linux_status("Name:\ttest\nVmHWM:\t2048 kB\nVmRSS:\t1024 kB\n");
        assert_eq!(memory.rss_bytes, Some(1024 * 1024));
        assert_eq!(memory.peak_rss_bytes, Some(2048 * 1024));
    }

    #[test]
    fn structured_output_advertises_the_async_domain_schema() {
        assert_eq!(
            serialize_results(&[]),
            "{\"schema_version\":6,\"results\":[]}"
        );
    }
}
