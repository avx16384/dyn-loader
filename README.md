# dyn-loader

A **language-agnostic module loading protocol** over the C ABI. Any dynamic
library (`.so` / `.dll` / `.dylib`) that follows the protocol becomes a
*module* that any host — in any language — can load, call, share and release.

Two layers:

1. **`abi`** — the protocol standard layer: interface tables
   ([`AbiTable<T>`]), ref-counted interface references ([`AbiRef<T>`],
   [`AbiStableDynRef`]) and single-owner data boxes ([`AbiBox`]). Plain
   `#[repr(C)]` function-pointer structs — ABI-stable, usable from
   C/C++/Zig/anything with a C FFI.
2. **`native`** — the Rust convenience layer: skip hand-written vtables by
   exchanging native Rust fat pointers ([`SafeArcDyn`], [`NativeModule`]).
   Requires an identical toolchain on both sides. Rust-to-Rust only.

Core types:

- **`DynLib`**: wraps `libloading::Library` with `Arc` for shared ownership.
- **`AbiDynFatPtr`**: ABI-stable representation of a Rust fat pointer
  (data ptr + vtable ptr), `#[repr(C)]`.
- **`AbiStableDynRef`**: fat pointer + retain/release function pointers,
  enabling safe cross-boundary Arc-like reference counting.
- **`SafeArcDyn<T>`**: safe, cloneable handle over an `AbiStableDynRef`.
- **`NativeModule<T>`**: loaded module holding a `SafeArcDyn<T>` that
  dereferences to `&T` for calling trait methods on the loaded object.
- **`AbiTable<T>`**: loaded C function table (the `abi` layer).
- **`AbiRef<T>`**: ref-counted handle to a foreign module object.
- **`AbiBox` / `AbiBoxHandle`**: single-owner data box with a producer-side
  `free` function pointer.

## ⚠️ IMPORTANT — ABI compatibility / compiler version alignment

> **You must strictly align the Rust compiler version for the `native`
> layer.** Every library and executable that exchanges Rust dyn fat pointers
> across the boundary **must be built with the exact same Rust compiler
> version** to guarantee a stable ABI. Rust makes no ABI stability
> guarantees between compiler releases (vtable layout, metadata encoding,
> etc. may change). Mixing compiler versions between the host and modules
> is **undefined behavior**.
>
> The `abi` layer has no such requirement — its layout is fixed by
> `#[repr(C)]` and the C calling convention.
>
> Pin one toolchain (e.g. via `rust-toolchain.toml`) and rebuild **all**
> crates, libs and executables together with that single version.

## Usage

### Layer 1: `abi` — C interface table (cross-language)

The module exports a function returning a `*const` to a plain
`#[repr(C)]` struct of function pointers. No trait objects involved —
callable from C, C++, Zig, anything with a C FFI.

Two ways to build the module side — **hand-written** or
**macro-generated** — and they produce the exact same contract, so the
host side is identical either way. Both are shown below.

#### A. Without the macro — hand-written vtable

**Module side (Rust)**

```rust,ignore
#[repr(C)]
pub struct MyVtable {
    pub add: unsafe extern "C" fn(ctx: *mut std::ffi::c_void, a: i32, b: i32) -> i32,
    pub name: unsafe extern "C" fn(ctx: *mut std::ffi::c_void) -> *const std::os::raw::c_char,
}

unsafe extern "C" fn add(_ctx: *mut std::ffi::c_void, a: i32, b: i32) -> i32 { a + b }
unsafe extern "C" fn name(_ctx: *mut std::ffi::c_void) -> *const std::os::raw::c_char {
    c"my_module".as_ptr()
}

#[no_mangle]
pub static MY_MODULE_VTABLE: MyVtable = MyVtable { add, name };

#[no_mangle]
pub extern "C" fn my_module_get_vtable() -> *const MyVtable {
    &MY_MODULE_VTABLE
}
```

**Host side (Rust)**

```rust,ignore
let module = unsafe {
    AbiTable::<MyVtable>::load("libmy_module.so", b"my_module_get_vtable\0")?
};
let vtable = module.vtable();
let sum = unsafe { (vtable.add)(std::ptr::null_mut(), 2, 3) }; // 5
```

#### B. With the macro — `#[abi_vtable]`

You write only a trait + an impl; the `abi_vtable` attribute macro
generates the vtable struct, thunks, statics and getter — exactly what
variant A writes by hand.

**Module side (Rust)**

