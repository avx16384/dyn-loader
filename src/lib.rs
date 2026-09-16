//! # dyn-loader — the loading gate
//!
//! Every dynamic library a process opens goes through one door. This crate is
//! that door, plus the Rust-native fast path through it.
//!
//! Two things live here:
//!
//! - **[`DynLib`]** — the gate. Locate, load, resolve a symbol. Nothing else
//!   in the system calls `dlopen`; the layer above builds on this handle.
//! - **[`native`]** — the fat-pointer bridge. Skip hand-written vtables by
//!   exchanging native Rust fat pointers ([`SafeArcDyn`], [`NativeModule`]).
//!   Requires the identical toolchain on both sides; Rust-to-Rust only.
//!
//! The C interface-table layer — positional `#[repr(C)]` tables, ref-counted
//! handles, single-owner data boxes — is a separate crate, `cdyn-loader`,
//! built on this one. A host that only needs the Rust fast path does not pay
//! for it.
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
//! ### Every struct crossing the boundary must be `#[repr(C)]`
//!
//! Not a style preference — a requirement, for two measured reasons.
//!
//! **The default representation reorders fields.** `repr(Rust)` is free to
//! lay fields out in whatever order minimizes padding, and does:
//!
//! ```text
//! struct Bad { a: u8, b: u64, c: u8 }        // repr(Rust)
//! #[repr(C)] struct Good { a: u8, b: u64, c: u8 }
//!
//! measured:
//!   Bad   a @8, b @0,  c @9,  size 16   — b moved to the front
//!   Good  a @0, b @8,  c @16, size 24   — declaration order
//! ```
//!
//! The layout is deterministic for one compiler and one declaration, but the
//! algorithm is unspecified and may change in any release. A module built
//! with one toolchain and a host built with another have no common promise
//! about where a field lives.
//!
//! **Padding bytes are not initialized.** A `#[repr(C)]` struct has padding
//! when its fields do not tile the size, and those bytes hold whatever was
//! in memory before:
//!
//! ```text
//! #[repr(C)] struct P { a: u8, b: u64 }   // bytes 1..8 are padding
//!
//! two structs, identical fields, different memory underneath:
//!   a = [01, aa, aa, aa, aa, aa, aa, aa, 02, 00, …]
//!   b = [01, bb, bb, bb, bb, bb, bb, bb, 02, 00, …]
//!   fields equal? true      bytes equal? false
//! ```
//!
//! So **never hash, `memcmp`, or otherwise read a struct as raw bytes** to
//! decide whether two payloads match. Compare or serialize field by field. If
//! a byte-level digest is genuinely needed, the struct must be zeroed first
//! or carry explicit padding fields.
//!
//! This applies to payloads too: a buffer that will be hashed must have its
//! padding written, not merely its fields.
//!
//! The rules are computed and checked in `tests/alignment_matrix.rs`, which
//! holds an independent implementation of the C layout rules and compares it
//! against what the compiler actually produces across a grid of field types.
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
//! ## Module lifecycle
//!
//! ```ignore
//! // In the module (.so):
//! #[no_mangle]
//! pub extern "C" fn my_transform_entry() -> AbiStableDynRef {
//!     SafeArcDyn::from_arc(Arc::new(MyTransform) as Arc<dyn Transform>).into_abi()
//! }
//!
//! // In the host:
//! let module = NativeModule::<dyn Transform>::load("libmy_transform.so", b"my_transform_entry\0")?;
//! let transform: &dyn Transform = module.trait_ref();
//! ```
//!
//! ## Layers built on this one
//!
//! | Crate | What it adds |
//! |---|---|
//! | `cdyn-loader` | C interface tables: positional `#[repr(C)]` dispatch, ref-counted handles, single-owner data boxes |
//! | `dyn-abi-map` | ABI policy: compiler facts mapped to levels, compared before dialling |
//! | `dyn-support` | Delivery: fetch a platform package, put it next to the executable |

#[cfg(feature = "abi-map")]
pub mod reconcile;
pub mod entry;
#[path = "native.rs"]
pub mod native;
mod helpers;

pub use entry::HeadEntry;
pub use helpers::{DynLib, display_symbol, looks_like_module};
pub use native::{
    AbiDynFatPtr, AbiStableDynRef, ModuleDynEntryPoint, NativeModule, ReleaseFn, RetainFn,
    SafeArcDyn, pack_fat_ptr, unpack_fat_ptr,
};
#[cfg(feature = "abi-map")]
pub use reconcile::Verdict;
