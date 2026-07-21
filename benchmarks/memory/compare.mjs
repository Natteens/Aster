// Deterministic baseline comparator and JSON validator for the `memory_matrix`
// executor. It consumes the JSON schema emitted by
// `crates/aster-codegen-cranelift/examples/memory_matrix.rs` and never
// reimplements the single-run validation rules that already live in the
// executor. Only cross-run baseline comparison and JSON-shape validation live
// here. Timing is never compared. Node's native `JSON.parse` performs the real
// structural validation, so no serialization dependency is added.

import { readFileSync } from "node:fs";
import process from "node:process";

export const MEMORY_FIELDS = [
  "total_allocations",
  "object_allocations",
  "array_allocations",
  "string_allocations",
  "requested_bytes",
  "used_bytes",
  "reserved_bytes",
  "peak_used_bytes",
  "peak_reserved_bytes",
];

export function caseKey(entry) {
  return `${entry.case}/${entry.region}/${entry.scale}`;
}

function isNumber(value) {
  return typeof value === "number" && Number.isFinite(value);
}

function validateMemory(memory, path, errors) {
  if (memory === null || typeof memory !== "object") {
    errors.push(`${path}: memory must be an object`);
    return;
  }
  for (const field of MEMORY_FIELDS) {
    if (!isNumber(memory[field])) {
      errors.push(`${path}: memory.${field} must be a number`);
    }
  }
}

function validateTimingGroup(timing, path, errors) {
  if (timing === null) {
    return;
  }
  if (typeof timing !== "object") {
    errors.push(`${path} must be an object or null`);
    return;
  }
  for (const field of ["median", "min", "max"]) {
    if (!isNumber(timing[field])) {
      errors.push(`${path}.${field} must be a number`);
    }
  }
}

export function validateReport(report) {
  const errors = [];
  if (report === null || typeof report !== "object") {
    return { ok: false, errors: ["report must be an object"] };
  }
  if (!isNumber(report.schema_version)) {
    errors.push("schema_version must be a number");
  }
  const environment = report.environment;
  if (environment === null || typeof environment !== "object") {
    errors.push("environment must be an object");
  } else {
    for (const field of ["aster_version", "os", "arch", "target", "profile", "git_revision"]) {
      if (typeof environment[field] !== "string") {
        errors.push(`environment.${field} must be a string`);
      }
    }
  }
  if (!Array.isArray(report.results)) {
    errors.push("results must be an array");
    return { ok: errors.length === 0, errors };
  }
  for (const result of report.results) {
    const path = `result ${caseKey(result)}`;
    for (const field of ["case", "region", "scale", "status"]) {
      if (typeof result[field] !== "string") {
        errors.push(`${path}: ${field} must be a string`);
      }
    }
    if (!isNumber(result.iterations)) {
      errors.push(`${path}: iterations must be a number`);
    }
    if (!isNumber(result.expected_checksum)) {
      errors.push(`${path}: expected_checksum must be a number`);
    }
    if (result.checksum !== null && !isNumber(result.checksum)) {
      errors.push(`${path}: checksum must be a number or null`);
    }
    if (result.status === "pass") {
      validateMemory(result.memory, path, errors);
    }
    const timing = result.timing_ms;
    if (timing === null || typeof timing !== "object") {
      errors.push(`${path}: timing_ms must be an object`);
    } else {
      validateTimingGroup(timing.frontend_compile, `${path}: timing_ms.frontend_compile`, errors);
      validateTimingGroup(timing.jit_and_execute, `${path}: timing_ms.jit_and_execute`, errors);
      validateTimingGroup(timing.end_to_end, `${path}: timing_ms.end_to_end`, errors);
    }
    if (result.error !== null && typeof result.error !== "string") {
      errors.push(`${path}: error must be a string or null`);
    }
  }
  return { ok: errors.length === 0, errors };
}