```rust,ignore
use dyn_loader::abi_vtable;

// The ONLY thing you write: a trait + an impl.
#[abi_vtable(name = "calc")]
pub trait Calc {
    fn add(&self, a: i32, b: i32) -> i32;
    fn is_even(&self, x: i32) -> bool;
}

struct MyCalc;
impl Calc for MyCalc {
    fn add(&self, a: i32, b: i32) -> i32 { a + b }
    fn is_even(&self, x: i32) -> bool { x % 2 == 0 }
}

// One line — generates: CalcVtable struct, `calc_add_thunk` /
// `calc_is_even_thunk`, static `CALC_INSTANCE`, static `CALC_VTABLE`,
// and `#[no_mangle] extern "C" fn calc_get_vtable()`.
abi_vtable_impl_calc!(MyCalc, MyCalc);
```

What the macro generates from `#[abi_vtable(name = "calc")]`:

| Entity | Name | Role |
|---|---|---|
| vtable struct | `CalcVtable` | `#[repr(C)]`, one `unsafe extern "C" fn(ctx, ...)` field per method, in declaration order |
| thunks | `calc_add_thunk`, ... | cast `ctx` back to `$impl_ty` and dispatch |
| static instance | `CALC_INSTANCE` | the `$instance` expression |
| static vtable | `CALC_VTABLE` | thunks wired in field order |
| getter | `calc_get_vtable` | `#[no_mangle] extern "C"` entry point for hosts |

**Host side (Rust)** — identical to variant A:

```rust,ignore
let module = unsafe {
    AbiTable::<CalcVtable>::load("libmy_calc.so", b"calc_get_vtable\0")?
};
let vt = module.vtable();
let sum = unsafe { (vt.add)(ctx, 2, 3) }; // ctx = *const MyCalc as *mut c_void
```

Rules the macro enforces for you:

- vtable field order **is** the protocol — it follows trait method
  declaration order; never reorder after release.
- all fn pointers are `extern "C"` (cdecl) with a leading
  `ctx: *mut c_void` (the two protocol laws: 谁分配谁释放 / 谁创建谁操作).
- `$instance` must be const-constructible (unit struct, `const fn`, literal).
- only C scalars (i8..i64, u8..u64, f32/f64, bool, usize/isize) may appear
  in signatures; anything else is rejected at compile time.
- `&mut self` receivers are rejected — use interior mutability
  (e.g. `AtomicI32` fields) instead.

#### Other languages — reference snippets

> The snippets below are **reference material only** — small fragments to
> convey the idea, not complete implementations. The contract is plain C:
> a getter symbol returning a pointer to a `#[repr(C)]`-equivalent struct
> of function pointers, with a leading `ctx` on every call.

**As a module (exposing a vtable)**

```c
/* C — struct, one static instance, one getter */
typedef struct CalcVtable {
    int (*add)(void *ctx, int a, int b);
} CalcVtable;

static int calc_add(void *ctx, int a, int b) { (void)ctx; return a + b; }
static const CalcVtable CALC_VTABLE = { calc_add };
const CalcVtable *calc_get_vtable(void) { return &CALC_VTABLE; }
```

```cpp
// C++ — extern "C" keeps symbols and calling convention C
extern "C" {
static int calc_add(void *ctx, int a, int b) { return a + b; }
static const CalcVtable CALC_VTABLE = { calc_add };
const CalcVtable *calc_get_vtable() { return &CALC_VTABLE; }
}
```

```zig
// Zig — extern struct + callconv(.c) mirror the #[repr(C)] layout
const CalcVtable = extern struct {
    add: *const fn (ctx: ?*anyopaque, a: i32, b: i32) callconv(.c) i32,
};
fn calc_add(ctx: ?*anyopaque, a: i32, b: i32) callconv(.c) i32 {
    _ = ctx;
    return a + b;
}
export fn calc_get_vtable() *const CalcVtable {
    return &CalcVtable{ .add = &calc_add };
}
```

**As a host (loading and calling)**

```c
/* C — dlopen + dlsym, then call through the table */
void *lib = dlopen("libmy_module.so", RTLD_NOW | RTLD_LOCAL);
const CalcVtable *(*get_vtable)(void) = dlsym(lib, "calc_get_vtable");
int sum = get_vtable()->add(NULL, 2, 3);   /* 5 */
```

