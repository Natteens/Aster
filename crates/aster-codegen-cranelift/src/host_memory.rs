//! Stable host memory-capacity discovery for experimental AARM research.
//!
//! This module observes fixed machine or environment ceilings once at execution
//! startup. It deliberately does not observe free memory, RSS, current cgroup
//! usage, or allocator state, and it has no allocation-path callers.

#[cfg(feature = "aarm-telemetry")]
use std::{fmt, sync::Arc};

/// The stable source or sources used to resolve an effective host capacity.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AarmHostMemoryCapacitySource {
    PhysicalTotal,
    EnvironmentLimit,
    PhysicalTotalAndEnvironmentLimit,
}

#[cfg(feature = "aarm-telemetry")]
impl AarmHostMemoryCapacitySource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PhysicalTotal => "physical_total",
            Self::EnvironmentLimit => "environment_limit",
            Self::PhysicalTotalAndEnvironmentLimit => "physical_total_and_environment_limit",
        }
    }
}

/// A best-effort, immutable snapshot of stable host capacity ceilings.
///
/// `physical_total_bytes` is host physical RAM. `environment_limit_bytes` is
/// a finite container/process hard limit when known. `effective_capacity_bytes`
/// is their conservative minimum when both exist; it is unavailable when neither
/// stable source can be discovered. None of these fields is current free memory,
/// RSS, arena capacity, or a governor budget.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AarmHostMemoryCapacity {
    pub physical_total_bytes: Option<u64>,
    pub environment_limit_bytes: Option<u64>,
    pub effective_capacity_bytes: Option<u64>,
    pub source: Option<AarmHostMemoryCapacitySource>,
}

/// The experimental frozen-budget policy for one top-level execution.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AarmBudgetPolicy {
    Auto,
    ExactBytes(u64),
}

/// The authority from which an experimental frozen budget was resolved.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AarmBudgetSource {
    Auto,
    Explicit,
}

impl AarmBudgetSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Explicit => "explicit",
        }
    }
}

/// Frozen experimental budget resolution for one top-level execution.
///
/// The resolved limit governs retained ASTER arena capacity. It is not process
/// RSS, committed backing, or a guarantee that the OS will admit every later
/// host allocation.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AarmAutoBudgetTelemetry {
    pub source: AarmBudgetSource,
    pub requested_explicit_bytes: Option<u64>,
    pub effective_capacity_bytes: Option<u64>,
    pub resolved_hard_limit_bytes: u64,
    pub address_width_clamped: bool,
    pub capacity_source: Option<AarmHostMemoryCapacitySource>,
}

/// Controlled failure to resolve an experimental Auto memory budget.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AarmAutoBudgetError {
    StableCapacityUnavailable,
    StableCapacityZero,
    ExplicitBudgetZero,
    ExplicitBudgetExceedsAddressableSize,
}

#[cfg(feature = "aarm-telemetry")]
impl fmt::Display for AarmAutoBudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StableCapacityUnavailable => formatter.write_str(
                "AARM Auto memory budget is unavailable because no stable host memory capacity could be discovered",
            ),
            Self::StableCapacityZero => formatter.write_str(
                "AARM Auto memory budget is unavailable because stable host memory capacity was zero",
            ),
            Self::ExplicitBudgetZero => {
                formatter.write_str("AARM explicit memory budget must be greater than zero")
            }
            Self::ExplicitBudgetExceedsAddressableSize => formatter.write_str(
                "AARM explicit memory budget exceeds the addressable process size",
            ),
        }
    }
}

#[cfg(feature = "aarm-telemetry")]
impl std::error::Error for AarmAutoBudgetError {}

/// Execution-owned frozen budget governor. Each participating context receives
/// clones of this one explicit governor authority.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
#[derive(Clone)]
pub struct AarmAutoGovernor {
    host_capacity: Option<AarmHostMemoryCapacity>,
    telemetry: AarmAutoBudgetTelemetry,
    governor: Arc<aster_runtime::MemoryGovernor>,
}

#[cfg(feature = "aarm-telemetry")]
impl AarmAutoGovernor {
    #[must_use]
    pub const fn host_capacity(&self) -> Option<AarmHostMemoryCapacity> {
        self.host_capacity
    }

    #[must_use]
    pub const fn telemetry(&self) -> AarmAutoBudgetTelemetry {
        self.telemetry
    }

