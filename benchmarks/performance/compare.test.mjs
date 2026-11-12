import assert from "node:assert/strict";
import test from "node:test";
import { compareReports } from "./compare.mjs";

function timing(median, min = median - 1, max = median + 1) {
  return { mode: "warm", unit: "ns", count: 3, min, median, max, mean: median, samples: [min, median, max] };
}

function report() {
  return {
    schema_version: 1,
    kind: "aster-performance-baseline",
    metadata: {
      target: "x86_64-windows",
      os: "windows",
      arch: "x86_64",
      profile: "release",
      timing_unit: "ns",
      git_revision: null,
    },
    run: { started_unix_seconds: 1 },
    benchmarks: [
      {
        name: "numeric_loops",
        category: "numeric",
        status: "pass",
        error: null,
        invariants: { entry: "Run", expected_checksum: 7, checksum: 7, mir_function_count: 3 },
        timings: { execution_warm: timing(100) },
      },
      {
        name: "strings",
        category: "strings",
        status: "pass",
        error: null,
        invariants: { entry: "Run", expected_checksum: 9, checksum: 9, mir_function_count: 4 },
        timings: { execution_warm: timing(200) },
      },
    ],
  };
}

test("equal reports pass", () => {
  const result = compareReports(report(), report());
  assert.equal(result.rejected, null);
  assert.deepEqual(result.diffs, []);
  assert.equal(result.timings.length, 2);
});

test("changed checksum fails class A", () => {
  const candidate = report();
  candidate.benchmarks[0].invariants.checksum = 8;
  const result = compareReports(report(), candidate);
  assert.equal(result.rejected, null);
  assert.equal(result.diffs.length, 1);
  assert.equal(result.diffs[0].name, "numeric_loops");
  assert.equal(result.diffs[0].field, "invariants.checksum");
});

test("changed status fails class A", () => {
  const candidate = report();
  candidate.benchmarks[1].status = "fail";
  const result = compareReports(report(), candidate);
  assert.ok(result.diffs.some((d) => d.field === "status"));
});

test("mismatched target is rejected", () => {
  const candidate = report();
  candidate.metadata.target = "aarch64-linux";
  const result = compareReports(report(), candidate);
  assert.match(result.rejected, /metadata\.target/);
});

test("mismatched profile is rejected", () => {
  const candidate = report();
  candidate.metadata.profile = "debug";
  assert.match(compareReports(report(), candidate).rejected, /metadata\.profile/);
});

test("schema mismatch is rejected", () => {
  const candidate = report();
  candidate.schema_version = 2;
  const result = compareReports(report(), candidate);
  assert.match(result.rejected, /schema_version/);
});

test("report without target identity is rejected", () => {
  const candidate = report();
  delete candidate.metadata.target;
  assert.match(compareReports(report(), candidate).rejected, /metadata\.target must be a string/);
});

test("missing and extra benchmarks fail class A", () => {
  const missing = report();
  missing.benchmarks.pop();
  const result = compareReports(report(), missing);
  assert.deepEqual(result.diffs, [
    { name: "strings", field: "benchmark", expected: "present", actual: "missing" },
  ]);
  const extra = compareReports(missing, report());
  assert.deepEqual(extra.diffs, [
    { name: "strings", field: "benchmark", expected: "absent", actual: "extra" },
  ]);
});

test("timing changes never fail and report ratio and spread", () => {
  const candidate = report();
  candidate.benchmarks[0].timings.execution_warm = timing(1000, 10, 5000);
  candidate.run.started_unix_seconds = 999;
  const result = compareReports(report(), candidate);
  assert.equal(result.rejected, null);
  assert.deepEqual(result.diffs, []);
  const row = result.timings.find((t) => t.name === "numeric_loops");
  assert.equal(row.ratio, 10);
  assert.equal(row.candidate_spread, 4990);
  assert.equal(row.baseline_spread, 2);
});
