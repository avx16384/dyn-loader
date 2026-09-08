# dyn-loader

Dynamic library loader with **two plugin-loading modes**:

1. **`dyn` mode** — Rust fat-pointer bridge: load Rust trait objects from
   `.so`/`.dylib` plugin files, with Arc-like retain/release.
2. **`cdyn` mode** — COM-style C function tables (`VTablePlugin<T>`): plain
   `#[repr(C)]` function-pointer structs, ABI-stable, usable from C/C++/Zig.

Core types:

- **`DynLib`**: wraps `libloading::Library` with `Arc` for shared ownership.
- **`AbiDynFatPtr`**: ABI-stable representation of a Rust fat pointer
  (data ptr + vtable ptr), `#[repr(C)]`.
- **`AbiStableDynRef`**: fat pointer + retain/release function pointers,
  enabling safe cross-boundary Arc-like reference counting.
- **`SafeArcDyn<T>`**: safe, cloneable handle over an `AbiStableDynRef`.
- **`DynPlugin<T>`**: loaded plugin holding a `SafeArcDyn<T>` that dereferences
  to `&T` for calling trait methods on the loaded object.
- **`VTablePlugin<T>`**: loaded COM-style function table (cdyn mode).

## ⚠️ IMPORTANT — ABI compatibility / compiler version alignment

> **You must strictly align the Rust compiler version.** Every library and
> executable that exchanges dyn fat pointers across the boundary **must be
> built with the exact same Rust compiler version** to guarantee a stable
> ABI. Rust makes no ABI stability guarantees between compiler releases
> (vtable layout, metadata encoding, etc. may change). Mixing compiler
> versions between the host and plugins is **undefined behavior**.
>
> Pin one toolchain (e.g. via `rust-toolchain.toml`) and rebuild **all**
> crates, libs and executables together with that single version.

## Usage

### Mode 1: `dyn` — Rust fat-pointer bridge

**Plugin side (the `.so`/`.dylib`)**

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
use dyn_loader::DynPlugin;

let plugin = DynPlugin::<dyn Transform>::load(
    "libmy_transform.so",
    b"core_ast_transform_entry\0",
)?;
let transform: &dyn Transform = plugin.trait_ref();
```

### Mode 2: `cdyn` — COM-style C function table (cross-language)

The plugin exports a function returning a `*const` to a plain
`#[repr(C)]` struct of function pointers. No trait objects involved —
callable from C, C++, Zig, anything with a C FFI.

**Plugin side (Rust)**

```rust,ignore
use dyn_loader::VTablePlugin;

#[repr(C)]
pub struct MyVtable {
    pub add: unsafe extern "C" fn(i32, i32) -> i32,
    pub name: unsafe extern "C" fn() -> *const std::os::raw::c_char,
}

unsafe extern "C" fn add(a: i32, b: i32) -> i32 { a + b }
unsafe extern "C" fn name() -> *const std::os::raw::c_char {
    c"my_plugin".as_ptr()
}

#[no_mangle]
pub static MY_PLUGIN_VTABLE: MyVtable = MyVtable { add, name };

#[no_mangle]
pub extern "C" fn my_plugin_get_vtable() -> *const MyVtable {
    &MY_PLUGIN_VTABLE
}
```

**Host side (Rust)**

```rust,ignore
let plugin = unsafe {
    VTablePlugin::<MyVtable>::load("libmy_plugin.so", b"my_plugin_get_vtable\0")?
};
let vtable = plugin.vtable();
let sum = unsafe { (vtable.add)(2, 3) }; // 5
```

The same vtable layout can be produced from any language with a C FFI
(C, C++, Zig, ...): just export a function returning `*const MyVtable`
to a `#[repr(C)]`-equivalent struct.

### Mode 2 shortcut: `#[abi_vtable]` — shadow mode

Writing the vtable struct, thunks, static and getter by hand (as above) is
error-prone. The `abi_vtable` attribute macro generates **all of it** from a
plain trait definition — abi mode with dyn-mode ergonomics:

**Plugin side (Rust)**

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
// `calc_is_even_thunk`, static `CALC_VTABLE`, and `#[no_mangle]
// extern "C" fn calc_get_vtable()`.
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

**Host side** — identical to plain Mode 2:

```rust,ignore
let plugin = unsafe {
    AbiTable::<CalcVtable>::load("libmy_calc.so", b"calc_get_vtable\0")?
};
let vt = plugin.vtable();
let sum = unsafe { (vt.add)(ctx, 2, 3) }; // ctx = *const MyCalc as *mut c_void
```