    #[must_use]
    pub fn governor(&self) -> Arc<aster_runtime::MemoryGovernor> {
        Arc::clone(&self.governor)
    }
}

#[cfg(feature = "aarm-telemetry")]
impl AarmHostMemoryCapacity {
    fn from_candidates(
        physical_total_bytes: Option<u64>,
        environment_limit_bytes: Option<u64>,
    ) -> Self {
        let physical_total_bytes = physical_total_bytes.filter(|&bytes| bytes != 0);
        let environment_limit_bytes = environment_limit_bytes.filter(|&bytes| bytes != 0);
        let (effective_capacity_bytes, source) =
            match (physical_total_bytes, environment_limit_bytes) {
                (Some(physical), Some(environment)) => (
                    Some(physical.min(environment)),
                    Some(AarmHostMemoryCapacitySource::PhysicalTotalAndEnvironmentLimit),
                ),
                (Some(physical), None) => (
                    Some(physical),
                    Some(AarmHostMemoryCapacitySource::PhysicalTotal),
                ),
                (None, Some(environment)) => (
                    Some(environment),
                    Some(AarmHostMemoryCapacitySource::EnvironmentLimit),
                ),
                (None, None) => (None, None),
            };
        Self {
            physical_total_bytes,
            environment_limit_bytes,
            effective_capacity_bytes,
            source,
        }
    }
}

/// Resolves one exact Auto hard limit from an already-frozen capacity snapshot.
/// No page alignment or percentage policy is applied.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
pub fn resolve_aarm_auto_budget(
    capacity: AarmHostMemoryCapacity,
) -> Result<AarmAutoBudgetTelemetry, AarmAutoBudgetError> {
    resolve_aarm_auto_budget_for_usize_max(capacity, usize::MAX as u64)
}

#[cfg(feature = "aarm-telemetry")]
fn resolve_aarm_auto_budget_for_usize_max(
    capacity: AarmHostMemoryCapacity,
    usize_max: u64,
) -> Result<AarmAutoBudgetTelemetry, AarmAutoBudgetError> {
    let effective_capacity_bytes = capacity
        .effective_capacity_bytes
        .ok_or(AarmAutoBudgetError::StableCapacityUnavailable)?;
    if effective_capacity_bytes == 0 {
        return Err(AarmAutoBudgetError::StableCapacityZero);
    }
    let capacity_source = capacity
        .source
        .ok_or(AarmAutoBudgetError::StableCapacityUnavailable)?;
    let resolved_hard_limit_bytes = effective_capacity_bytes.min(usize_max);
    Ok(AarmAutoBudgetTelemetry {
        source: AarmBudgetSource::Auto,
        requested_explicit_bytes: None,
        effective_capacity_bytes: Some(effective_capacity_bytes),
        resolved_hard_limit_bytes,
        address_width_clamped: resolved_hard_limit_bytes != effective_capacity_bytes,
        capacity_source: Some(capacity_source),
    })
}

/// Resolves an exact caller-provided experimental hard limit without consulting
/// host capacity. Explicit values never clamp: they are either representable
/// exactly or rejected before execution.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
pub fn resolve_aarm_explicit_budget(
    requested_bytes: u64,
) -> Result<AarmAutoBudgetTelemetry, AarmAutoBudgetError> {
    resolve_aarm_explicit_budget_for_usize_max(requested_bytes, usize::MAX as u64)
}

#[cfg(feature = "aarm-telemetry")]
fn resolve_aarm_explicit_budget_for_usize_max(
    requested_bytes: u64,
    usize_max: u64,
) -> Result<AarmAutoBudgetTelemetry, AarmAutoBudgetError> {
    if requested_bytes == 0 {
        return Err(AarmAutoBudgetError::ExplicitBudgetZero);
    }
    if requested_bytes > usize_max {
        return Err(AarmAutoBudgetError::ExplicitBudgetExceedsAddressableSize);
    }
    Ok(AarmAutoBudgetTelemetry {
        source: AarmBudgetSource::Explicit,
        requested_explicit_bytes: Some(requested_bytes),
        effective_capacity_bytes: None,
        resolved_hard_limit_bytes: requested_bytes,
        address_width_clamped: false,
        capacity_source: None,
    })
}

