# Clock reads

`aster.time.Clock` exposes two allocation-free host reads:

- `MonotonicMilliseconds()` is a non-wall-clock elapsed-time source. Values do not move backward
  during one execution, but its origin is deliberately unspecified.
- `UnixMilliseconds()` is signed milliseconds since the Unix epoch and may move when the host wall
  clock is adjusted.

Both return `long`. No timezone, calendar, sleep, timer, or scheduling API is implied. Clock reads
are host-sensitive operations and are rejected when directly or transitively reachable from a
Task/Parallel worker body.
