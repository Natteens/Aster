# Logging

Aster's standard library starts with one logging facade:

```aster
Log("normal message");
Log.Warning("something recoverable looks wrong");
Log.Error("something failed");
```

Each call takes exactly one `string` and prints one line while the program runs:

```text
[log] normal message
[warning] something recoverable looks wrong
[error] something failed
```

`Log` goes to standard output; `Log.Warning` and `Log.Error` go to standard error. There
are no timestamps at this stage.

Things worth knowing:

- `Log.Error` reports a failure but does **not** stop the program. Logging is not error
  handling.
- The argument is evaluated exactly once, like any normal function argument.
- `Log.Debug`, `Log.Info`, `print`, and `Console.WriteLine` do not exist; the API is
  deliberately these three calls.
