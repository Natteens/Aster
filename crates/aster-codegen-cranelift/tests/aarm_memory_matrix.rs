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
    assert_eq!(results.len(), 9);
    for workload in [
        "tiny_allocations",
        "long_scope_temporary",
        "helper_scoped_temporary",
        "temporary_burst_rewind",
        "temporary_burst_rewind_reuse",
        "persistent_control",
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
    }
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
    assert!(json.starts_with("{\"schema_version\":1,"));
    assert!(json.contains("\"requested_bytes\""));
    assert!(json.contains("\"arena_capacity_bytes\""));
    assert!(json.contains("\"last_rewind\""));
    assert!(json.contains("\"process_rss_bytes\""));
    assert!(!json.contains("virtual_reserved_bytes"));
    assert!(!json.contains("committed_backing_bytes"));
    assert!(!json.contains("purge_events"));
    assert_eq!(json.matches("\"workload\":").count(), 9);
}

#[test]
#[cfg(any(windows, target_os = "linux"))]
fn supported_hosts_report_current_and_peak_process_rss() {
    let memory = matrix::process_memory();
    assert!(memory.rss_bytes.is_some());
    assert!(memory.peak_rss_bytes.is_some());
}
