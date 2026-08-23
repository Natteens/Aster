# 05 — Functions and methods

## Objective

Distinguish namespace-level functions, class methods, struct methods, and interface members while
preserving ASTER’s type-before-name convention. This chapter does not select a program entry point.

## Accepted syntax

### Module function

```aster
public int Add(int a, int b)
{
    return a + b;
}

public int Double(int value) => value * 2;
```

### Class method

```aster
public class Calculator
{
    public int Add(int a, int b)
    {
        return a + b;
    }
}
```

### Struct method

```aster
public struct Counter
{
    private int value;

    public int GetValue()
    {
        return value;
    }
}
```

### Interface member

```aster
public interface ICalculator
{
    int Add(int a, int b);
}
```

Interface members are public by contract and omit a redundant visibility modifier.

## Accepted rules

- ASTER supports namespace-level functions and methods simultaneously.
- A function declaration consists of return type, name, parameter list, and body.
- Each parameter has a type before its name.
- A namespace-level function belongs to its declaring namespace rather than to a type.
- An instance method receives `this`; a `static` method belongs to its class and receives no `this`.
- A struct method operates on a struct value; mutation of its receiver remains tied to the memory model.
- An interface member declares a required callable contract and has no body or instance state.
- A non-`void` callable returns a compatible value on every reachable path.
- A `void` callable may use `return;` or reach the end of its body.
- Calls supply the declared number of arguments with compatible types.
- Executable namespace functions and class/struct/static/generic methods may use `=> expression;`.
  It is normalized to the existing return or expression-statement semantics. Interface and foreign
  declarations remain bodyless signatures ending in `;`.
- Calls accept positional arguments followed by named arguments. Expressions always evaluate in
  source order; names affect parameter placement only.
- Trailing parameters may declare compile-time constant defaults. Required parameters cannot follow
  an optional one. Omitted defaults are materialized before HIR/MIR, and defaults do not change
  overload identity or break equal-best ambiguities.
- Functions do not use `fn`, and return types do not use `->`.

## Valid design examples

```aster
internal float Average(float left, float right)
{
    return (left + right) / 2.0;
}

public class Greeter
{
    public void Greet(string name)
    {
        Log("Olá, " + name);
    }
}
```

## Invalid design examples

```aster
fn Add(int a, int b) -> int { return a + b; }
// Invalid: ASTER does not use `fn` or `->`.
```

```aster
int Missing()
{
    return;
}
// Invalid: the declared result type is `int`.
```

```aster
public interface Invalid
{
    int Add(int a, int b) { return a + b; }
}
// Invalid in the initial contract model: required interface members do not provide bodies.
```

## Implemented status

Namespace-level functions with parameters, `void` and value returns, calls between such functions,
direct recursion, and mutual recursion are implemented end to end (all signatures are
declared before any body is compiled, so declaration order does not matter). Duplicate
function signatures may be overloaded. Class instance methods and one
user-declared constructor per class are executable in the JIT with an implicit receiver;
`this` exposes that receiver explicitly. Static and struct methods execute with their established
receiver semantics. Exact overload
matches win before safe implicit conversions; an equal best match is diagnosed, and return type
alone never distinguishes an overload. Named/default arguments share this resolver for functions,
methods, interface dispatch, and the single supported constructor. Constructor overloads remain
future work.

Recursion is supported with a per-execution limit of 1,024 simultaneously active calls through a
recursive call cycle. Exceeding it is a controlled runtime failure rather than a native stack
overflow. The same limit applies to normal execution and worker-local execution such as
`Task.Run`; independent execution contexts own independent counters. Acyclic calls are not counted
against this recursion limit.

## Application entry

### ACCEPTED — Program entry point

A normal application begins at one `public static Main()` method in a public class. It takes no
parameters and returns `void` or `int`. Without a manifest the eligible method must be in the root
namespace. An optional `Aster.toml` can select `namespace.Class.Main`; `--function` remains an explicit
tooling override for examples and compiler development.

`Main` runs once as an ordinary method. It does not define `Start`, `Update`, a game loop, threads,
GPU work, or ECS lifecycle. `check`, `dump-hir`, and `dump-mir` do not require an entry because
libraries are valid compilation targets.

## OPEN QUESTIONS

### PROPOSED — Function values and closures

1. **Named callables only** — small implementation; limits callbacks and composition.
2. **Function references without captures** — supports callbacks cheaply; still excludes closures.
3. **Full closures** — expressive; requires capture, allocation, and lifetime rules.

**Recommendation:** PROPOSED — design non-capturing function references first and add closures only
with the memory model.
