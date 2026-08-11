# Path dependency

Two local packages: `app` depends on `math` by relative path.

```bash
cd app
aster run
```

Prints `42`. `math` is a library package: it declares `[package] name` but no `[application]`.

`math.Seed` is `internal`, so it is visible inside `math` but not from `app`. Calling it from
`app/app/main.aster` is a controlled error.

See [packages and dependencies](../../docs/reference/packages.md).