/// Creates one frozen Auto governor from a supplied immutable snapshot.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
pub fn aarm_auto_governor_from_capacity(
    host_capacity: AarmHostMemoryCapacity,
) -> Result<AarmAutoGovernor, AarmAutoBudgetError> {
    let telemetry = resolve_aarm_auto_budget(host_capacity)?;
    let hard_limit = usize::try_from(telemetry.resolved_hard_limit_bytes)
        .expect("Auto budget was clamped to usize::MAX");
    Ok(AarmAutoGovernor {
        host_capacity: Some(host_capacity),
        telemetry,
        governor: Arc::new(aster_runtime::MemoryGovernor::new(hard_limit)),
    })
}

/// Creates one frozen explicit governor without querying host capacity.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
pub fn aarm_explicit_governor(
    requested_bytes: u64,
) -> Result<AarmAutoGovernor, AarmAutoBudgetError> {
    let telemetry = resolve_aarm_explicit_budget(requested_bytes)?;
    let hard_limit = usize::try_from(telemetry.resolved_hard_limit_bytes)
        .expect("explicit budget was checked against usize::MAX");
    Ok(AarmAutoGovernor {
        host_capacity: None,
        telemetry,
        governor: Arc::new(aster_runtime::MemoryGovernor::new(hard_limit)),
    })
}

/// Resolves one policy and creates its one execution-owned governor.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
pub fn aarm_governor_from_policy(
    policy: AarmBudgetPolicy,
) -> Result<AarmAutoGovernor, AarmAutoBudgetError> {
    aarm_governor_from_policy_with(policy, discover_aarm_host_memory_capacity)
}

#[cfg(feature = "aarm-telemetry")]
fn aarm_governor_from_policy_with(
    policy: AarmBudgetPolicy,
    discover: impl FnOnce() -> AarmHostMemoryCapacity,
) -> Result<AarmAutoGovernor, AarmAutoBudgetError> {
    match policy {
        AarmBudgetPolicy::Auto => aarm_auto_governor_from_capacity(discover()),
        AarmBudgetPolicy::ExactBytes(requested_bytes) => aarm_explicit_governor(requested_bytes),
    }
}

/// Captures stable host capacity facts once for an experimental AARM execution.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
#[must_use]
pub fn discover_aarm_host_memory_capacity() -> AarmHostMemoryCapacity {
    platform_capacity()
}

/// Discovers once, resolves once, and freezes one Auto governor for a single
/// experimental top-level execution.
#[cfg(feature = "aarm-telemetry")]
#[doc(hidden)]
pub fn discover_aarm_auto_governor() -> Result<AarmAutoGovernor, AarmAutoBudgetError> {
    aarm_governor_from_policy(AarmBudgetPolicy::Auto)
}

#[cfg(feature = "aarm-telemetry")]
#[cfg(test)]
fn discover_aarm_auto_governor_with(
    discover: impl FnOnce() -> AarmHostMemoryCapacity,
) -> Result<AarmAutoGovernor, AarmAutoBudgetError> {
    aarm_governor_from_policy_with(AarmBudgetPolicy::Auto, discover)
}

#[cfg(feature = "aarm-telemetry")]
#[cfg(windows)]
fn platform_capacity() -> AarmHostMemoryCapacity {
    AarmHostMemoryCapacity::from_candidates(windows_physical_total(), None)
}

#[cfg(feature = "aarm-telemetry")]
#[cfg(target_os = "linux")]
fn platform_capacity() -> AarmHostMemoryCapacity {
    let physical = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|contents| parse_linux_mem_total(&contents));
    let environment = linux_cgroup_memory_limit();
    AarmHostMemoryCapacity::from_candidates(physical, environment)
}

#[cfg(feature = "aarm-telemetry")]
#[cfg(not(any(windows, target_os = "linux")))]
fn platform_capacity() -> AarmHostMemoryCapacity {
    AarmHostMemoryCapacity::default()
}

