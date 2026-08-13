#![cfg(feature = "aarm-telemetry")]

#[allow(dead_code)]
#[path = "../examples/aarm_memory_matrix.rs"]
mod matrix;

use std::sync::OnceLock;

use aster_runtime::ExecutionContext;
use matrix::{
    CaseResult, Scale, run_matrix, serialize_results_with_budget,
    serialize_results_with_host_capacity,
};

fn small_results() -> &'static [CaseResult] {
    static RESULTS: OnceLock<Vec<CaseResult>> = OnceLock::new();
    RESULTS.get_or_init(|| run_matrix(&[Scale::Small]))
}

#[test]
fn small_matrix_covers_required_workload_shapes() {
    let results = small_results();
    assert_eq!(results.len(), 119);
    for workload in [
        "tiny_allocations",
        "long_scope_temporary",
        "helper_scoped_temporary",
        "temporary_burst_rewind",
        "temporary_burst_rewind_reuse",
        "persistent_control",
        "direct_tiny_allocations_control",
        "governed_tiny_allocations",
        "page_growth_control",
        "governed_page_growth",
        "governed_contexts_1",
        "governed_contexts_4",
        "governed_contexts_16",
        "shared_governor_denial",
        "governor_teardown_reuse",
        "parallel_for_control",
        "governed_parallel_for",
        "parallel_for_each_control",
        "governed_parallel_for_each",
        "parallel_reduce_control",
        "governed_parallel_reduce",
        "governed_parallel_tight_partition",
        "governed_parallel_uneven_chunks",
        "governed_parallel_deterministic_denial",
        "task_empty_control",
        "governed_task_empty",
        "task_small_allocation_control",
        "governed_task_small_allocation",
        "task_moderate_allocation_control",
        "governed_task_moderate_allocation",
        "task_swarm_control",
        "governed_task_swarm",
        "governed_task_more_than_workers",
        "governed_task_main_worker_growth",
        "governed_task_teardown_reuse",
        "governed_task_tight_page_domain",
        "governed_task_deterministic_denial",
        "async_trivial_control",
        "governed_async_trivial",
        "async_before_await_control",
        "governed_async_before_await",
        "async_inner_control",
        "governed_async_inner",
        "async_after_await_control",
        "governed_async_after_await",
        "async_multiple_handles_control",
        "governed_async_multiple_handles",
        "governed_async_tight_three_page_domain",
        "governed_async_inner_denial",
        "governed_async_move_next_denial",
        "governed_async_repeated_wait",
        "governed_async_temporal_before_await",
        "governed_async_temporal_inner",
        "governed_async_temporal_after_await",
        "budget_governed_plain",
        "budget_governed_parallel",
        "budget_governed_task",
        "budget_governed_async",
    ] {
        assert!(results.iter().any(|result| result.workload == workload));
    }
    let worker_counts = results
        .iter()
        .filter(|result| result.workload == "worker_contexts")
        .map(|result| result.workers.expect("worker result has a count"))
        .collect::<Vec<_>>();
    assert_eq!(worker_counts, [1, 2, 4, 8, 16]);
}

#[test]
fn governed_async_domains_are_frozen_page_aware_and_quiescent() {
    let results = small_results();
    for result in results
        .iter()
        .filter(|result| result.async_domain.is_some() && !result.workload.starts_with("budget_"))
    {
        let domain = result.async_domain.expect("async domain exists");
        assert!(domain.temporal_borrowing_enabled);
        assert_eq!(
            domain.move_next_context_ceiling_bytes,
            domain.phase_context_ceiling_bytes
        );
        assert_eq!(
            domain.awaited_inner_context_ceiling_bytes,
            domain.phase_context_ceiling_bytes
        );
        assert!(domain.phase_context_ceiling_bytes <= domain.available_headroom_bytes);
        assert_eq!(
            domain.move_next_contexts_started,
            domain.move_next_contexts_completed
        );
        assert_eq!(
            domain.inner_contexts_started,
            domain.inner_contexts_completed
        );
        let governor = result
            .telemetry
            .governor
            .expect("governed async reports governor telemetry");
        assert_eq!(governor.current_capacity_bytes, 0);
        assert!(governor.peak_capacity_bytes <= governor.hard_limit_bytes);
        assert_eq!(governor.grant_events, governor.release_events);
        assert_eq!(
            governor.granted_bytes_cumulative,
            governor.released_bytes_cumulative
        );
    }

    let page = ExecutionContext::AARM_MIN_PAGE_CAPACITY_BYTES as u64;
    let tight = results
        .iter()
        .find(|result| result.workload == "governed_async_tight_three_page_domain")
        .expect("tight async case exists")
        .async_domain
        .expect("tight async case reports its plan");
    assert_eq!(tight.move_next_context_ceiling_bytes, 3 * page);
    assert_eq!(tight.awaited_inner_context_ceiling_bytes, 3 * page);
    assert_eq!(tight.phase_context_ceiling_bytes, 3 * page);
    assert_eq!(tight.main_future_growth_bytes, page);

    let repeated = results
        .iter()
        .find(|result| result.workload == "governed_async_repeated_wait")
        .expect("repeated Wait case exists")
        .async_domain
        .expect("repeated Wait reports its domain");
    assert_eq!(repeated.async_handles_created, 1);
    assert_eq!(repeated.move_next_contexts_started, 2);
    assert_eq!(repeated.inner_contexts_started, 1);

    for workload in [
        "governed_async_inner_denial",
        "governed_async_move_next_denial",
    ] {
        let governor = results
            .iter()
            .find(|result| result.workload == workload)
            .expect("async denial case exists")
            .telemetry
            .governor
            .expect("async denial reports governor telemetry");
        assert_eq!(governor.current_capacity_bytes, 0);
        assert!(governor.peak_capacity_bytes <= governor.hard_limit_bytes);
    }
}

