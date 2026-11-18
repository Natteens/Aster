// Comparator for `perf_baseline --json` reports (PERF-005 measurement discipline).
//
// Class A (deterministic invariants: status, checksum, MIR function count, entry,
// workload set) must be equal and gate the exit code. Class B (wall-clock timings)
// is reported as medians, ratio and spread only and never affects the exit code.
// Reports with a different schema, kind or target identity are rejected, not
// compared. Native Node, no dependencies.
//
// usage: node compare.mjs <baseline.json> <candidate.json>
// exit: 0 equal class A, 1 class A difference, 2 usage/read error, 3 incompatible

import { readFileSync } from "node:fs";
import process from "node:process";

export const IDENTITY_FIELDS = ["target", "os", "arch", "profile", "timing_unit"];

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export function validateReport(report) {
  const errors = [];
  if (!isObject(report)) {
    return ["report must be an object"];
  }
  if (typeof report.schema_version !== "number") {
    errors.push("schema_version must be a number");
  }
  if (typeof report.kind !== "string") {
    errors.push("kind must be a string");
  }
  if (!isObject(report.metadata)) {
    errors.push("metadata must be an object");
  } else {
    for (const field of IDENTITY_FIELDS) {
      if (typeof report.metadata[field] !== "string") {
        errors.push(`metadata.${field} must be a string`);
      }
    }
  }
  if (!Array.isArray(report.benchmarks)) {
    errors.push("benchmarks must be an array");
  } else {
    for (const bench of report.benchmarks) {
      if (!isObject(bench) || typeof bench.name !== "string") {
        errors.push("every benchmark needs a string name");
      } else if (!isObject(bench.invariants)) {
        errors.push(`benchmark ${bench.name}: invariants must be an object`);
      }
    }
  }
  return errors;
}

function rejection(baseline, candidate) {
  if (candidate.schema_version !== baseline.schema_version) {
    return `schema_version ${candidate.schema_version} is incompatible with baseline ${baseline.schema_version}`;
  }
  if (candidate.kind !== baseline.kind) {
    return `kind ${candidate.kind} is incompatible with baseline ${baseline.kind}`;
  }
  for (const field of IDENTITY_FIELDS) {
    if (candidate.metadata[field] !== baseline.metadata[field]) {
      return `metadata.${field} ${candidate.metadata[field]} is incompatible with baseline ${baseline.metadata[field]}`;
    }
  }
  return null;
}

function show(value) {
  return value === undefined ? "<absent>" : JSON.stringify(value);
}

function spread(timing) {
  return timing.max - timing.min;
}

export function compareReports(baseline, candidate) {
  const errors = [...validateReport(baseline).map((e) => `baseline: ${e}`),
    ...validateReport(candidate).map((e) => `candidate: ${e}`)];
  if (errors.length > 0) {
    return { rejected: errors.join("; "), diffs: [], timings: [] };
  }
  const rejected = rejection(baseline, candidate);
  if (rejected !== null) {
    return { rejected, diffs: [], timings: [] };
  }

  const diffs = [];
  const baseByName = new Map(baseline.benchmarks.map((b) => [b.name, b]));
  const candByName = new Map(candidate.benchmarks.map((b) => [b.name, b]));
  for (const name of baseByName.keys()) {
    if (!candByName.has(name)) {
      diffs.push({ name, field: "benchmark", expected: "present", actual: "missing" });
    }
  }
  for (const name of candByName.keys()) {
    if (!baseByName.has(name)) {
      diffs.push({ name, field: "benchmark", expected: "absent", actual: "extra" });
    }
  }

  const timings = [];
  for (const [name, base] of baseByName) {
    const cand = candByName.get(name);
    if (cand === undefined) {
      continue;
    }
    if (base.status !== cand.status) {
      diffs.push({ name, field: "status", expected: show(base.status), actual: show(cand.status) });
    }
    const fields = new Set([...Object.keys(base.invariants), ...Object.keys(cand.invariants)]);
    for (const field of [...fields].sort()) {
      const expected = base.invariants[field];
      const actual = cand.invariants[field];
      if (JSON.stringify(expected) !== JSON.stringify(actual)) {
        diffs.push({ name, field: `invariants.${field}`, expected: show(expected), actual: show(actual) });
      }
    }
    // Class B: advisory only.
    const baseTimings = isObject(base.timings) ? base.timings : {};
    const candTimings = isObject(cand.timings) ? cand.timings : {};
    for (const phase of Object.keys(baseTimings)) {
      const b = baseTimings[phase];
      const c = candTimings[phase];
      if (!isObject(b) || !isObject(c) || typeof b.median !== "number" || typeof c.median !== "number") {
        continue;
      }
      timings.push({
        name,
        phase,
        baseline_median: b.median,
        candidate_median: c.median,
        ratio: b.median === 0 ? null : c.median / b.median,
        baseline_spread: spread(b),
        candidate_spread: spread(c),
      });
    }
  }
  return { rejected: null, diffs, timings };
}

function main(argv) {
  if (argv.length !== 2) {
    console.error("usage: compare.mjs <baseline.json> <candidate.json>");
    return 2;
  }
  let baseline;
  let candidate;
  try {
    baseline = JSON.parse(readFileSync(argv[0], "utf8"));
    candidate = JSON.parse(readFileSync(argv[1], "utf8"));
  } catch (error) {
    console.error(`cannot read reports: ${error.message}`);
    return 2;
  }
  const result = compareReports(baseline, candidate);
  if (result.rejected !== null) {
    console.error(`INCOMPATIBLE: ${result.rejected}`);
    return 3;
  }
  console.log("class B (advisory, never a gate): median ns, ratio candidate/baseline, spread = max-min");
  for (const t of result.timings) {
    const ratio = t.ratio === null ? "n/a" : t.ratio.toFixed(3);
    console.log(
      `  ${t.name} ${t.phase}: ${t.baseline_median} -> ${t.candidate_median} ratio ${ratio} spread ${t.baseline_spread}/${t.candidate_spread}`,
    );
  }
  if (result.diffs.length > 0) {
    console.error(`CLASS A MISMATCH: ${result.diffs.length} deterministic difference(s)`);
    for (const diff of result.diffs) {
      console.error(`  ${diff.name} ${diff.field}: expected ${diff.expected}, got ${diff.actual}`);
    }
    return 1;
  }
  console.log("OK: class A invariants equal");
  return 0;
}

if (import.meta.url === `file://${process.argv[1]}`
  || (process.argv[1] && import.meta.url.endsWith(process.argv[1].replaceAll("\\", "/")))) {
  process.exit(main(process.argv.slice(2)));
}
