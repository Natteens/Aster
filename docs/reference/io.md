# Filesystem and terminal I/O

The `aster.io` namespace exposes host-managed text and terminal operations. File and directory
handles are not public ASTER values; each operation opens, uses, and closes its host resource
within one call.

```aster
using aster.core;
using aster.io;
```

## Terminal

```aster
Write("Name: ");
Option<string> name = ReadLine();
WriteLine("done");
```

`Write` and `WriteLine` accept `string`. `ReadLine` returns `Option<string>` so end-of-input does not
require `null` or an exception.

Terminal I/O is rejected inside worker bodies.

## Paths and text files

```aster
Result<string, IOError> path = CombinePath("data", "input.txt");
Result<string, IOError> text = ReadAllText("data/input.txt");
Result<int, IOError> written = WriteAllText("out/report.txt", "ready");
Result<string[], IOError> files = ListFiles("data");
```

- `CombinePath` performs a lexical join. It does not touch the filesystem or resolve `.`/`..`.
- `ReadAllText` accepts one regular UTF-8 file and preserves its contents.
- `WriteAllText` creates or truncates one file, writes UTF-8, flushes, and closes it. It does not
  create parent directories or promise atomic replacement.
- `ListFiles` returns complete paths for immediate regular-file children, sorted ordinally. It is
  non-recursive and excludes directories, symlinks, and other file types.

Text operations enforce an internal 64 MiB limit. `WriteAllText` returns the number of UTF-8 bytes,
not Unicode scalars.

## Errors

Filesystem operations return `Result<T, IOError>`. `IOError` contains a portable `IOErrorKind` and
an OS error code when one exists:

```text
NotFound, PermissionDenied, AlreadyExists, InvalidPath, InvalidUtf8,
NotFile, NotDirectory, LimitExceeded, Other
```

Expected filesystem failures do not throw exceptions. Paths and messages are not stored in
`IOError`, and no operation exposes a native handle.

Filesystem I/O is also rejected inside worker bodies. These checks happen before JIT execution.

## Resource lifetime

Current I/O remains operation-scoped: ASTER has no public file, socket, or
terminal handle. If a future API exposes a long-lived host resource, it must
have explicit deterministic `Close`/`Dispose`-style authority; repeated close
must be idempotent. Context teardown may clean up a forgotten handle as a
safety backstop, but finalization or future memory reclamation must never be
required for resource correctness.
