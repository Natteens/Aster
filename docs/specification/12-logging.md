# 12 — Standard logging

## Objective

Define Aster’s accepted minimal logging API and the proposed build-profile filtering model.
Logging is a standard-library facility, not language syntax.

## Accepted syntax

```aster
Log("Mensagem comum");
Log.Warning("Aviso");
Log.Error("Erro");
```

## Accepted rules

- `Log(message)` records a normal message and also serves development diagnostics.
- `Log.Warning(message)` records a problematic but recoverable situation.
- `Log.Error(message)` records a failure.
- No separate informational or debug methods are part of the initial API.
- Logging belongs to the standard library. `Log` is neither a keyword nor a special expression.
- Logging does not change control flow and never replaces explicit error handling.
- The accepted initial message input is `string`; formatting and structured fields remain open.
- Filtering must correspond to explicit build configuration. Messages must not silently disappear.

## Valid design examples

```aster
public class ConfigurationLoader
{
    public void ReportFallback(string file)
    {
        Log.Warning("Usando configuração alternativa: " + file);
    }

    public void ReportFailure(string reason)
    {
        Log.Error(reason);
    }
}
```

## Invalid design examples

```aster
log "Iniciando";
// Invalid: logging is a standard-library call, not a language statement.
```

```aster
Log.Error();
// Invalid: the initial API requires a string message.
```

## Logging profiles — PROPOSED

The filtering model is proposed, not implemented or accepted as a final manifest contract:

- Development builds show `Log`, `Log.Warning`, and `Log.Error` by default.
- Release builds may default to the minimum level `warning`.
- The release minimum level is configurable in the project manifest.
- Initial candidate values are `log`, `warning`, `error`, and `off`.
- `log` shows all three levels.
- `warning` shows warnings and errors.
- `error` shows only errors.
- `off` disables all logging output.
- A future optimizer may remove calls filtered by a fixed build profile, but this proposal does not
  authorize implementation and must not alter evaluation semantics silently.

The following is the only provisional configuration example. The final manifest name, schema,
inheritance rules, and command-line overrides remain open:

```toml
[logging]
release_level = "warning"
```

## OPEN QUESTIONS

### PROPOSED — Filtering and argument evaluation

1. **Evaluate arguments, then suppress output** — preserves side effects; wastes work and makes
   optimized builds differ if calls are later removed.
2. **Do not evaluate a statically filtered call** — enables zero-cost filtering; expressions with
   side effects behave differently across profiles.
3. **Require logging arguments to be side-effect free when removable** — predictable optimization;
   needs effect analysis or restrictive diagnostics.

**Recommendation:** PROPOSED — logging arguments should be side-effect free, and statically filtered
calls may then be removed. Until such a rule is accepted, implementations must preserve evaluation.

### PROPOSED — Manifest and override precedence

1. **Manifest only** — reproducible; inconvenient for temporary diagnostics.
2. **Manifest plus command-line override** — practical; artifacts can vary with invocation.
3. **Manifest, environment, and command line** — flexible; precedence and reproducibility are complex.

**Recommendation:** PROPOSED — manifest defines artifact behavior; explicit CLI overrides are allowed
only when recorded in build metadata. Environment-variable overrides are not recommended initially.

### PROPOSED — Output sink

1. **Runtime standard-error sink** — universally available; limited structure and routing.
2. **Configurable process-wide sink** — supports hosts and tests; introduces global state.
3. **Explicit logger instances** — composable; contradicts the accepted convenience API.

**Recommendation:** PROPOSED — retain the accepted static facade backed by a runtime sink with scoped,
explicit test/host overrides after thread-safety rules are defined.