Rules the macro enforces for you:

- vtable field order **is** the protocol — it follows trait method
  declaration order; never reorder after release.
- all fn pointers are `extern "C"` (cdecl) with a leading
  `ctx: *mut c_void` (the two protocol laws: 谁分配谁释放 / 谁创建谁操作).
- `$instance` must be const-constructible (unit struct, `const fn`, literal).

---

## The cdyn mechanism — how cross-language loading works

*(This section explains the mechanism behind "Mode 2". If you just want
to use it, skip to the tutorial below.)*

### The problem it solves

A Rust `dyn Trait` fat pointer is two words: a data pointer and a pointer
to a **compiler-generated vtable** whose layout is private to the Rust
toolchain. That is why the fat-pointer bridge requires identical compiler
versions. The cdyn mechanism removes the compiler from the equation:

> **The vtable is not something the compiler generates — it is a plain
> `#[repr(C)]` struct that *you* define and *you* fill.**

### The three pieces

```
        plugin module (.so)                      host module
┌─────────────────────────────────┐    ┌──────────────────────────────┐
│  #[repr(C)] struct MyVtable {   │    │  #[repr(C)] struct MyVtable  │
│      add:    unsafe extern "C"  │    │      (same field order —     │
│              fn(i32,i32)->i32,  │    │       this IS the protocol)  │
│      name:   unsafe extern "C"  │    │                              │
│              fn() -> *const c_char, │  │  AbiTable::<MyVtable>::load( │
│  }                              │    │      path, b"get_vtable\0")  │
│                                 │    │                              │
│  static MY_VTABLE: MyVtable = … │    │  vtable.add(2, 3)            │
│                                 │    │   └─ direct fn-ptr call,     │
│  #[no_mangle]                   │    │      executes in plugin      │
│  pub extern "C" fn              │    │                              │
│    my_get_vtable() -> *const MyVtable ─► copied by value into host  │
└─────────────────────────────────┘    └──────────────────────────────┘
```

1. **The vtable struct** — a `#[repr(C)]` struct whose fields are all
   `unsafe extern "C"` function pointers. Field order is the calling
   contract: position 0 is `add`, position 1 is `name`, and so on.
   Both sides must compile the *same* struct definition (same order,
   same signatures) — that is the entire compatibility requirement.

2. **The static instance** — the plugin fills one static instance of
   that struct with its function implementations. It lives in the
   plugin's static storage and stays valid as long as the library is
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
  plugin memory in this mode.

### The ownership tiers on top

The bare vtable is stateless. Two smart-pointer tiers extend it while
keeping the same C-ABI discipline (see the crate docs for the full
ownership model):

- **`AbiRef<T>`** (instance, ref-counted) — the plugin exports a second
  entry point returning an `AbiStableDynRef`: `{data, vtable, retain,
  release}`. The host wraps it in `AbiRef<T>`; `clone()` calls the
  plugin's `retain`, `drop()` calls its `release`. The vtable pointer
  inside is reconstructed to `&T` on demand.
- **`AbiBox`** (data, single-owner) — a `{data, len, free}` triple for
  raw buffers. `free` is the *producer's* deallocator; `AbiBoxHandle`
  calls it exactly once on drop.

Both follow the two invariants: *whoever allocates, deallocates* and
*whoever creates, operates*.

---

## Tutorial — plain vtable bridge (cdyn mode)

A complete, minimal walkthrough. No theory — just the steps.

### Step 1: Define the vtable (shared header)

Write the struct **once** and give both sides the identical definition.
In Rust:

```rust,ignore
// vtable_def.rs — include this file in BOTH host and plugin crates
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CalcVtable {
    pub add:  unsafe extern "C" fn(i32, i32) -> i32,
    pub mul:  unsafe extern "C" fn(i32, i32) -> i32,
    pub name: unsafe extern "C" fn() -> *const std::os::raw::c_char,
}
```

Rules:
- All fields are `unsafe extern "C" fn(...)`.
- Derive `Clone, Copy` (required by `AbiTable<T>`).
- Never reorder or insert fields after release — that is a breaking
  ABI change. Append new functions at the **end** only.

### Step 2: Write the plugin