#[cfg(windows)]
fn windows_physical_total() -> Option<u64> {
    use std::mem::{MaybeUninit, size_of};
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MaybeUninit::<MEMORYSTATUSEX>::zeroed();
    // SAFETY: the output is writable storage for the exact API structure, and
    // `dwLength` advertises its initialized size before the FFI call.
    #[allow(unsafe_code)]
    unsafe {
        (*status.as_mut_ptr()).dwLength = u32::try_from(size_of::<MEMORYSTATUSEX>()).ok()?;
    }
    // SAFETY: `status` points to initialized writable storage with the required
    // advertised size. No pointer escapes this function.
    #[allow(unsafe_code)]
    let success = unsafe { GlobalMemoryStatusEx(status.as_mut_ptr()) };
    if success == 0 {
        return None;
    }
    // SAFETY: a successful `GlobalMemoryStatusEx` initializes the full structure.
    #[allow(unsafe_code)]
    let status = unsafe { status.assume_init() };
    Some(status.ullTotalPhys).filter(|&bytes| bytes != 0)
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_mem_total(contents: &str) -> Option<u64> {
    let line = contents.lines().find(|line| {
        line.split_once(':')
            .is_some_and(|(name, _)| name == "MemTotal")
    })?;
    let (_, value) = line.split_once(':')?;
    let mut fields = value.split_whitespace();
    let kib = fields.next()?.parse::<u64>().ok()?;
    let unit = fields.next()?;
    if !unit.eq_ignore_ascii_case("kb") || fields.next().is_some() {
        return None;
    }
    kib.checked_mul(1024).filter(|&bytes| bytes != 0)
}

#[cfg(target_os = "linux")]
fn linux_cgroup_memory_limit() -> Option<u64> {
    let process_cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    discover_linux_cgroup_memory_limit(&process_cgroup, &mountinfo, |path| {
        std::fs::read_to_string(path).ok()
    })
}

#[cfg(any(target_os = "linux", test))]
fn discover_linux_cgroup_memory_limit(
    process_cgroup: &str,
    mountinfo: &str,
    read_file: impl FnMut(&str) -> Option<String>,
) -> Option<u64> {
    let mounts = parse_cgroup_mounts(mountinfo);
    let cgroups = parse_process_cgroups(process_cgroup);
    let mut read_file = read_file;

    if let Some(path) = cgroups.v2.as_deref() {
        for mount in mounts.iter().filter(|mount| mount.kind == CgroupKind::V2) {
            if let Some(directory) = resolve_cgroup_directory(mount, path)
                && let Some(limit) = hierarchy_limit(
                    &directory,
                    &mount.mount_point,
                    "memory.max",
                    &mut read_file,
                    parse_cgroup_v2_memory_max,
                )
            {
                return Some(limit);
            }
        }
    }

    let v1_path = cgroups.v1_memory.as_deref()?;
    for mount in mounts
        .iter()
        .filter(|mount| mount.kind == CgroupKind::V1Memory)
    {
        if let Some(directory) = resolve_cgroup_directory(mount, v1_path)
            && let Some(limit) = hierarchy_limit(
                &directory,
                &mount.mount_point,
                "memory.limit_in_bytes",
                &mut read_file,
                parse_cgroup_v1_memory_limit,
            )
        {
            return Some(limit);
        }
    }
    None
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CgroupKind {
    V2,
    V1Memory,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct CgroupMount {
    kind: CgroupKind,
    root: String,
    mount_point: String,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Default)]
struct ProcessCgroups {
    v2: Option<String>,
    v1_memory: Option<String>,
}

#[cfg(any(target_os = "linux", test))]
fn parse_process_cgroups(contents: &str) -> ProcessCgroups {
    let mut result = ProcessCgroups::default();
    for line in contents.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some(hierarchy), Some(controllers), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some(path) = normalize_kernel_path(path) else {
            continue;
        };
        if hierarchy == "0" && controllers.is_empty() {
            result.v2 = Some(path);
        } else if controllers
            .split(',')
            .any(|controller| controller == "memory")
        {
            result.v1_memory = Some(path);
        }
    }
    result
}

#[cfg(any(target_os = "linux", test))]
fn parse_cgroup_mounts(contents: &str) -> Vec<CgroupMount> {
    contents.lines().filter_map(parse_cgroup_mount).collect()
}

#[cfg(any(target_os = "linux", test))]
fn parse_cgroup_mount(line: &str) -> Option<CgroupMount> {
    let (before_separator, after_separator) = line.split_once(" - ")?;
    let fields = before_separator.split_whitespace().collect::<Vec<_>>();
    let root = normalize_kernel_path(unescape_mountinfo_path(fields.get(3)?))?;
    let mount_point = normalize_kernel_path(unescape_mountinfo_path(fields.get(4)?))?;
    let mut after = after_separator.split_whitespace();
    let file_system = after.next()?;
    let _source = after.next()?;
    let super_options = after.next().unwrap_or_default();
    let kind = match file_system {
        "cgroup2" => CgroupKind::V2,
        "cgroup" if super_options.split(',').any(|option| option == "memory") => {
            CgroupKind::V1Memory
        }
        _ => return None,
    };
    Some(CgroupMount {
        kind,
        root,
        mount_point,
    })
}

#[cfg(any(target_os = "linux", test))]
fn unescape_mountinfo_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..=index + 3].iter().all(u8::is_ascii_digit)
        {
            let octal = &path[index + 1..index + 4];
            if let Ok(value) = u8::from_str_radix(octal, 8) {
                result.push(value);
                index += 4;
                continue;
            }
        }
        result.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(result).unwrap_or_default()
}

