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

use aster_codegen_cranelift::{AarmMemoryTelemetry, ExecutionValue, execute_with_aarm_telemetry};
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
        rss_before_bytes: before.rss_bytes,
        rss_at_peak_bytes: at_peak.rss_bytes,
        rss_after_bytes: after.rss_bytes,
        process_peak_rss_bytes: after.peak_rss_bytes,
    }
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

fn json_case(result: &CaseResult) -> String {
    let workers = result
        .workers
        .map_or_else(|| "null".to_string(), |workers| workers.to_string());
    format!(
        "{{\"workload\":\"{}\",\"scale\":\"{}\",\"iterations\":{},\"workers\":{},\
         \"checksum\":{},\"elapsed_micros\":{},\"requested_bytes\":{},\
         \"temporary\":{},\"persistent\":{},\"total\":{},\"governor\":{},\
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
        json_option(result.rss_before_bytes),
        json_option(result.rss_at_peak_bytes),
        json_option(result.rss_after_bytes),
        json_option(result.process_peak_rss_bytes),
    )
}

#[must_use]
pub fn serialize_results(results: &[CaseResult]) -> String {
    let mut output = String::from("{\"schema_version\":2,\"results\":[");
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
        println!(
            "workload={label:<36} scale={:<6} elapsed_ms={:>5}.{:03} requested={:>10} \
             final_used={:>10} peak_used={:>10} capacity={:>10} fast={:>8} slow={:>8} \
             reuse={:>5} rewinds={:>5} governor={governor} rss_peak={}",
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn linux_status_parser_keeps_current_and_peak_rss_distinct() {
        let memory = parse_linux_status("Name:\ttest\nVmHWM:\t2048 kB\nVmRSS:\t1024 kB\n");
        assert_eq!(memory.rss_bytes, Some(1024 * 1024));
        assert_eq!(memory.peak_rss_bytes, Some(2048 * 1024));
    }
}