```rust,ignore
// plugin crate — Cargo.toml: [lib] crate-type = ["cdylib"]
use vtable_def::CalcVtable;

unsafe extern "C" fn add(a: i32, b: i32) -> i32 { a + b }
unsafe extern "C" fn mul(a: i32, b: i32) -> i32 { a * b }
unsafe extern "C" fn name() -> *const std::os::raw::c_char {
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

let table = unsafe {
    AbiTable::<CalcVtable>::load("libcalc.so", b"calc_get_vtable\0")?
};
let vt = table.get();

unsafe {
    println!("{}", (vt.add)(2, 3));   // 5
    println!("{}", (vt.mul)(4, 5));   // 20
    let name = std::ffi::CStr::from_ptr((vt.name)());
    println!("{}", name.to_str().unwrap()); // "calc"
}
// `table` keeps the library loaded; dropping it unloads (ref-counted
// via Arc — other clones of the handle keep it alive).
```

That is the whole flow: **define struct → fill static → export getter →
`AbiTable::load` → call fields**.

### Step 4 (optional): probe before loading

```rust,ignore
use dyn_loader::looks_like_plugin;

if unsafe { looks_like_plugin("libcalc.so", b"calc_get_vtable\0") } {
    // safe to AbiTable::load
}
```

### Common pitfalls

| Pitfall | Consequence | Fix |
|---|---|---|
| Field order differs between host and plugin | Silent wrong-function calls | Share one definition file; never reorder |
| Forgetting `#[no_mangle]` on the getter | Symbol not found at load | Always `#[no_mangle] pub extern "C"` |
| Plugin built as `rlib` instead of `cdylib` | No `.so` produced | `crate-type = ["cdylib"]` in Cargo.toml |
| Calling a vtable fn after the library unloaded | UB (stale fn pointer) | Keep the `AbiTable`/`DynLib` handle alive |
| Non-`extern "C"` fn pointer in the struct | Compile error or UB | Every field must be `unsafe extern "C" fn` |

### C++ / Zig side of the same protocol

The identical vtable is trivially produced from other languages:

```cpp
// C++ plugin
extern "C" {
static CalcVtable g_vtable = { add, mul, name };  // same field order
CalcVtable* calc_get_vtable() { return &g_vtable; }
}
```

```zig
// Zig plugin
pub const CalcVtable = extern struct {   // same field order
    add:  *const fn (i32, i32) callconv(.c) i32,
    mul:  *const fn (i32, i32) callconv(.c) i32,
    name: *const fn () callconv(.c) [*:0]const u8,
};
```

As long as the field order and signatures match, the Rust host above
loads them unchanged.

---

## Toolchain / ABI compatibility matrix

The two loading modes have different ABI stability guarantees:

| Mode | Module | ABI stability | Cross-compiler-version safe? | Cross-language (C++/Zig) safe? |
|---|---|---|---|---|
| Rust fat-pointer bridge | `dyn_mod` | ❌ Unstable — depends on Rust vtable layout | **No** — host & plugin must use the *exact same* compiler version | ❌ No (Rust trait objects only) |
| COM-style C function table | `cdyn` (`VTablePlugin<T>`) | ✅ Stable — plain `#[repr(C)]` struct of function pointers | ✅ Yes, to a large extent (layout is fixed by `#[repr(C)]`) | ✅ Yes — any language with a C FFI |

### Verified compiler versions

Cross-version interoperability was verified with an actual host/plugin matrix
test (plugin compiled to a `.so` with toolchain A, loaded by a host binary
compiled with toolchain B). **All 16 combinations passed** (both modes) as of
2026-09:

| Toolchain pair (host ↔ plugin)                     | `dyn_mod` (fat-pointer bridge)                    | `cdyn` (C function table)        |
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

- The cross-version `dyn_mod` results above are an **observation, not a
  guarantee**: vtable layout happened to be identical across these four
  toolchains (spanning nightly-2025-05-06 through stable-1.98.1). Rust
  officially makes no ABI stability promise between compiler releases — a
  future release may break it silently. Always re-run the matrix test when
  adopting a new toolchain, and prefer exact version alignment in production.
- `cdyn` mode is layout-stable by construction (`#[repr(C)]` struct of
  function pointers), but both sides must still compile the *same* `T`
  definition (same field order, same pointer widths).
- The `AbiDynFatPtr` / `AbiStableDynRef` structs are `#[repr(C)]` and
  layout-stable across versions; what is *not* guaranteed stable is the
  **vtable contents** behind a `dyn Trait` pointer, which is why `dyn_mod`
  requires version alignment.

## Safety

Loading dynamic libraries and unpacking raw fat pointers is inherently unsafe.
See the `# Safety` sections on each API. The retain/release function pointers
give you Arc-like reference counting across the library boundary, but the
caller is still responsible for ABI compatibility (see the warning above).

## License

MIT
