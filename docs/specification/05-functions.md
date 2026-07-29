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
`this` exposes that receiver explicitly. Static methods execute without a receiver. Exact overload
matches win before safe implicit conversions; an equal best match is diagnosed, and return type
alone never distinguishes an overload. Struct methods, default/named arguments, constructor
overloads remain future work.

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

### PROPOSED — Overloads and optional arguments

1. **No overloading/defaults** — simplest resolution and diagnostics; repetitive APIs.
2. **Overloads only** — familiar type-directed APIs; ambiguity rules become necessary.
3. **Overloads plus named/default arguments** — expressive; considerably expands call resolution.

**Recommendation:** PROPOSED — begin without overloads or defaults, then evaluate named arguments
before type-directed overloads.

### PROPOSED — Function values and closures

1. **Named callables only** — small implementation; limits callbacks and composition.
2. **Function references without captures** — supports callbacks cheaply; still excludes closures.
3. **Full closures** — expressive; requires capture, allocation, and lifetime rules.

**Recommendation:** PROPOSED — design non-capturing function references first and add closures only
with the memory model.