#[test]
fn every_region_and_total_obey_allocator_invariants() {
    for result in small_results() {
        let telemetry = result.telemetry;
        for region in [telemetry.temporary, telemetry.persistent, telemetry.total] {
            assert!(region.live_used_bytes <= region.arena_capacity_bytes);
            assert!(region.peak_live_used_bytes >= region.live_used_bytes);
            assert!(region.peak_arena_capacity_bytes >= region.arena_capacity_bytes);
            assert_eq!(
                region.arena_capacity_bytes,
                region.active_page_capacity_bytes + region.inactive_page_capacity_bytes
            );
            assert_eq!(
                region.page_count,
                region.active_page_count + region.inactive_page_count
            );
            match (
                region.virtual_extent_bytes,
                region.backing_retained_bytes,
                region.backing_discarded_bytes,
                region.peak_backing_retained_bytes,
            ) {
                (Some(virtual_extent), Some(retained), Some(discarded), Some(peak_retained)) => {
                    assert_eq!(retained.checked_add(discarded), Some(virtual_extent));
                    assert!(peak_retained >= retained);
                }
                (None, None, None, None) => {}
                values => panic!("partial VM backing telemetry is invalid: {values:?}"),
            }
        }
        assert_eq!(
            telemetry.total.live_used_bytes,
            telemetry.temporary.live_used_bytes + telemetry.persistent.live_used_bytes
        );
        assert_eq!(
            telemetry.total.arena_capacity_bytes,
            telemetry.temporary.arena_capacity_bytes + telemetry.persistent.arena_capacity_bytes
        );
        assert_eq!(
            telemetry.total.virtual_extent_bytes,
            sum_known(
                telemetry.temporary.virtual_extent_bytes,
                telemetry.persistent.virtual_extent_bytes,
            )
        );
        assert_eq!(
            telemetry.total.backing_retained_bytes,
            sum_known(
                telemetry.temporary.backing_retained_bytes,
                telemetry.persistent.backing_retained_bytes,
            )
        );
        assert_eq!(
            telemetry.total.backing_discarded_bytes,
            sum_known(
                telemetry.temporary.backing_discarded_bytes,
                telemetry.persistent.backing_discarded_bytes,
            )
        );
        if let Some(governor) = telemetry.governor {
            assert!(governor.current_capacity_bytes <= governor.hard_limit_bytes);
            assert!(governor.peak_capacity_bytes >= governor.current_capacity_bytes);
            assert_eq!(
                governor.granted_bytes_cumulative - governor.released_bytes_cumulative,
                governor.current_capacity_bytes
            );
        }
    }
}