```cpp
// C++ — dlsym + reinterpret_cast
auto get_vtable = reinterpret_cast<const CalcVtable *(*)()>(
    dlsym(lib, "calc_get_vtable"));
int sum = get_vtable()->add(nullptr, 2, 3);
```

```zig
// Zig — std.DynLib lookup (link libc: zig build-exe host.zig -lc)
var lib = try std.DynLib.open("libmy_module.so");
const get_vtable = lib.lookup(
    *const fn () callconv(.c) *const CalcVtable,
    "calc_get_vtable",
) orelse return error.SymbolNotFound;
const sum = get_vtable().add(null, 2, 3);
```

Rules for foreign hosts and modules:

- match the struct **field order** to the counterpart's declaration order —
  positional contract, no names cross the boundary.
- keep the leading `void*`/`?*anyopaque` ctx parameter even if unused.
- only C scalars (i8..i64, u8..u64, f32/f64, bool, usize/isize) may appear in
  signatures; the macro rejects anything else at compile time.
- pass `NULL`/`null` ctx for stateless tables; instance handles (`AbiRef`)
  pass the ctx they received from the module.

### Layer 2: `native` — Rust fat-pointer bridge

Load Rust trait objects from a `.so` built with the **same compiler
version**. The module exports a `#[no_mangle] extern "C"` entry point
returning an `AbiStableDynRef`.

**Module side (the `.so`/`.dylib`)**

```rust,ignore
use dyn_loader::{AbiStableDynRef, SafeArcDyn};
use std::sync::Arc;

#[no_mangle]
pub extern "C" fn core_ast_transform_entry() -> AbiStableDynRef {
    SafeArcDyn::from_arc(Arc::new(MyTransform) as Arc<dyn Transform>).into_abi()
}
```

**Host side**

```rust,ignore
use dyn_loader::NativeModule;

let module = NativeModule::<dyn Transform>::load(
    "libmy_transform.so",
    b"core_ast_transform_entry\0",
)?;
let transform: &dyn Transform = module.trait_ref();
```

---

## The interface-table mechanism — how cross-language loading works

*(This section explains the mechanism behind the `abi` layer. If you just
want to use it, skip to the tutorial below.)*

### The problem it solves

A Rust `dyn Trait` fat pointer is two words: a data pointer and a pointer
to a **compiler-generated vtable** whose layout is private to the Rust
toolchain. That is why the fat-pointer bridge requires identical compiler
versions. The interface-table mechanism removes the compiler from the
equation:

> **The vtable is not something the compiler generates — it is a plain
> `#[repr(C)]` struct that *you* define and *you* fill.**

### The three pieces

```
        module (.so)                             host
┌─────────────────────────────────┐    ┌──────────────────────────────┐
│  #[repr(C)] struct MyVtable {   │    │  #[repr(C)] struct MyVtable  │
│      add:    unsafe extern "C"  │    │      (same field order —     │
│              fn(ctx,i32,i32),   │    │       this IS the protocol)  │
│      name:   unsafe extern "C"  │    │                              │
│              fn(ctx) -> *const c_char, │  AbiTable::<MyVtable>::load( │
│  }                              │    │      path, b"get_vtable\0")  │
│                                 │    │                              │
│  static MY_VTABLE: MyVtable = … │    │  vtable.add(ctx, 2, 3)       │
│                                 │    │   └─ direct fn-ptr call,     │
│  #[no_mangle]                   │    │      executes in module      │
│  pub extern "C" fn              │    │                              │
│    my_get_vtable() -> *const MyVtable ─► copied by value into host  │
└─────────────────────────────────┘    └──────────────────────────────┘
```

1. **The vtable struct** — a `#[repr(C)]` struct whose fields are all
   `unsafe extern "C"` function pointers. Field order is the calling
   contract: position 0 is `add`, position 1 is `name`, and so on.
   Both sides must compile the *same* struct definition (same order,
   same signatures) — that is the entire compatibility requirement.

2. **The static instance** — the module fills one static instance of
   that struct with its function implementations. It lives in the
   module's static storage and stays valid as long as the library is
   loaded.

3. **The getter entry point** — a `#[no_mangle] extern "C"` function
   returning `*const MyVtable`. The host resolves this one symbol,
   dereferences it, and **copies the vtable by value** (`MyVtable: Copy`).
   After that, every call is a direct function-pointer call — zero
   indirection beyond the pointer itself, no runtime involvement.

### Why this is ABI-stable

- The struct layout is fixed by `#[repr(C)]` and C rules, not by any
  compiler's internal representation.