export function reportToBaseline(report, options = {}) {
  const validation = validateReport(report);
  if (!validation.ok) {
    throw new Error(`cannot build baseline from invalid report:\n${validation.errors.join("\n")}`);
  }
  if (options.target !== undefined && options.target !== report.environment.target) {
    throw new Error(
      `refusing to relabel target: report is ${report.environment.target}, override is ${options.target}`,
    );
  }
  if (options.profile !== undefined && options.profile !== report.environment.profile) {
    throw new Error(
      `refusing to relabel profile: report is ${report.environment.profile}, override is ${options.profile}`,
    );
  }
  const cases = {};
  for (const result of report.results) {
    if (result.status !== "pass" || result.memory === null) {
      throw new Error(`cannot baseline non-passing case ${caseKey(result)}`);
    }
    const memory = {};
    for (const field of MEMORY_FIELDS) {
      memory[field] = result.memory[field];
    }
    cases[caseKey(result)] = {
      case: result.case,
      region: result.region,
      scale: result.scale,
      iterations: result.iterations,
      checksum: result.checksum,
      memory,
    };
  }
  return {
    schema_version: report.schema_version,
    target: options.target ?? report.environment.target,
    profile: options.profile ?? report.environment.profile,
    aster_version: options.asterVersion ?? report.environment.aster_version,
    baseline_generated_from_rev: options.rev ?? report.environment.git_revision,
    cases,
  };
}

export function compareBaseline(baseline, report) {
  if (report.schema_version !== baseline.schema_version) {
    return {
      ok: false,
      rejected: `schema_version ${report.schema_version} is incompatible with baseline ${baseline.schema_version}`,
      diffs: [],
    };
  }
  if (report.environment.target !== baseline.target) {
    return {
      ok: false,
      rejected: `target ${report.environment.target} is incompatible with baseline ${baseline.target}`,
      diffs: [],
    };
  }
  if (report.environment.profile !== baseline.profile) {
    return {
      ok: false,
      rejected: `profile ${report.environment.profile} is incompatible with baseline ${baseline.profile}`,
      diffs: [],
    };
  }

  const reportByKey = new Map();
  for (const result of report.results) {
    reportByKey.set(caseKey(result), result);
  }

  const diffs = [];
  for (const [key, expected] of Object.entries(baseline.cases)) {
    const actual = reportByKey.get(key);
    if (actual === undefined) {
      diffs.push({ key, field: "case", expected: "present", actual: "missing" });
      continue;
    }
    if (actual.status !== "pass" || actual.memory === null) {
      diffs.push({ key, field: "status", expected: "pass", actual: actual.status });
      continue;
    }
    if (actual.checksum !== expected.checksum) {
      diffs.push({ key, field: "checksum", expected: expected.checksum, actual: actual.checksum });
    }
    for (const field of MEMORY_FIELDS) {
      if (actual.memory[field] !== expected.memory[field]) {
        diffs.push({
          key,
          field: `memory.${field}`,
          expected: expected.memory[field],
          actual: actual.memory[field],
        });
      }
    }
  }
  return { ok: diffs.length === 0, rejected: null, diffs };
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function parseOptions(args) {
  const options = {};
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === "--profile") {
      options.profile = args[index + 1];
      index += 1;
    } else if (args[index] === "--rev") {
      options.rev = args[index + 1];
      index += 1;
    } else if (args[index] === "--target") {
      options.target = args[index + 1];
      index += 1;
    }
  }
  return options;
}

function main(argv) {
  const [command, ...rest] = argv;
  if (command === "validate") {
    const report = readJson(rest[0]);
    const { ok, errors } = validateReport(report);
    if (!ok) {
      console.error(`invalid report:\n${errors.join("\n")}`);
      return 1;
    }
    console.log(`valid report: schema ${report.schema_version}, ${report.results.length} case(s)`);
    return 0;
  }
  if (command === "to-baseline") {
    const report = readJson(rest[0]);
    const baseline = reportToBaseline(report, parseOptions(rest.slice(1)));
    console.log(JSON.stringify(baseline, null, 2));
    return 0;
  }
  if (command === "compare") {
    const baseline = readJson(rest[0]);
    const report = readJson(rest[1]);
    const result = compareBaseline(baseline, report);
    if (result.rejected !== null) {
      console.error(`INCOMPATIBLE: ${result.rejected}`);
      return 3;
    }
    if (!result.ok) {
      console.error(`REGRESSION: ${result.diffs.length} deterministic difference(s)`);
      for (const diff of result.diffs) {
        console.error(`  ${diff.key} ${diff.field}: expected ${diff.expected}, got ${diff.actual}`);
      }
      return 1;
    }
    console.log(`OK: report matches baseline ${baseline.target} ${baseline.profile}`);
    return 0;
  }
  console.error("usage: compare.mjs <validate|to-baseline|compare> <files...>");
  return 2;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  process.exit(main(process.argv.slice(2)));
}