#[test]
fn governed_cases_report_shared_admission_denial_and_release() {
    let results = small_results();
    for context_count in [1_u64, 4, 16] {
        let workload = format!("governed_contexts_{context_count}");
        let result = results
            .iter()
            .find(|result| result.workload == workload)
            .expect("governed context result exists");
        let governor = result
            .telemetry
            .governor
            .expect("governed result has governor telemetry");
        assert_eq!(governor.grant_events, context_count);
        assert_eq!(governor.denial_events, 0);
        assert_eq!(
            governor.current_capacity_bytes,
            result.telemetry.total.arena_capacity_bytes
        );
    }

    let denial = results
        .iter()
        .find(|result| result.workload == "shared_governor_denial")
        .expect("shared denial result exists")
        .telemetry
        .governor
        .expect("shared denial has governor telemetry");
    assert_eq!(denial.grant_events, 1);
    assert_eq!(denial.denial_events, 1);
    assert_eq!(denial.current_capacity_bytes, denial.hard_limit_bytes);

    let teardown = results
        .iter()
        .find(|result| result.workload == "governor_teardown_reuse")
        .expect("teardown result exists")
        .telemetry
        .governor
        .expect("teardown result has governor telemetry");
    assert_eq!(teardown.grant_events, 2);
    assert_eq!(teardown.release_events, 1);
    assert_eq!(teardown.current_capacity_bytes, teardown.hard_limit_bytes);
}

#[test]
fn governed_parallel_plans_are_exact_and_bound_to_logical_chunk_count() {
    let results = small_results();
    for result in results
        .iter()
        .filter(|result| result.workload.starts_with("governed_parallel"))
    {
        for plan in &result.parallel_plans {
            assert_eq!(
                plan.chunk_budgets_bytes.iter().sum::<u64>(),
                plan.available_headroom_bytes
            );
        }
        let governor = result
            .telemetry
            .governor
            .expect("governed Parallel cases report governor telemetry");
        assert!(governor.current_capacity_bytes <= governor.hard_limit_bytes);
        assert!(governor.peak_capacity_bytes <= governor.hard_limit_bytes);
    }

    let uneven = results
        .iter()
        .find(|result| result.workload == "governed_parallel_uneven_chunks")
        .expect("uneven partition case exists");
    assert_eq!(
        uneven.parallel_plans[0].chunk_budgets_bytes,
        [10_241, 10_241, 10_240, 10_240]
    );

    let denial = results
        .iter()
        .find(|result| result.workload == "governed_parallel_deterministic_denial")
        .expect("deterministic denial case exists");
    assert_eq!(denial.checksum, 2);
    let governor = denial.telemetry.governor.expect("denial has governor data");
    assert_eq!(governor.current_capacity_bytes, 0);
    assert_eq!(governor.grant_events, governor.release_events);
}

#[test]
fn governed_task_domains_are_frozen_page_aware_and_quiescent() {
    let results = small_results();
    for result in results
        .iter()
        .filter(|result| result.workload.starts_with("governed_task"))
    {
        let domain = result
            .task_domain
            .expect("governed Task.Run cases report their frozen domain");
        assert!(domain.task_memory_concurrency_limit > 0);
        assert_eq!(domain.task_submissions, domain.task_contexts_started);
        assert_eq!(domain.task_contexts_started, domain.task_contexts_completed);
        assert!(
            u128::from(domain.main_future_growth_bytes)
                + u128::from(domain.task_context_ceiling_bytes)
                    * u128::try_from(domain.task_memory_concurrency_limit)
                        .expect("concurrency fits u128")
                <= u128::from(domain.available_headroom_bytes)
        );
        let governor = result
            .telemetry
            .governor
            .expect("governed Task.Run cases report governor telemetry");
        assert_eq!(governor.current_capacity_bytes, 0);
        assert!(governor.peak_capacity_bytes <= governor.hard_limit_bytes);
        assert_eq!(governor.grant_events, governor.release_events);
        assert_eq!(
            governor.granted_bytes_cumulative,
            governor.released_bytes_cumulative
        );
    }

    let tight = results
        .iter()
        .find(|result| result.workload == "governed_task_tight_page_domain")
        .expect("tight task domain case exists")
        .task_domain
        .expect("tight task domain reports planning");
    assert_eq!(tight.task_memory_concurrency_limit, 2);
    assert_eq!(tight.task_submissions, 8);

    let denial = results
        .iter()
        .find(|result| result.workload == "governed_task_deterministic_denial")
        .expect("task denial case exists")
        .task_domain
        .expect("task denial reports planning");
    assert_eq!(denial.task_context_memory_failures, 20);
}

