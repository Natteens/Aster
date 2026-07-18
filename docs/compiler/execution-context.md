# ExecutionContext and dynamic memory

Every `aster run` invocation owns one Rust `ExecutionContext`. `aster watch` calls the same
execution function after each successful rebuild, so each run receives a new context and the
previous one is dropped first. Generated Aster code never owns, frees, or retains this context.

Every compiled Aster function receives a hidden context pointer as its first ABI parameter. Calls
between Aster functions forward that pointer. It is not visible in source, HIR signatures, or the
user-selected entry function.

## Ownership

Dynamic allocations are stored in host-owned collections inside the context. Dropping the context
drops every array header, backing buffer, and object allocation at once. There is no garbage collector, reference
count, finalizer, process-global arena, or thread-local owner. No pointer from one execution can be
supplied to another execution.

This per-execution arena is intentionally simple: cleanup is deterministic while Aster has no
general ownership syntax. It supports fixed-size arrays and class instances for the duration of
one execution; it is not a promise that a future long-lived/AOT runtime will use the same policy.

## Object ABI

A class value is a non-null pointer to zeroed context-owned storage laid out in declaration order
with natural field alignment. `new Class(arguments)` allocates that storage and immediately invokes
the resolved constructor with the object as a hidden receiver. Methods receive the same receiver;
assigning or passing a class value copies only its pointer.

The compiler permits zero defaults only for fields whose all-zero representation is valid. Array
and class references have no hidden null value and must be initialized by the constructor before
they can be read or escape through `this`.

## Array ABI

An array value is one pointer to a context-owned header. The header records a data pointer, an
`int` length, and the byte stride of one element. The zeroed backing buffer is naturally aligned
for the currently executable scalar and struct fields and never moves. Length and stride do not
change.

The runtime exports allocation, checked element-address lookup, and immutable length lookup.
Element addresses are consumed immediately by generated loads/stores. Struct elements use the
same fieldwise value-copy rules as stack structs; array variables copy only the header reference.

## Bounds failures

Every index reaches the checked runtime operation. A negative or too-large index records a readable
error and returns valid sentinel storage, allowing generated code to finish without dereferencing
an invalid pointer or unwinding through `extern "C"`. The Rust host checks the context after return
and reports the first runtime failure instead of the computed value.

## Current limits

Arrays are fixed-length, mutable, non-null references. There are no slices, resizing, nested or
multidimensional arrays, array `foreach`, escaping host references, or independent deallocation.
Class inheritance, finalizers, independent destruction, weak references and objects escaping the
execution remain outside this implementation.
