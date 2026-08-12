#![cfg(feature = "aarm-telemetry")]

#[allow(dead_code)]
#[path = "../examples/aarm_memory_matrix.rs"]
mod matrix;

use std::sync::OnceLock;

use matrix::{CaseResult, Scale, run_matrix, serialize_results};

fn small_results() -> &'static [CaseResult] {
    static RESULTS: OnceLock<Vec<CaseResult>> = OnceLock::new();
    RESULTS.get_or_init(|| run_matrix(&[Scale::Small]))
}

#[test]
fn small_matrix_covers_required_workload_shapes() {
    let results = small_results();
    assert_eq!(results.len(), 39);
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
    ] {
        assert!(results.iter().any(|result| result.workload == workload));
    }
    let worker_counts = results
        .iter()
        .filter(|result| result.workload == "worker_contexts")
        .map(|result| result.workers.expect("worker result has a count"))
        .collect::<Vec<_>>();
    assert_eq!(worker_counts, [1, 4, 16]);
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
        }
        assert_eq!(
            telemetry.total.live_used_bytes,
            telemetry.temporary.live_used_bytes + telemetry.persistent.live_used_bytes
        );
        assert_eq!(
            telemetry.total.arena_capacity_bytes,
            telemetry.temporary.arena_capacity_bytes + telemetry.persistent.arena_capacity_bytes
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
    let json = serialize_results(small_results());
    assert!(json.starts_with("{\"schema_version\":3,"));
    assert!(json.contains("\"requested_bytes\""));
    assert!(json.contains("\"arena_capacity_bytes\""));
    assert!(json.contains("\"last_rewind\""));
    assert!(json.contains("\"process_rss_bytes\""));
    assert!(json.contains("\"current_capacity_bytes\""));
    assert!(json.contains("\"grant_events\""));
    assert!(json.contains("\"denial_events\""));
    assert!(json.contains("\"release_events\""));
    assert!(json.contains("\"parallel_plans\""));
    assert!(json.contains("\"available_headroom_bytes\""));
    assert!(json.contains("\"chunk_budgets_bytes\""));
    assert!(!json.contains("virtual_reserved_bytes"));
    assert!(!json.contains("committed_backing_bytes"));
    assert!(!json.contains("purge_events"));
    assert_eq!(json.matches("\"workload\":").count(), 39);
}

#[test]
#[cfg(any(windows, target_os = "linux"))]
fn supported_hosts_report_current_and_peak_process_rss() {
    let memory = matrix::process_memory();
    assert!(memory.rss_bytes.is_some());
    assert!(memory.peak_rss_bytes.is_some());
}