#[test]
fn rewind_reuse_and_persistence_are_visible_without_policy_changes() {
    let results = small_results();
    let burst = results
        .iter()
        .find(|result| result.workload == "temporary_burst_rewind")
        .expect("burst result exists");
    assert_eq!(burst.telemetry.temporary.live_used_bytes, 0);
    assert!(burst.telemetry.temporary.arena_capacity_bytes > 0);
    assert!(burst.telemetry.temporary.events.rewind_events > 0);
    assert!(burst.telemetry.temporary.events.rewound_bytes > 0);

    let reuse = results
        .iter()
        .find(|result| result.workload == "temporary_burst_rewind_reuse")
        .expect("reuse result exists");
    assert!(reuse.telemetry.temporary.events.inactive_page_reuse_events > 0);
    assert_eq!(
        reuse
            .telemetry
            .temporary
            .events
            .fresh_oversized_page_allocations,
        1
    );

    let persistent = results
        .iter()
        .find(|result| result.workload == "persistent_control")
        .expect("persistent result exists");
    assert!(persistent.telemetry.persistent.live_used_bytes > 0);
    assert_eq!(persistent.telemetry.temporary.live_used_bytes, 0);
}

#[test]
fn structured_output_contains_no_future_aarm_claims() {
    let capacity = aster_codegen_cranelift::AarmHostMemoryCapacity {
        physical_total_bytes: Some(16 * 1024),
        environment_limit_bytes: Some(4 * 1024),
        effective_capacity_bytes: Some(4 * 1024),
        source: Some(
            aster_codegen_cranelift::AarmHostMemoryCapacitySource::PhysicalTotalAndEnvironmentLimit,
        ),
    };
    let json = serialize_results_with_host_capacity(small_results(), capacity);
    assert!(json.starts_with("{\"schema_version\":10,"));
    assert!(json.contains("\"host_memory_capacity\""));
    assert!(json.contains("\"memory_budget\""));
    assert!(json.contains("\"physical_total_bytes\":16384"));
    assert!(json.contains("\"environment_limit_bytes\":4096"));
    assert!(json.contains("\"effective_capacity_bytes\":4096"));
    assert!(json.contains("\"capacity_source\":\"physical_total_and_environment_limit\""));
    assert!(json.contains("\"requested_bytes\""));
    assert!(json.contains("\"arena_capacity_bytes\""));
    assert!(json.contains("\"virtual_extent_bytes\""));
    assert!(json.contains("\"backing_retained_bytes\""));
    assert!(json.contains("\"backing_discarded_bytes\""));
    assert!(json.contains("\"peak_backing_retained_bytes\""));
    assert!(json.contains("\"last_rewind\""));
    assert!(json.contains("\"process_rss_bytes\""));
    assert!(json.contains("\"current_capacity_bytes\""));
    assert!(json.contains("\"grant_events\""));
    assert!(json.contains("\"denial_events\""));
    assert!(json.contains("\"release_events\""));
    assert!(json.contains("\"parallel_plans\""));
    assert!(json.contains("\"available_headroom_bytes\""));
    assert!(json.contains("\"chunk_budgets_bytes\""));
    assert!(json.contains("\"task_memory_domain\""));
    assert!(json.contains("\"task_context_ceiling_bytes\""));
    assert!(json.contains("\"async_memory_domain\""));
    assert!(json.contains("\"move_next_context_ceiling_bytes\""));
    assert!(json.contains("\"awaited_inner_context_ceiling_bytes\""));
    assert!(json.contains("\"temporal_borrowing_enabled\""));
    assert!(json.contains("\"phase_context_ceiling_bytes\""));
    assert!(!json.contains("virtual_reserved_bytes"));
    assert!(!json.contains("committed_backing_bytes"));
    assert!(!json.contains("purge_events"));
    assert_eq!(json.matches("\"workload\":").count(), 119);

    let explicit = aster_codegen_cranelift::resolve_aarm_explicit_budget(64 * 1024 * 1024)
        .expect("explicit budget resolves");
    let explicit_json = serialize_results_with_budget(small_results(), None, Some(explicit));
    assert!(explicit_json.starts_with("{\"schema_version\":10,"));
    assert!(explicit_json.contains("\"host_memory_capacity\":null"));
    assert!(explicit_json.contains("\"source\":\"explicit\""));
    assert!(explicit_json.contains("\"requested_explicit_bytes\":67108864"));
    assert!(explicit_json.contains("\"resolved_hard_limit_bytes\":67108864"));
}

fn sum_known(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    left?.checked_add(right?)
}

#[test]
#[cfg(any(windows, target_os = "linux"))]
fn supported_hosts_report_current_and_peak_process_rss() {
    let memory = matrix::process_memory();
    assert!(memory.rss_bytes.is_some());
    assert!(memory.peak_rss_bytes.is_some());
}