- Function pointers use the standard C calling convention (`extern "C"`
  / cdecl), which every language can produce.
- Nothing Rust-specific crosses the boundary: no trait objects, no
  panics, no `Result`, no allocator. The host never allocates or frees
  module memory in this layer.

### The ownership tiers on top

The bare vtable is stateless. Two smart-pointer tiers extend it while
keeping the same C-ABI discipline (see the crate docs for the full
ownership model):

- **`AbiRef<T>`** (instance, ref-counted) — the module exports a second
  entry point returning an `AbiStableDynRef`: `{data, vtable, retain,
  release}`. The host wraps it in `AbiRef<T>`; `clone()` calls the
  module's `retain`, `drop()` calls its `release`. The vtable pointer
  inside is reconstructed to `&T` on demand.
- **`AbiBox`** (data, single-owner) — a `{data, len, free}` triple for
  raw buffers. `free` is the *producer's* deallocator; `AbiBoxHandle`
  calls it exactly once on drop.

Both follow the two invariants: *whoever allocates, deallocates* and
*whoever creates, operates*.

---

## Tutorial — plain interface-table bridge

A complete, minimal walkthrough. No theory — just the steps.

### Step 1: Define the vtable (shared header)

Write the struct **once** and give both sides the identical definition.
In Rust:

```rust,ignore
// vtable_def.rs — include this file in BOTH host and module crates
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CalcVtable {
    pub add:  unsafe extern "C" fn(ctx: *mut std::ffi::c_void, i32, i32) -> i32,
    pub mul:  unsafe extern "C" fn(ctx: *mut std::ffi::c_void, i32, i32) -> i32,
    pub name: unsafe extern "C" fn(ctx: *mut std::ffi::c_void) -> *const std::os::raw::c_char,
}
```

Rules:
- All fields are `unsafe extern "C" fn(...)` with a leading `ctx`.
- Derive `Clone, Copy` (required by `AbiTable<T>`).
- Never reorder or insert fields after release — that is a breaking
  ABI change. Append new functions at the **end** only.

### Step 2: Write the module

```rust,ignore
// module crate — Cargo.toml: [lib] crate-type = ["cdylib"]
use vtable_def::CalcVtable;

unsafe extern "C" fn add(_ctx: *mut std::ffi::c_void, a: i32, b: i32) -> i32 { a + b }
unsafe extern "C" fn mul(_ctx: *mut std::ffi::c_void, a: i32, b: i32) -> i32 { a * b }
unsafe extern "C" fn name(_ctx: *mut std::ffi::c_void) -> *const std::os::raw::c_char {
    c"calc".as_ptr()
}

#[no_mangle]
pub static CALC_VTABLE: CalcVtable =
    CalcVtable { add, mul, name };

#[no_mangle]
pub extern "C" fn calc_get_vtable() -> *const CalcVtable {
    &CALC_VTABLE
}
```

Build it:

```bash
cargo build --release   # produces libcalc.so / calc.dll / libcalc.dylib
```

### Step 3: Load and call from the host

```rust,ignore
use dyn_loader::AbiTable;
use vtable_def::CalcVtable;

let module = unsafe {
    AbiTable::<CalcVtable>::load("libcalc.so", b"calc_get_vtable\0")?
};
let vt = module.vtable();
let ctx = std::ptr::null_mut::<std::ffi::c_void>();

unsafe {
    println!("{}", (vt.add)(ctx, 2, 3));   // 5
    println!("{}", (vt.mul)(ctx, 4, 5));   // 20
    let name = std::ffi::CStr::from_ptr((vt.name)(ctx));
    println!("{}", name.to_str().unwrap()); // "calc"
}
// `module` keeps the library loaded; dropping it unloads (ref-counted
// via Arc — other clones of the handle keep it alive).
```

That is the whole flow: **define struct → fill static → export getter →
`AbiTable::load` → call fields**.

### Step 4 (optional): probe before loading

```rust,ignore
use dyn_loader::looks_like_module;

if unsafe { looks_like_module("libcalc.so", b"calc_get_vtable\0") } {
    // safe to AbiTable::load
}
```

### Common pitfalls