#[cfg(any(target_os = "linux", test))]
fn normalize_kernel_path(path: impl AsRef<str>) -> Option<String> {
    let path = path.as_ref();
    if !path.starts_with('/') {
        return None;
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" => {}
            "." | ".." => return None,
            component => components.push(component),
        }
    }
    Some(if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    })
}

#[cfg(any(target_os = "linux", test))]
fn resolve_cgroup_directory(mount: &CgroupMount, process_path: &str) -> Option<String> {
    let process_path = normalize_kernel_path(process_path)?;
    let relative = if mount.root == "/" {
        process_path.trim_start_matches('/')
    } else if process_path == mount.root {
        ""
    } else {
        process_path.strip_prefix(&format!("{}/", mount.root))?
    };
    Some(if relative.is_empty() {
        mount.mount_point.clone()
    } else {
        format!("{}/{}", mount.mount_point, relative)
    })
}

#[cfg(any(target_os = "linux", test))]
fn hierarchy_limit(
    directory: &str,
    mount_point: &str,
    file_name: &str,
    mut read_file: impl FnMut(&str) -> Option<String>,
    parse_limit: impl Fn(&str) -> Option<u64>,
) -> Option<u64> {
    let mut current = normalize_kernel_path(directory)?;
    let mount_point = normalize_kernel_path(mount_point)?;
    if current != mount_point && !current.starts_with(&format!("{mount_point}/")) {
        return None;
    }
    let mut smallest = None;
    loop {
        let limit_path = if current == "/" {
            format!("/{file_name}")
        } else {
            format!("{current}/{file_name}")
        };
        if let Some(limit) = read_file(&limit_path).and_then(|value| parse_limit(&value)) {
            smallest = Some(smallest.map_or(limit, |existing: u64| existing.min(limit)));
        }
        if current == mount_point {
            return smallest;
        }
        current = current.rsplit_once('/')?.0.to_string();
        if current.is_empty() {
            return None;
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_cgroup_v2_memory_max(contents: &str) -> Option<u64> {
    let value = contents.trim();
    if value == "max" {
        return None;
    }
    value.parse::<u64>().ok().filter(|&bytes| bytes != 0)
}

#[cfg(any(target_os = "linux", test))]
fn parse_cgroup_v1_memory_limit(contents: &str) -> Option<u64> {
    // The v1 kernel exports PAGE_COUNTER_MAX (LONG_MAX rounded down to the
    // host page size) for an unlimited memory controller. On Linux x64 this
    // is a small range immediately below LONG_MAX, not one literal value.
    const UNLIMITED_PAGE_COUNTER_FLOOR: u64 = (i64::MAX as u64) - 4095;
    let bytes = contents.trim().parse::<u64>().ok()?;
    (bytes != 0 && bytes < UNLIMITED_PAGE_COUNTER_FLOOR).then_some(bytes)
}

#[cfg(all(test, feature = "aarm-telemetry"))]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn physical_capacity(bytes: Option<u64>) -> AarmHostMemoryCapacity {
        AarmHostMemoryCapacity {
            physical_total_bytes: bytes,
            environment_limit_bytes: None,
            effective_capacity_bytes: bytes,
            source: bytes.map(|_| AarmHostMemoryCapacitySource::PhysicalTotal),
        }
    }

    #[test]
    fn effective_capacity_uses_only_stable_nonzero_candidates() {
        let cases = [
            (
                Some(16),
                None,
                Some(16),
                Some(AarmHostMemoryCapacitySource::PhysicalTotal),
            ),
            (
                None,
                Some(4),
                Some(4),
                Some(AarmHostMemoryCapacitySource::EnvironmentLimit),
            ),
            (
                Some(16),
                Some(4),
                Some(4),
                Some(AarmHostMemoryCapacitySource::PhysicalTotalAndEnvironmentLimit),
            ),
            (
                Some(4),
                Some(16),
                Some(4),
                Some(AarmHostMemoryCapacitySource::PhysicalTotalAndEnvironmentLimit),
            ),
            (
                Some(8),
                Some(8),
                Some(8),
                Some(AarmHostMemoryCapacitySource::PhysicalTotalAndEnvironmentLimit),
            ),
            (None, None, None, None),
            (Some(0), Some(0), None, None),
        ];
        for (physical, environment, effective, source) in cases {
            let snapshot = AarmHostMemoryCapacity::from_candidates(physical, environment);
            assert_eq!(snapshot.effective_capacity_bytes, effective);
            assert_eq!(snapshot.source, source);
        }
        let max = AarmHostMemoryCapacity::from_candidates(Some(u64::MAX), Some(u64::MAX - 1));
        assert_eq!(max.effective_capacity_bytes, Some(u64::MAX - 1));
    }

    #[test]
    fn auto_budget_is_exact_for_representable_capacities_and_fails_closed_when_unknown() {
        for bytes in [
            1,
            4095,
            4096,
            4097,
            1024_u64.pow(3),
            4 * 1024_u64.pow(3),
            16 * 1024_u64.pow(3),
        ] {
            let resolved = resolve_aarm_auto_budget(physical_capacity(Some(bytes)))
                .expect("capacity resolves");
            assert_eq!(resolved.source, AarmBudgetSource::Auto);
            assert_eq!(resolved.effective_capacity_bytes, Some(bytes));
            assert_eq!(
                resolved.resolved_hard_limit_bytes,
                bytes.min(usize::MAX as u64)
            );
            assert_eq!(resolved.address_width_clamped, bytes > usize::MAX as u64);
        }
        assert_eq!(
            resolve_aarm_auto_budget(physical_capacity(None)),
            Err(AarmAutoBudgetError::StableCapacityUnavailable)
        );
        assert_eq!(
            resolve_aarm_auto_budget(AarmHostMemoryCapacity {
                effective_capacity_bytes: Some(0),
                source: Some(AarmHostMemoryCapacitySource::PhysicalTotal),
                ..AarmHostMemoryCapacity::default()
            }),
            Err(AarmAutoBudgetError::StableCapacityZero)
        );
    }

    #[test]
    fn auto_budget_clamps_only_at_the_supplied_address_width_boundary() {
        let capacity = physical_capacity(Some(u64::MAX));
        let synthetic_32_bit =
            resolve_aarm_auto_budget_for_usize_max(capacity, u64::from(u32::MAX))
                .expect("u64 capacity clamps");
        assert_eq!(
            synthetic_32_bit.resolved_hard_limit_bytes,
            u64::from(u32::MAX)
        );
        assert!(synthetic_32_bit.address_width_clamped);

        let native = resolve_aarm_auto_budget_for_usize_max(capacity, u64::MAX).expect("u64 fits");
        assert_eq!(native.resolved_hard_limit_bytes, u64::MAX);
        assert!(!native.address_width_clamped);
    }

    #[test]
    fn auto_governor_discovers_once_and_shares_one_explicit_authority() {
        let discoveries = Cell::new(0);
        let auto = discover_aarm_auto_governor_with(|| {
            discoveries.set(discoveries.get() + 1);
            physical_capacity(Some(8193))
        })
        .expect("first discovery resolves");
        assert_eq!(discoveries.get(), 1);
        assert_eq!(auto.telemetry().resolved_hard_limit_bytes, 8193);
        let first = auto.governor();
        let second = auto.governor();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.telemetry().hard_limit_bytes, 8193);
    }

    #[test]
    fn explicit_budget_is_exact_and_never_queries_or_clamps_host_capacity() {
        for bytes in [
            1,
            4095,
            4096,
            4097,
            8191,
            8192,
            64 * 1024 * 1024,
            1024_u64.pow(3),
        ] {
            let resolved = resolve_aarm_explicit_budget(bytes).expect("explicit budget resolves");
            assert_eq!(resolved.source, AarmBudgetSource::Explicit);
            assert_eq!(resolved.requested_explicit_bytes, Some(bytes));
            assert_eq!(resolved.effective_capacity_bytes, None);
            assert_eq!(resolved.resolved_hard_limit_bytes, bytes);
            assert!(!resolved.address_width_clamped);
            assert_eq!(resolved.capacity_source, None);
        }
        assert_eq!(
            resolve_aarm_explicit_budget(0),
            Err(AarmAutoBudgetError::ExplicitBudgetZero)
        );
        assert_eq!(
            resolve_aarm_explicit_budget_for_usize_max(u64::from(u32::MAX), u64::from(u32::MAX)),
            Ok(AarmAutoBudgetTelemetry {
                source: AarmBudgetSource::Explicit,
                requested_explicit_bytes: Some(u64::from(u32::MAX)),
                effective_capacity_bytes: None,
                resolved_hard_limit_bytes: u64::from(u32::MAX),
                address_width_clamped: false,
                capacity_source: None,
            })
        );
        assert_eq!(
            resolve_aarm_explicit_budget_for_usize_max(
                u64::from(u32::MAX) + 1,
                u64::from(u32::MAX)
            ),
            Err(AarmAutoBudgetError::ExplicitBudgetExceedsAddressableSize)
        );
    }

    #[test]
    fn explicit_governor_bypasses_discovery_and_can_exceed_host_capacity() {
        let discoveries = Cell::new(0);
        let explicit = aarm_governor_from_policy_with(
            AarmBudgetPolicy::ExactBytes(2 * 1024 * 1024 * 1024),
            || {
                discoveries.set(discoveries.get() + 1);
                panic!("explicit policy must not discover host capacity");
            },
        )
        .expect("explicit budget does not need a host snapshot");
        assert_eq!(discoveries.get(), 0);
        assert_eq!(explicit.host_capacity(), None);
        assert_eq!(
            explicit.telemetry().resolved_hard_limit_bytes,
            2 * 1024 * 1024 * 1024
        );
        let first = explicit.governor();
        let second = explicit.governor();
        assert!(Arc::ptr_eq(&first, &second));
        let host = physical_capacity(Some(1024 * 1024 * 1024));
        assert_eq!(
            resolve_aarm_auto_budget(host)
                .expect("Auto resolves host capacity")
                .resolved_hard_limit_bytes,
            1024 * 1024 * 1024
        );
    }

    #[test]
    fn linux_meminfo_parser_accepts_only_a_valid_mem_total_kib_field() {
        assert_eq!(
            parse_linux_mem_total("SwapTotal: 2 kB\nMemTotal: 1024 kB\n"),
            Some(1024 * 1024)
        );
        assert_eq!(
            parse_linux_mem_total("MemTotal:   2   KB\nOther: 1 kB"),
            Some(2048)
        );
        assert_eq!(parse_linux_mem_total("MemAvailable: 1024 kB"), None);
        assert_eq!(parse_linux_mem_total("MemTotal: 1024"), None);
        assert_eq!(parse_linux_mem_total("MemTotal: nope kB"), None);
        assert_eq!(
            parse_linux_mem_total("MemTotal: 18446744073709551615 kB"),
            None
        );
        assert_eq!(parse_linux_mem_total("MemTotal: 1024 bytes"), None);
    }

    fn fixture_cgroup_limit(
        process_cgroup: &str,
        mountinfo: &str,
        files: &[(&str, &str)],
    ) -> Option<u64> {
        discover_linux_cgroup_memory_limit(process_cgroup, mountinfo, |path| {
            files
                .iter()
                .find_map(|(candidate, value)| (*candidate == path).then(|| (*value).to_string()))
        })
    }

    #[test]
    fn cgroup_v2_resolves_the_mounted_current_hierarchy_and_all_ancestors() {
        const MOUNTS: &str = "29 23 0:26 / /cg rw - cgroup2 cgroup rw\n";
        assert_eq!(
            fixture_cgroup_limit(
                "0::/current/child\n",
                MOUNTS,
                &[
                    ("/cg/current/child/memory.max", "max"),
                    ("/cg/current/memory.max", "1073741824"),
                    ("/cg/memory.max", "max"),
                ],
            ),
            Some(1024 * 1024 * 1024)
        );
        assert_eq!(
            fixture_cgroup_limit(
                "0::/parent/child\n",
                MOUNTS,
                &[
                    ("/cg/parent/child/memory.max", "4294967296"),
                    ("/cg/parent/memory.max", "2147483648"),
                ],
            ),
            Some(2 * 1024 * 1024 * 1024)
        );
        assert_eq!(
            fixture_cgroup_limit(
                "0::/parent/child\n",
                MOUNTS,
                &[
                    ("/cg/parent/child/memory.max", "3221225472"),
                    ("/cg/parent/memory.max", "max"),
                ],
            ),
            Some(3 * 1024 * 1024 * 1024)
        );
        assert_eq!(
            fixture_cgroup_limit(
                "0::/one/two/three\n",
                MOUNTS,
                &[
                    ("/cg/one/two/three/memory.max", "max"),
                    ("/cg/one/two/memory.max", "6442450944"),
                    ("/cg/one/memory.max", "5368709120"),
                    ("/cg/memory.max", "7516192768"),
                ],
            ),
            Some(5 * 1024 * 1024 * 1024)
        );
        assert_eq!(
            fixture_cgroup_limit(
                "0::/current/child\n",
                MOUNTS,
                &[
                    ("/cg/current/child/memory.max", "not-a-limit"),
                    ("/cg/current/memory.max", "1073741824"),
                ],
            ),
            Some(1024 * 1024 * 1024)
        );
        assert_eq!(
            fixture_cgroup_limit(
                "0::/all-max\n",
                MOUNTS,
                &[("/cg/all-max/memory.max", "max"), ("/cg/memory.max", "max"),],
            ),
            None
        );
    }

    #[test]
    fn cgroup_v1_considers_ancestor_limits_and_unlimited_page_counter_values() {
        const MOUNTS: &str = "30 23 0:27 / /cg-memory rw - cgroup cgroup rw,memory\n";
        assert_eq!(
            fixture_cgroup_limit(
                "5:cpu,memory:/parent/child\n",
                MOUNTS,
                &[
                    (
                        "/cg-memory/parent/child/memory.limit_in_bytes",
                        "9223372036854771712"
                    ),
                    ("/cg-memory/parent/memory.limit_in_bytes", "1073741824"),
                ],
            ),
            Some(1024 * 1024 * 1024)
        );
        assert_eq!(
            fixture_cgroup_limit(
                "5:memory:/parent/child\n",
                MOUNTS,
                &[
                    (
                        "/cg-memory/parent/child/memory.limit_in_bytes",
                        "4294967296"
                    ),
                    ("/cg-memory/parent/memory.limit_in_bytes", "2147483648"),
                ],
            ),
            Some(2 * 1024 * 1024 * 1024)
        );
        assert_eq!(parse_cgroup_v1_memory_limit("9223372036854771712"), None);
        assert_eq!(parse_cgroup_v1_memory_limit("9223372036854775807"), None);
        assert_eq!(parse_cgroup_v1_memory_limit("18446744073709551615"), None);
        assert_eq!(parse_cgroup_v1_memory_limit("4096"), Some(4096));
    }

    #[test]
    fn cgroup_mount_root_mapping_ignores_unrelated_mounts_and_prevents_escape() {
        const MOUNTS: &str = concat!(
            "29 23 0:26 /unrelated /wrong rw - cgroup2 cgroup rw\n",
            "30 23 0:27 /docker/abc /sys/fs/cgroup rw - cgroup2 cgroup rw\n"
        );
        assert_eq!(
            fixture_cgroup_limit(
                "0::/docker/abc/child\n",
                MOUNTS,
                &[("/sys/fs/cgroup/child/memory.max", "1073741824")],
            ),
            Some(1024 * 1024 * 1024)
        );
        assert_eq!(
            fixture_cgroup_limit("0::/docker/other\n", MOUNTS, &[]),
            None
        );
        assert_eq!(
            fixture_cgroup_limit("0::/docker/abc/../../escape\n", MOUNTS, &[]),
            None
        );
        assert_eq!(fixture_cgroup_limit("0::/current\n", "", &[]), None);
    }

    #[test]
    fn cgroup_parsers_are_conservative() {
        assert_eq!(parse_cgroup_v2_memory_max("4096\n"), Some(4096));
        assert_eq!(parse_cgroup_v2_memory_max("max\n"), None);
        assert_eq!(parse_cgroup_v2_memory_max("bad"), None);
        assert_eq!(parse_cgroup_v1_memory_limit("bad"), None);
        assert_eq!(parse_cgroup_v2_memory_max("0"), None);
        assert_eq!(
            normalize_kernel_path("/nested/path"),
            Some("/nested/path".to_string())
        );
        assert_eq!(normalize_kernel_path("relative"), None);
        assert_eq!(normalize_kernel_path("/nested/../escape"), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_physical_capacity_is_nonzero_when_reported() {
        if let Some(bytes) = windows_physical_total() {
            assert!(bytes > 0);
        }
    }
}
