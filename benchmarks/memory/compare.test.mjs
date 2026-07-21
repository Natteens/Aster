import assert from "node:assert/strict";
import { test } from "node:test";

import { compareBaseline, reportToBaseline, validateReport } from "./compare.mjs";

function baseCase() {
  return {
    case: "object",
    region: "temporary",
    scale: "small",
    iterations: 10000,
    status: "pass",
    checksum: 390000,
    expected_checksum: 390000,
    samples: 5,
    memory: {
      total_allocations: 10000,
      object_allocations: 10000,
      array_allocations: 0,
      string_allocations: 0,
      requested_bytes: 40000,
      used_bytes: 0,
      reserved_bytes: 65536,
      peak_used_bytes: 4,
      peak_reserved_bytes: 65536,
    },
    timing_ms: {
      frontend_compile: { median: 1, min: 1, max: 1 },
      jit_and_execute: { median: 2, min: 1, max: 3 },
      end_to_end: { median: 3, min: 2, max: 4 },
    },
    error: null,
  };
}

function makeReport() {
  return {
    schema_version: 1,
    environment: {
      aster_version: "0.14.0",
      os: "linux",
      arch: "x86_64",
      target: "x86_64-linux",
      profile: "release",
      git_revision: "rev-a",
    },
    results: [baseCase()],
  };
}

function baseline() {
  return reportToBaseline(makeReport());
}

test("identical report matches baseline", () => {
  const result = compareBaseline(baseline(), makeReport());
  assert.equal(result.ok, true);
  assert.equal(result.diffs.length, 0);
});

test("divergent checksum fails", () => {
  const report = makeReport();
  report.results[0].checksum = 111;
  const result = compareBaseline(baseline(), report);
  assert.equal(result.ok, false);
  assert.ok(result.diffs.some((diff) => diff.field === "checksum"));
});

test("divergent allocation count fails", () => {
  const report = makeReport();
  report.results[0].memory.object_allocations = 9999;
  const result = compareBaseline(baseline(), report);
  assert.equal(result.ok, false);
  assert.ok(result.diffs.some((diff) => diff.field === "memory.object_allocations"));
});

test("divergent used_bytes fails", () => {
  const report = makeReport();
  report.results[0].memory.used_bytes = 1;
  const result = compareBaseline(baseline(), report);
  assert.equal(result.ok, false);
  assert.ok(result.diffs.some((diff) => diff.field === "memory.used_bytes"));
});

test("incompatible target is rejected with a diagnostic", () => {
  const report = makeReport();
  report.environment.target = "aarch64-linux";
  const result = compareBaseline(baseline(), report);
  assert.equal(result.ok, false);
  assert.match(result.rejected, /target/);
});

test("incompatible schema is rejected with a diagnostic", () => {
  const report = makeReport();
  report.schema_version = 2;
  const result = compareBaseline(baseline(), report);
  assert.equal(result.ok, false);
  assert.match(result.rejected, /schema_version/);
});

test("divergent timing does not fail", () => {
  const report = makeReport();
  report.results[0].timing_ms.jit_and_execute = { median: 999, min: 900, max: 1000 };
  report.results[0].timing_ms.end_to_end = { median: 1000, min: 901, max: 1001 };
  const result = compareBaseline(baseline(), report);
  assert.equal(result.ok, true);
});

test("different git revision does not fail", () => {
  const report = makeReport();
  report.environment.git_revision = "rev-b";
  const result = compareBaseline(baseline(), report);
  assert.equal(result.ok, true);
});

test("missing case is not accepted silently", () => {
  const report = makeReport();
  report.results = [];
  const result = compareBaseline(baseline(), report);
  assert.equal(result.ok, false);
  assert.ok(result.diffs.some((diff) => diff.actual === "missing"));
});

test("non-passing case is not accepted silently", () => {
  const report = makeReport();
  report.results[0].status = "fail";
  report.results[0].memory = null;
  const result = compareBaseline(baseline(), report);
  assert.equal(result.ok, false);
  assert.ok(result.diffs.some((diff) => diff.field === "status"));
});

test("validateReport accepts a well-formed report", () => {
  assert.equal(validateReport(makeReport()).ok, true);
});

test("validateReport rejects a passing case without memory", () => {
  const report = makeReport();
  report.results[0].memory = null;
  const { ok, errors } = validateReport(report);
  assert.equal(ok, false);
  assert.ok(errors.length > 0);
});

test("to-baseline preserves the report target and profile", () => {
  const baseline = reportToBaseline(makeReport());
  assert.equal(baseline.target, "x86_64-linux");
  assert.equal(baseline.profile, "release");
});

test("to-baseline refuses a mislabeling target override", () => {
  assert.throws(() => reportToBaseline(makeReport(), { target: "x86_64-windows" }), /relabel target/);
});

test("to-baseline refuses a mislabeling profile override", () => {
  const report = makeReport();
  report.environment.profile = "debug";
  assert.throws(() => reportToBaseline(report, { profile: "release" }), /relabel profile/);
});

test("escape forms emitted by the executor are valid JSON", () => {
  const cases = [
    ['"\\""', '"'],
    ['"\\\\"', "\\"],
    ['"\\n"', "\n"],
    ['"\\r"', "\r"],
    ['"\\t"', "\t"],
    ['"\\b"', "\b"],
    ['"\\f"', "\f"],
    ['"\\u0001"', ""],
    ['"\\u001f"', ""],
  ];
  for (const [encoded, expected] of cases) {
    assert.equal(JSON.parse(encoded), expected);
  }
});
