//! # dyn-loader — standardized module loading protocol
//!
//! A **language-agnostic module loading protocol** built on the C ABI.
//! Any dynamic library (.so/.dll/.dylib) that follows
//! the protocol becomes a *module* that any host — in any language — can
//! load, call, share and release.
//!
//! ## Protocol boundary rule (first law)
//!
//! **Only standard C interfaces may cross the module boundary.** Legal
//! transports are: `extern "C"` functions, C scalar types, `#[repr(C)]` POD
//! structs, C strings, and function pointers. Language-runtime objects
//! (C++ exceptions, Zig error unions, Rust panics, GC references) must never
//! cross. The single controlled exception is the [`native`] fat-pointer
//! pair, valid only under the same-compiler-version clause.
//!
//! ## Ownership invariants
//!
//! 1. **Whoever allocates, deallocates** — every pointer crossing the
//!    boundary is paired with a free/release function pointer belonging to
//!    the allocating module.
//! 2. **Whoever creates, operates** — the host receives a calling convention
//!    (vtable layout) plus function pointers; all operations execute inside
//!    the creator's module.
//!
//! The only thing that ever crosses the boundary, in any language, is the
//! protocol itself: pointers plus function pointers.
//!
//! ## Layers
//!
//! - **[`native`]** — the Rust convenience layer: skip hand-written vtables
//!   by exchanging native Rust fat pointers (`SafeArcDyn`, `NativeModule`).
//!   Requires identical toolchain on both sides. Rust-to-Rust only.
//! - **[`abi`]** *(feature `abi`, enabled by default)* — the protocol
//!   standard layer: interface tables ([`AbiTable<T>`]), ref-counted
//!   interface references ([`AbiRef<T>`], [`AbiStableDynRef`]) and
//!   single-owner data boxes ([`AbiBox`]). Pure C ABI, works from
//!   C/C++/Zig/anything. Disable the feature for Rust-only hosts that
//!   never touch C interface tables.
//!
//! ## Module lifecycle
//!
//! ```ignore
//! // Host loads a module and its interface table (feature "abi"):
//! let iface = unsafe { AbiTable::<MyInterface>::load("libmy.so", b"my_get_interface\0")? };
//! let sum = unsafe { (iface.get().add)(1, 2) };
//!
//! // Ref-counted module object (multi-owner, feature "abi"):
//! let obj = unsafe { AbiRef::<MyInterface>::load(&path, b"my_get_dyn\0")? };
//!
//! // Single-owner data box (freed by the producer module, feature "abi"):
//! let data = unsafe { AbiBoxHandle::load(&path, b"my_get_data\0")? };
//!
//! // Or the native fat-pointer bridge (no feature needed):
//! let module = NativeModule::<dyn MyTrait>::load("libmy.so", b"my_entry\0")?;
//! ```

#[cfg(feature = "abi")]
pub mod abi;
#[path = "native.rs"]
pub mod native;
mod helpers;

#[cfg(feature = "abi")]
pub use abi::{
    AbiBox, AbiBoxHandle, AbiRef, AbiTable, GeneratedFunction, MathModuleVtable, MathSession,
    abi_box_free_rust,
};
pub use helpers::{DynLib, looks_like_module};
#[cfg(feature = "abi")]
pub use abi_vtable_macro::abi_vtable;
pub use native::{
    AbiDynFatPtr, AbiStableDynRef, ModuleDynEntryPoint, NativeModule, ReleaseFn, RetainFn,
    SafeArcDyn, pack_fat_ptr, unpack_fat_ptr,
};
