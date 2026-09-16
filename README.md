# dyn-loader

The **loading gate** for the dyn protocol, plus the Rust-native fast path
through it.

Every dynamic library a process opens goes through one door. This crate is
that door: locate, load, resolve a symbol. Nothing else in the system calls
`dlopen`, and every layer above builds on this handle.

```rust
// In the module (.so):
#[no_mangle]
pub extern "C" fn my_transform_entry() -> AbiStableDynRef {
    SafeArcDyn::from_arc(Arc::new(MyTransform) as Arc<dyn Transform>).into_abi()
}

// In the host:
let module = NativeModule::<dyn Transform>::load("libmy_transform.so", b"my_transform_entry\0")?;
let transform: &dyn Transform = module.trait_ref();
```

## ⚠️ Align your compiler version

> **Every crate that exchanges Rust fat pointers across the boundary must be
> built with the exact same Rust compiler version.** Rust makes no ABI
> stability guarantee between compiler releases — vtable layout, metadata
> encoding and more may change. Mixing versions between host and module is
> **undefined behaviour**.
>
> Pin one toolchain (e.g. `rust-toolchain.toml`) and rebuild **everything**
> with it.
>
> A C interface table has no such requirement: its layout is fixed by
> `#[repr(C)]` and the C calling convention. That layer lives in
> [`cdyn-loader`](https://crates.io/crates/cdyn-loader), built on this crate.

## Crate features

- **`abi-map`** (off by default) — `Verdict`, the check a host runs before
  dialling a module: the producer's compiler facts are mapped to ABI levels
  and compared against the host's own. Refuses rather than guesses when a
  producer is not covered. Enable it when your host carries modules produced
  by toolchains you do not control.

## Core types

- **`DynLib`** — the gate. Wraps `libloading::Library` with `Arc` for shared
  ownership, resolves symbols, and carries the path for diagnostics.
- **`NativeModule<T>`** — a loaded module holding a `SafeArcDyn<T>` that
  dereferences to `&T` for calling trait methods on the loaded object.
- **`SafeArcDyn<T>`** — a safe, cloneable handle over an `AbiStableDynRef`.
- **`AbiStableDynRef`** — fat pointer plus retain/release function pointers,
  enabling Arc-like reference counting across the boundary.
- **`AbiDynFatPtr`** — ABI-stable representation of a Rust fat pointer
  (data pointer + vtable pointer), `#[repr(C)]`.
- **`HeadEntry`** — the one thing every contract package has in common: an
  identity.

  ```rust
  use dyn_loader::HeadEntry;

  pub struct Math;

  impl HeadEntry for Math {
      const ID: &'static str = "com.example.math";
  }
  ```

  Contract packages differ in everything else — what interface they declare,
  which entry symbol they expect, whether they check anything before opening —
  so nothing but a name is shared. Reverse-domain, for naming a library in a
  log, an error, or a report of what is loaded. Nothing here interprets it.

## The boundary rule

**Only standard C interfaces may cross the module boundary.** Legal transports
are `extern "C"` functions, C scalar types, `#[repr(C)]` POD structs, C
strings and function pointers. Language-runtime objects — C++ exceptions, Zig
error unions, Rust panics, GC references — never cross. The single controlled
exception is the fat-pointer pair above, valid only under the same-compiler
clause.

### Every struct crossing the boundary must be `#[repr(C)]`

Not a style preference — a requirement, for two measured reasons.

**The default representation reorders fields.** `repr(Rust)` is free to lay
fields out in whatever order minimizes padding, and does:

```text
struct Bad { a: u8, b: u64, c: u8 }        // repr(Rust)
#[repr(C)] struct Good { a: u8, b: u64, c: u8 }

measured:
  Bad   a @8, b @0,  c @9,  size 16   — b moved to the front
  Good  a @0, b @8,  c @16, size 24   — declaration order
```

The layout is deterministic for one compiler and one declaration, but the
algorithm is unspecified and may change in any release. A module built with
one toolchain and a host built with another have no common promise about where
a field lives.

**Padding bytes are not initialized.** A `#[repr(C)]` struct has padding when
its fields do not tile the size, and those bytes hold whatever was in memory
before:

```text
#[repr(C)] struct P { a: u8, b: u64 }   // bytes 1..8 are padding

two structs, identical fields, different memory underneath:
  a = [01, aa, aa, aa, aa, aa, aa, aa, 02, 00, …]
  b = [01, bb, bb, bb, bb, bb, bb, bb, 02, 00, …]
  fields equal? true      bytes equal? false
```

So **never hash, `memcmp`, or otherwise read a struct as raw bytes** to decide
whether two payloads match. Compare or serialize field by field. If a
byte-level digest is genuinely needed, zero the struct first or carry explicit
padding fields — and remember this applies to buffers that will be hashed, not
only to structs.

### The rules are computed and checked

`tests/alignment_matrix.rs` holds an independent implementation of the C
layout rules and compares it against what the compiler actually produces,
across a grid of field types. `tests/layout_rules.rs` pins the two facts
above. If either ever stops holding, the tests say so before the rule is
quietly wrong.

## Ownership invariants

1. **Whoever allocates, deallocates** — every pointer crossing the boundary is
   paired with a free/release function pointer belonging to the allocating
   module. Allocators are not guaranteed to match across boundaries, so the
   free must execute inside the allocator's own module.
2. **Whoever creates, operates** — the host receives a calling convention plus
   function pointers, and every operation executes inside the creator's
   module. The host never "has" the object; it only holds pointers and dials
   the protocol.

The only thing that ever crosses the boundary, in any language, is the
protocol itself: pointers plus function pointers.

## Where this fits

| Crate | What it adds |
|---|---|
| `dyn-loader` | the gate, and the Rust fat-pointer bridge |
| `cdyn-loader` | C interface tables: positional `#[repr(C)]` dispatch, ref-counted handles, single-owner data boxes |
| `dyn-abi-map` | ABI policy: compiler facts mapped to levels, compared before dialling |
| `dyn-support` | delivery: fetch a platform package and put it next to the executable |

## ABI stability across toolchains

Measured with a host/module matrix test (module built to a `.so` with toolchain
A, loaded by a host binary built with toolchain B), 2026-09:

| Toolchain pair (host ↔ module) | `native` (fat-pointer bridge) |
|---|---|
| `nightly-2025-05-06` ↔ `stable 1.98.1` | ✅ Verified |
| `nightly-2025-05-06` ↔ `1.95.0` | ✅ Verified |
| `nightly-2025-05-06` ↔ `1.92.0` | ✅ Verified |
| `stable 1.98.1` ↔ `1.95.0` | ✅ Verified |
| `stable 1.98.1` ↔ `1.92.0` | ✅ Verified |
| `1.95.0` ↔ `1.92.0` | ✅ Verified |
| same-version pairs (all four toolchains) | ✅ Verified |
| **other / future versions** | ❌ **undefined behaviour — do not rely on it** |

The cross-version results above are an **observation, not a guarantee**: the
vtable layout happened to be identical across these toolchains. Rust officially
promises nothing here, and a future release may break it silently. Re-run the
matrix when adopting a new toolchain, and prefer exact alignment in production.

The `AbiDynFatPtr` / `AbiStableDynRef` structs are themselves `#[repr(C)]` and
layout-stable across versions. What is *not* guaranteed is the **vtable
contents** behind a `dyn Trait` pointer — which is exactly why this mode needs
version alignment and the interface-table mode does not.

## Safety

Loading dynamic libraries and unpacking raw fat pointers is inherently unsafe.
See the `# Safety` sections on each API. The retain/release function pointers
give you Arc-like reference counting across the boundary, but the caller is
still responsible for ABI compatibility.

## License

Apache License 2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE).
Redistributions must retain the attribution notices in `NOTICE`
(Apache-2.0 §4(c)/(d)).