| Pitfall | Consequence | Fix |
|---|---|---|
| Field order differs between host and module | Silent wrong-function calls | Share one definition file; never reorder |
| Forgetting `#[no_mangle]` on the getter | Symbol not found at load | Always `#[no_mangle] pub extern "C"` |
| Module built as `rlib` instead of `cdylib` | No `.so` produced | `crate-type = ["cdylib"]` in Cargo.toml |
| Calling a vtable fn after the library unloaded | UB (stale fn pointer) | Keep the `AbiTable`/`DynLib` handle alive |
| Non-`extern "C"` fn pointer in the struct | Compile error or UB | Every field must be `unsafe extern "C" fn` |

### C++ / Zig side of the same protocol

The identical vtable is trivially produced from other languages:

```cpp
// C++ module
extern "C" {
static CalcVtable g_vtable = { add, mul, name };  // same field order
CalcVtable* calc_get_vtable() { return &g_vtable; }
}
```

```zig
// Zig module
pub const CalcVtable = extern struct {   // same field order
    add:  *const fn (ctx: ?*anyopaque, i32, i32) callconv(.c) i32,
    mul:  *const fn (ctx: ?*anyopaque, i32, i32) callconv(.c) i32,
    name: *const fn (ctx: ?*anyopaque) callconv(.c) [*:0]const u8,
};
```

As long as the field order and signatures match, the Rust host above
loads them unchanged.

---

## Toolchain / ABI compatibility matrix

The two loading layers have different ABI stability guarantees:

| Layer | Module path | ABI stability | Cross-compiler-version safe? | Cross-language (C++/Zig) safe? |
|---|---|---|---|---|
| Rust fat-pointer bridge | `native` | ❌ Unstable — depends on Rust vtable layout | **No** — host & module must use the *exact same* compiler version | ❌ No (Rust trait objects only) |
| C interface table | `abi` (`AbiTable<T>`) | ✅ Stable — plain `#[repr(C)]` struct of function pointers | ✅ Yes, to a large extent (layout is fixed by `#[repr(C)]`) | ✅ Yes — any language with a C FFI |

### Verified compiler versions

Cross-version interoperability was verified with an actual host/module matrix
test (module compiled to a `.so` with toolchain A, loaded by a host binary
compiled with toolchain B). **All 16 combinations passed** (both layers) as of
2026-09:

| Toolchain pair (host ↔ module)                    | `native` (fat-pointer bridge)                     | `abi` (C interface table)        |
| --------------------------------------------------- | --------------------------------------------------- | ---------------------------------- |
| `nightly-2025-05-06` ↔ `stable 1.98.1` (cross) | ✅ Verified                                         | ✅ Verified                        |
| `nightly-2025-05-06` ↔ `1.95.0` (cross)        | ✅ Verified                                         | ✅ Verified                        |
| `nightly-2025-05-06` ↔ `1.92.0` (cross)        | ✅ Verified                                         | ✅ Verified                        |
| `stable 1.98.1` ↔ `1.95.0` (cross)             | ✅ Verified                                         | ✅ Verified                        |
| `stable 1.98.1` ↔ `1.92.0` (cross)             | ✅ Verified                                         | ✅ Verified                        |
| `1.95.0` ↔ `1.92.0` (cross)                    | ✅ Verified                                         | ✅ Verified                        |
| Same-version pairs (all 4 toolchains)               | ✅ Verified                                         | ✅ Verified                        |
| **Other / future versions**                   | ❌**Undefined behavior — do not rely on it** | ⚠️ Usually works, not guaranteed |

Notes:

- The cross-version `native` results above are an **observation, not a
  guarantee**: vtable layout happened to be identical across these four
  toolchains (spanning nightly-2025-05-06 through stable-1.98.1). Rust
  officially makes no ABI stability promise between compiler releases — a
  future release may break it silently. Always re-run the matrix test when
  adopting a new toolchain, and prefer exact version alignment in production.
- The `abi` layer is layout-stable by construction (`#[repr(C)]` struct of
  function pointers), but both sides must still compile the *same* `T`
  definition (same field order, same pointer widths).
- The `AbiDynFatPtr` / `AbiStableDynRef` structs are `#[repr(C)]` and
  layout-stable across versions; what is *not* guaranteed stable is the
  **vtable contents** behind a `dyn Trait` pointer, which is why `native`
  requires version alignment.

## Safety

Loading dynamic libraries and unpacking raw fat pointers is inherently unsafe.
See the `# Safety` sections on each API. The retain/release function pointers
give you Arc-like reference counting across the library boundary, but the
caller is still responsible for ABI compatibility (see the warning above).

## License

Apache License 2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE).
Redistributions must retain the attribution notices in `NOTICE`
(Apache-2.0 §4(c)/(d)).
