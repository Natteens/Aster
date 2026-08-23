# Testing

Run a root package's tests with:

```console
aster test
```

Tests live below `tests/` and are ordinary package source files. They keep the usual namespace,
`using`, visibility, type, package, worker, FFI, and runtime rules. Only the package invoked by
`aster test` contributes tests; dependency test directories are never discovered.
The command uses the normal project root source at `app/main.aster`, but it does not require an
application entry point or `Main`.

```aster
namespace tests;

using aster.testing;

test void AddsNumbers()
{
    Assert.Equal(42, 20 + 22);
}
```

`test` is contextual: it marks a namespace-level, parameterless, synchronous, non-generic `void`
function with a body. Tests cannot be `public`, so they are not exported as dependency API.

## Assertions

`aster.testing.Assert` provides `True(bool)`, `False(bool)`, overloaded `Equal` and `NotEqual` for
bool, char, scalar numeric types, and string, absolute-tolerance `ApproximatelyEqual` for
float/double, and deliberate `Fail(message)`. `Equal` uses ASTER's
ordinary equality semantics: in particular, it is exact for floats, `NaN != NaN`, and `+0 == -0`.
Arrays and collections have no deep-equality assertion in this release; use `Assert.True` for a
custom comparison.

`ApproximatelyEqual` requires a non-negative tolerance. NaN always fails; equal infinities and
signed zeros pass; otherwise the absolute difference must be at most the tolerance. All assertion
failures use the existing controlled runtime assertion path—never exceptions or unwind.

`aster.testing` is an ordinary standard-library namespace. It can also be imported by ordinary
program code, where a failing assertion is the same controlled runtime error; no runner-specific
language mode exists.

## Execution and output

Discovery walks only the root package's `tests/` tree, ignores symlinks, and sorts fully qualified
test identities. The suite compiles and prepares its main executable module once, then invokes each
test sequentially with a fresh `ExecutionContext`. A test that uses Task/Parallel receives a
per-test worker runtime, so its worker pool and task records are torn down before the next test. A
failed assertion or ordinary controlled runtime failure ends that test, tears down its context, and
does not stop later tests.

Test-owned terminal and log output is captured per context. Passing-test output is suppressed;
failing-test output appears beneath that test's failure. Test stdin is the empty in-memory input
stream (EOF).

```text
running 2 tests
PASS sample.tests.AddsNumbers
FAIL sample.tests.Subtracts
  Aster runtime error: assertion failed: values are not equal

test result: FAILED. 1 passed; 1 failed
```

Projects with no tests succeed and report `0 passed; 0 failed`. `aster test --help` shows the
command help. There is no file mode, filtering, parallel test execution, fixtures, timeouts, or
test-only package visibility in this release.

## Exit status

`aster test` preserves the CLI-wide status contract: all passing (including zero tests) exits `0`;
test, compilation, discovery, preparation, or runtime failures exit `1`; invalid arguments exit
`2`.
