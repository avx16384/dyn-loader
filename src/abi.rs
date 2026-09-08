//! # abi — protocol standard layer: C interface tables (ABI-stable)
//!
//! Load Copy-sized vtable/descriptor structs from dynamic libraries using
//! plain C function tables — no Rust trait objects, no named-based access,
//! purely positional dispatch. This is the COM+-like emulated vtable system
//! (formerly the separate `cdyn-loader` crate).
//!
//! Unlike [`crate::native`], this mode is ABI-stable across languages:
//! the C++ SDK (`cdyn-loader-sdks/cpp`) and Zig SDK (`cdyn-loader-sdks/zig`)
//! build plugins that match these layouts exactly.
//!
//! ## Cross-module memory ownership model
//!
//! The core of every cross-module boundary is **who manages memory**, and it
//! reduces to two invariant rules that hold in every mode of this crate:
//!
//! 1. **Whoever allocates, deallocates** (谁分配谁释放). Memory is never
//!    freed across the boundary by the borrower: every pointer crossing the
//!    boundary travels paired with a deallocation function pointer that
//!    belongs to the module which allocated the memory. Allocators (and CRTs)
//!    are not guaranteed to match across module boundaries, so the free must
//!    execute inside the allocator's own module.
//!
//! 2. **Whoever creates, operates** (谁创建谁操作). Behavior also belongs to
//!    the creator: the host receives a *calling convention* (a `#[repr(C)]`
//!    vtable layout) plus function pointers, and every operation — method
//!    dispatch through thunks, with the instance pointer passed back as the
//!    leading `ctx` argument — executes inside the creator's module. The host
//!    never "has" the object; it only holds pointers and dials the protocol.
//!
//! Together these mean the *only* thing that ever crosses a module boundary,
//! in any language, is the protocol itself: a pointer plus function pointers
//! that route both deallocation and operation back to the owning module.
//!
//! This crate provides three ownership tiers, all following those rules:
//!
//! | Tier | Type | Ownership | Free function |
//! |---|---|---|---|
//! | Stateless | [`AbiTable<T>`] | none (static vtable) | — |
//! | Instance (multi-owner) | [`AbiRef<T>`] | ref-counted via `retain`/`release` fn ptrs in `AbiStableDynRef` | provided by the plugin |
//! | Data (single-owner) | [`AbiBox`] / [`AbiBoxHandle`] | move-only, exactly one owner | `free` fn ptr in the box, provided by the allocating module |
//!
//! Cross-module safety rules enforced by construction:
//!
//! 1. **Allocator symmetry** — `free`/`release` always execute inside the
//!    module that allocated (the function pointer belongs to that module's
//!    code, e.g. `AbiBox::from_vec` pairs with a Rust-plugin allocator).
//! 2. **Library lifetime** — [`AbiBoxHandle`] and [`AbiRef<T>`] own a
//!    [`DynLib`](crate::DynLib) (Arc-shared), so the library cannot be
//!    unloaded while a handle (and thus a free/release fn ptr) still exists.
//!
//! ## Usage
//!
//! ```ignore
//! #[repr(C)]
//! struct MyVtable {
//!     add: unsafe extern "C" fn(i32, i32) -> i32,
//!     name: unsafe extern "C" fn() -> *const std::ffi::c_char,
//! }
//!
//! let plugin = unsafe { AbiTable::<MyVtable>::load("libmy.so", b"my_get_vtable\0")? };
//! let n = unsafe { (plugin.vtable().add)(1, 2) };
//! ```

use std::ffi::c_char;
use std::marker::PhantomData;
use std::path::Path;

use anyhow::{Context, Result};

use crate::DynLib;
use crate::native::{AbiStableDynRef, ModuleDynEntryPoint as _ModuleDynEntry};

// ---------------------------------------------------------------------------
// AbiTable — simpler vtable-based loading (Copy types only)
// ---------------------------------------------------------------------------

/// Load a Copy-sized vtable/descriptor struct from a dynamic library.
pub struct AbiTable<T: Copy> {
    _lib: DynLib,
    vtable: T,
}

impl<T: Copy> AbiTable<T> {
    /// Load a vtable struct from a dynamic library.
    ///
    /// # Safety
    ///
    /// - The target file must be a valid dynamic library.
    /// - The symbol must refer to a static vtable-compatible value of type `T`.
    pub unsafe fn load(path: &Path, symbol: &[u8]) -> Result<Self> {
        let lib = unsafe { DynLib::load(path) }?;
        unsafe { Self::from_lib(lib, symbol) }
    }

    /// # Safety
    ///
    /// The symbol must refer to a static vtable-compatible value of type `T`.
    pub unsafe fn from_lib(lib: DynLib, symbol: &[u8]) -> Result<Self> {
        let getter: unsafe extern "C" fn() -> *const T = unsafe { lib.symbol(symbol) }?;
        let vtable = unsafe { getter() };
        let vtable = unsafe { vtable.as_ref() }
            .copied()
            .ok_or_else(|| anyhow::anyhow!("vtable getter returned null"))?;
        Ok(Self { _lib: lib, vtable })
    }

    pub fn vtable(&self) -> &T {
        &self.vtable
    }
}

// ---------------------------------------------------------------------------
// C++ math module VTable ABI — matches cpp/math_module/include/math_vtable.h
// ---------------------------------------------------------------------------

/// Opaque handle for a C++ MathSession
pub type MathSession = *mut std::ffi::c_void;

/// Descriptor for a generated math function (matches C++ GeneratedFunction)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GeneratedFunction {
    pub name: *const c_char,
    pub signature: *const c_char,
    pub arg_count: u32,
    pub id: u32,
}

/// Vtable struct matching C++ MathModuleVtable layout exactly
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MathModuleVtable {
    // Session lifecycle
    pub create_session: unsafe extern "C" fn() -> MathSession,
    pub destroy_session: unsafe extern "C" fn(MathSession),

    // Math operations (lazy — only "generated" if called)
    pub add_i32: unsafe extern "C" fn(MathSession, i32, i32) -> i32,
    pub add_i64: unsafe extern "C" fn(MathSession, i64, i64) -> i64,
    pub add_f64: unsafe extern "C" fn(MathSession, f64, f64) -> f64,
    pub mul_i32: unsafe extern "C" fn(MathSession, i32, i32) -> i32,
    pub mul_f64: unsafe extern "C" fn(MathSession, f64, f64) -> f64,
    pub pi: unsafe extern "C" fn(MathSession) -> f64,
    pub tau: unsafe extern "C" fn(MathSession) -> f64,

    // Code-generation introspection
    pub generated_count: unsafe extern "C" fn(MathSession) -> u32,
    pub generated_at: unsafe extern "C" fn(MathSession, u32) -> GeneratedFunction,

    // Module info
    pub module_name: unsafe extern "C" fn() -> *const c_char,
    pub module_version: unsafe extern "C" fn() -> u32,
}

// SAFETY: MathModuleVtable contains only function pointers and is safe to Send/Sync
unsafe impl Send for MathModuleVtable {}
unsafe impl Sync for MathModuleVtable {}

// ---------------------------------------------------------------------------
// AbiBox — cross-module data box (single owner + free fn ptr)
// ---------------------------------------------------------------------------

/// A cross-module data box: memory allocated and freed by the **same** module.
///
/// This is the cdyn mode's raw-data smart pointer, complementing the
/// ref-counted instance handle [`AbiRef<T>`]:
///
/// - Instance objects (with vtables) → ref-counted, use `AbiStableDynRef`'s
///   `retain`/`release` via [`AbiRef<T>`].
/// - Raw data buffers (payloads, serialized blobs, arrays) → single-owner,
///   use this struct's `free` function pointer.
///
/// Both share the same principle: the deallocator lives in the module that
/// allocated the memory, so allocator mismatches across module/CRT boundaries
/// (notably on Windows) can never corrupt the heap.
///
/// The box is **move-only**: there is exactly one owner, and dropping it
/// (or explicitly calling `free`) hands the memory back to its origin module.
/// Ref-counted sharing of data should be modeled as an instance instead.
///
/// ## C/C++/Zig side
///
/// Any language can produce a box: allocate, fill, and export
/// `struct { void* data; size_t len; void (*free)(void*, size_t); }`
/// where `free` calls the module's own allocator (C++: `operator delete[]`
/// / `std::free`, Zig: `allocator.free`). The layout is `#[repr(C)]` /
/// plain C struct.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AbiBox {
    /// Pointer to the data. Allocated by the producing module.
    pub data: *mut std::ffi::c_void,
    /// Length in **bytes**.
    pub len: usize,
    /// Frees `data`. **Must** be a function from the module that allocated it.
    pub free: unsafe extern "C" fn(data: *mut std::ffi::c_void, len: usize),
}

impl AbiBox {
    /// A null box (no data, free is a no-op).
    pub const fn null() -> Self {
        Self {
            data: std::ptr::null_mut(),
            len: 0,
            free: abi_box_free_noop,
        }
    }

    pub fn is_null(&self) -> bool {
        self.data.is_null()
    }

    /// **Plugin side (Rust)** — wrap an owned `Vec<u8>` into a box.
    ///
    /// The buffer is exact-fit (`into_boxed_slice`), so `len == capacity` and
    /// the paired [`abi_box_free_rust`] can reconstruct and free it. The
    /// returned `free` pointer executes in the plugin's code — the module
    /// that owns the allocation.
    pub fn from_vec(v: Vec<u8>) -> Self {
        let boxed: Box<[u8]> = v.into_boxed_slice();
        let len = boxed.len();
        let data = Box::into_raw(boxed) as *mut std::ffi::c_void;
        Self {
            data,
            len,
            free: abi_box_free_rust,
        }
    }

    /// View the contents as a byte slice.
    ///
    /// # Safety
    ///
    /// `data` must point to `len` readable bytes for the lifetime of `&self`.
    pub unsafe fn as_slice(&self) -> &[u8] {
        if self.data.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.data as *const u8, self.len) }
        }
    }

    /// Hand ownership back to the caller (no `free` on drop afterwards).
    pub fn into_raw(self) -> (Self, bool) {
        let consumed = !self.is_null();
        (self, consumed)
    }
}

// SAFETY: AbiBox is a raw pointer + length + fn pointer; thread-safety is
// the plugin's declared guarantee (same policy as AbiStableDynRef).
unsafe impl Send for AbiBox {}
unsafe impl Sync for AbiBox {}

unsafe extern "C" fn abi_box_free_noop(_: *mut std::ffi::c_void, _: usize) {}

/// **Rust plugin** free function paired with [`AbiBox::from_vec`].
///
/// Executes in the plugin module; reconstructs the exact-fit boxed slice and
/// drops it with the plugin's own allocator.
pub unsafe extern "C" fn abi_box_free_rust(
    data: *mut std::ffi::c_void,
    len: usize,
) {
    if data.is_null() {
        return;
    }
    let slice_ptr = std::slice::from_raw_parts_mut(data as *mut u8, len) as *mut [u8];
    drop(unsafe { Box::from_raw(slice_ptr) });
}

/// Host-side owning handle over a [`AbiBox`] received from a foreign module.
///
/// Drop → calls the box's `free` (the **producer module's** deallocator).
/// The handle optionally owns the originating [`DynLib`](crate::DynLib) so the
/// library stays loaded while the `free` function pointer is live — the
/// cross-module-safety guarantee.
pub struct AbiBoxHandle {
    _lib: Option<DynLib>,
    inner: Option<AbiBox>,
}

impl AbiBoxHandle {
    /// Adopt a box received across the boundary, keeping `lib` loaded.
    pub fn from_box(box_: AbiBox, lib: DynLib) -> Self {
        Self {
            _lib: Some(lib),
            inner: Some(box_),
        }
    }

    /// Adopt a box whose originating library is kept alive by other means.
    ///
    /// # Safety
    ///
    /// The caller must guarantee the producer library outlives this handle.
    pub unsafe fn from_box_unowned(box_: AbiBox) -> Self {
        Self {
            _lib: Some(DynLib::unowned()),
            inner: Some(box_),
        }
    }

    /// Load from a dynamic library entry point returning a `AbiBox`.
    ///
    /// # Safety
    ///
    /// The entry must return a valid `AbiBox` whose `free` belongs to that
    /// library.
    pub unsafe fn load(path: &Path, symbol: &[u8]) -> Result<Self> {
        let lib = unsafe { DynLib::load(path) }?;
        unsafe { Self::from_lib(lib, symbol) }
    }

    /// # Safety
    ///
    /// The entry must return a valid `AbiBox` whose `free` belongs to `lib`.
    pub unsafe fn from_lib(lib: DynLib, symbol: &[u8]) -> Result<Self> {
        let getter: unsafe extern "C" fn() -> AbiBox = unsafe { lib.symbol(symbol) }?;
        let box_ = unsafe { getter() };
        if box_.is_null() {
            anyhow::bail!("box entry returned null AbiBox");
        }
        Ok(Self::from_box(box_, lib))
    }

    /// View the contents.
    pub fn as_slice(&self) -> &[u8] {
        match &self.inner {
            Some(b) => unsafe { b.as_slice() },
            None => &[],
        }
    }

    pub fn len(&self) -> usize {
        self.inner.as_ref().map_or(0, |b| b.len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Release ownership of the raw box without freeing
    /// (the caller becomes responsible for calling `free`).
    pub fn into_raw(mut self) -> AbiBox {
        self.inner.take().unwrap_or_else(AbiBox::null)
    }
}

impl std::ops::Deref for AbiBoxHandle {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Drop for AbiBoxHandle {
    fn drop(&mut self) {
        if let Some(box_) = self.inner.take() {
            if !box_.is_null() {
                // Executes in the producer module — never our allocator.
                unsafe { (box_.free)(box_.data, box_.len) };
            }
        }
    }
}

// SAFETY: the box's memory and free fn are governed by the producer library,
// which the handle keeps loaded (or the caller promised to).
unsafe impl Send for AbiBoxHandle {}
unsafe impl Sync for AbiBoxHandle {}

#[cfg(test)]
mod cdyn_handle_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // A minimal "foreign-style" plugin simulated in-process:
    // static instance + atomic ref-count + vtable of thunks — exactly the
    // pattern the C++ CdynExposed / Zig CdynPlugin generate.
    static INSTANCE: u64 = 0xdead_beef;
    static REFCOUNT: AtomicU32 = AtomicU32::new(0);

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct TestVtable {
        get_value: unsafe extern "C" fn(ctx: *mut std::ffi::c_void) -> u64,
    }

    unsafe extern "C" fn test_get_value(ctx: *mut std::ffi::c_void) -> u64 {
        // simulate thunk: ctx -> instance
        let _ = ctx;
        INSTANCE
    }

    unsafe extern "C" fn test_retain(_: AbiStableDynRef__FatPtr) {
        REFCOUNT.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C" fn test_release(_: AbiStableDynRef__FatPtr) {
        let prev = REFCOUNT.fetch_sub(1, Ordering::SeqCst);
        assert!(prev > 0, "release called more times than retain");
    }

    // alias so the fns match the RetainFn/ReleaseFn signatures
    type AbiStableDynRef__FatPtr = crate::native::AbiDynFatPtr;

    static TEST_VTABLE: TestVtable = TestVtable { get_value: test_get_value };

    #[test]
    fn cdyn_handle_retain_release_roundtrip() {
        REFCOUNT.store(1, Ordering::SeqCst); // plugin starts with 1 ref

        let raw = AbiStableDynRef {
            object: crate::native::AbiDynFatPtr {
                data: &INSTANCE as *const u64 as *const std::ffi::c_void,
                vtable: &TEST_VTABLE as *const TestVtable as *const std::ffi::c_void,
            },
            retain: test_retain,
            release: test_release,
        };

        // from_raw (unowned lib)
        let h1 = unsafe { AbiRef::<TestVtable>::from_raw(raw) };
        assert_eq!(REFCOUNT.load(Ordering::SeqCst), 1);

        // clone → retain
        let h2 = h1.clone();
        assert_eq!(REFCOUNT.load(Ordering::SeqCst), 2);

        // vtable call through the handle
        unsafe {
            let vt = h1.vtable();
            assert_eq!((vt.get_value)(h1.ctx()), INSTANCE);
        }
        // same vtable pointer via h2
        assert_eq!(h1.as_raw().object.vtable, h2.as_raw().object.vtable);

        // drop → release
        drop(h2);
        assert_eq!(REFCOUNT.load(Ordering::SeqCst), 1);
        drop(h1);
        assert_eq!(REFCOUNT.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cdyn_handle_into_raw_skips_release() {
        REFCOUNT.store(1, Ordering::SeqCst);
        let raw = AbiStableDynRef {
            object: crate::native::AbiDynFatPtr {
                data: &INSTANCE as *const u64 as *const std::ffi::c_void,
                vtable: &TEST_VTABLE as *const TestVtable as *const std::ffi::c_void,
            },
            retain: test_retain,
            release: test_release,
        };
        let h = unsafe { AbiRef::<TestVtable>::from_raw(raw) };
        let raw2 = h.into_raw(); // must NOT call release
        drop(raw2); // plain Copy struct, no Drop
        assert_eq!(REFCOUNT.load(Ordering::SeqCst), 1); // unchanged
        // give the ref back to the "plugin" to balance counts
        REFCOUNT.fetch_sub(1, Ordering::SeqCst);
    }

    // ---- AbiBox: single-owner data with producer-side free fn ----

    static BOX_FREED: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn counting_free(data: *mut std::ffi::c_void, len: usize) {
        BOX_FREED.fetch_add(1, Ordering::SeqCst);
        // still actually free (exact-fit boxed slice, same as from_vec)
        if !data.is_null() {
            let slice_ptr = std::slice::from_raw_parts_mut(data as *mut u8, len) as *mut [u8];
            drop(unsafe { Box::from_raw(slice_ptr) });
        }
    }

    #[test]
    fn cdyn_box_drop_calls_producer_free() {
        BOX_FREED.store(0, Ordering::SeqCst);
        let payload: Box<[u8]> = vec![1u8, 2, 3, 4].into_boxed_slice();
        let len = payload.len();
        let data = Box::into_raw(payload) as *mut std::ffi::c_void;
        let box_ = AbiBox {
            data,
            len,
            free: counting_free,
        };

        let handle = unsafe { AbiBoxHandle::from_box_unowned(box_) };
        assert_eq!(handle.as_slice(), &[1, 2, 3, 4]);
        assert_eq!(handle.len(), 4);
        assert_eq!(BOX_FREED.load(Ordering::SeqCst), 0);

        drop(handle); // → producer's free
        assert_eq!(BOX_FREED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cdyn_box_from_vec_roundtrip_and_free() {
        // Plugin side: from_vec pairs with abi_box_free_rust (same module).
        let box_ = AbiBox::from_vec(b"hello cross-module".to_vec());
        assert_eq!(box_.len, 18);
        let handle = unsafe { AbiBoxHandle::from_box_unowned(box_) };
        assert_eq!(&*handle, b"hello cross-module");
        drop(handle); // abi_box_free_rust runs — no leak, no allocator mismatch

        // into_raw transfers ownership: free must NOT run on drop
        BOX_FREED.store(0, Ordering::SeqCst);
        let box_ = AbiBox {
            data: Box::into_raw(vec![9u8; 4].into_boxed_slice()) as *mut std::ffi::c_void,
            len: 4,
            free: counting_free,
        };
        let handle = unsafe { AbiBoxHandle::from_box_unowned(box_) };
        let raw = handle.into_raw();
        assert_eq!(BOX_FREED.load(Ordering::SeqCst), 0);
        // caller now frees manually
        unsafe { (raw.free)(raw.data, raw.len) };
        assert_eq!(BOX_FREED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cdyn_box_null_is_safe() {
        let handle = unsafe { AbiBoxHandle::from_box_unowned(AbiBox::null()) };
        assert!(handle.is_empty());
        assert!(handle.as_slice().is_empty());
        drop(handle); // free is noop, no panic
    }
}

// ---------------------------------------------------------------------------
// AbiRef — cross-language ref-counted smart handle
// ---------------------------------------------------------------------------

/// Ref-counted handle to a **foreign** plugin object exposed through a
/// `*_get_dyn` style entry point returning an [`AbiStableDynRef`].
///
/// This is the Rust-side counterpart of the C++ SDK's `CdynExposed` /
/// `CdynPlugin` and the Zig SDK's `CdynPlugin(PluginType)`:
///
/// - [`clone`](Clone::clone) → calls the plugin's `retain`
/// - [`drop`](Drop) → calls the plugin's `release`
/// - [`ctx`](Self::ctx) → the instance pointer (first arg of every vtable method)
/// - [`vtable`](Self::vtable) → `&T` reconstructed from the packed vtable pointer
///
/// `T` is the C-layout vtable struct (`#[repr(C)]`, `Copy`) — e.g.
/// [`MathModuleVtable`] or your own. The vtable is **not** copied; it is
/// referenced through the pointer packed inside the dyn ref (it points into
/// the plugin's static storage, valid while the library stays loaded).
///
/// # Example
///
/// ```ignore
/// // C++ plugin built with: CDYN_EXPOSE_CLASS(MathPlugin, MathModuleVtable, math)
/// // exports: math_get_vtable() and math_get_dyn()
/// let handle = unsafe { AbiRef::<MathModuleVtable>::load(&path, b"math_get_dyn\0")? };
/// let vt = unsafe { handle.vtable() };
/// let session = unsafe { (vt.create_session)() };
/// let sum = unsafe { (vt.add_i32)(session, 1, 2) };
/// unsafe { (vt.destroy_session)(session) };
/// // handle drop → plugin release()
/// ```
pub struct AbiRef<T: Copy> {
    _lib: DynLib,
    raw: AbiStableDynRef,
    _marker: PhantomData<T>,
}

impl<T: Copy> AbiRef<T> {
    /// Load a foreign plugin object via its dyn entry point.
    ///
    /// # Safety
    ///
    /// - The target file must be a valid dynamic library.
    /// - The entry must return a valid `AbiStableDynRef` whose vtable pointer
    ///   refers to a `T`-layout vtable.
    pub unsafe fn load(path: &Path, dyn_entry: &[u8]) -> Result<Self> {
        let lib = unsafe { DynLib::load(path) }?;
        unsafe { Self::from_lib(lib, dyn_entry) }
    }

    /// Load from an already-loaded library.
    ///
    /// # Safety
    ///
    /// The entry must return a valid `AbiStableDynRef` whose vtable pointer
    /// refers to a `T`-layout vtable.
    pub unsafe fn from_lib(lib: DynLib, dyn_entry: &[u8]) -> Result<Self> {
        let entry: _ModuleDynEntry =
            unsafe { lib.symbol(dyn_entry) }.with_context(|| {
                format!("dyn entry '{}' not found", crate::helpers::display_symbol(dyn_entry))
            })?;
        let raw = unsafe { entry() };
        if raw.is_null() {
            anyhow::bail!("dyn entry returned null AbiStableDynRef");
        }
        Ok(Self {
            _lib: lib,
            raw,
            _marker: PhantomData,
        })
    }

    /// Reconstruct from a raw [`AbiStableDynRef`] obtained elsewhere.
    ///
    /// The handle does not own the originating library; the caller must keep
    /// it loaded (e.g. hold another [`DynPlugin`](crate::DynPlugin) or
    /// [`DynLib`](crate::DynLib)) for as long as this handle lives.
    ///
    /// # Safety
    ///
    /// `raw` must be a live ref produced by a compatible plugin.
    pub unsafe fn from_raw(raw: AbiStableDynRef) -> Self {
        Self {
            _lib: DynLib::unowned(),
            raw,
            _marker: PhantomData,
        }
    }

    /// Instance context pointer — pass as the first argument of vtable methods.
    pub fn ctx(&self) -> *mut std::ffi::c_void {
        self.raw.object.data as *mut std::ffi::c_void
    }

    /// The vtable, reconstructed from the pointer packed inside the dyn ref.
    ///
    /// # Safety
    ///
    /// `T` must be the exact vtable type the plugin used when building the ref.
    pub unsafe fn vtable(&self) -> &T {
        unsafe { &*(self.raw.object.vtable as *const T) }
    }

    /// Raw ABI ref (for passing back across the boundary).
    pub fn as_raw(&self) -> &AbiStableDynRef {
        &self.raw
    }

    /// Consume without calling `release` (ownership handed back to the plugin).
    pub fn into_raw(self) -> AbiStableDynRef {
        let mut this = std::mem::ManuallyDrop::new(self);
        // Detach the library handle so Drop won't run for it either.
        let lib = unsafe { std::ptr::read(&this._lib) };
        std::mem::forget(lib);
        this.raw
    }
}

/// Type of the dyn entry point that foreign plugins export
/// (C++ `CDYN_EXPORT AbiStableDynRef name_get_dyn()`, Zig `declareDynEntry`).
pub use crate::native::ModuleDynEntryPoint;

// SAFETY: AbiRef manages the plugin's own ref-count via retain/release;
// thread-safety follows the plugin's guarantees (same policy as SafeArcDyn).
unsafe impl<T: Copy + Send> Send for AbiRef<T> {}
unsafe impl<T: Copy + Sync> Sync for AbiRef<T> {}

impl<T: Copy> Clone for AbiRef<T> {
    fn clone(&self) -> Self {
        unsafe { (self.raw.retain)(self.raw.object) };
        Self {
            _lib: self._lib.clone(),
            raw: self.raw,
            _marker: PhantomData,
        }
    }
}

impl<T: Copy> Drop for AbiRef<T> {
    fn drop(&mut self) {
        unsafe { (self.raw.release)(self.raw.object) };
    }
}

#[cfg(test)]
mod cpp_math_tests {
    use super::*;
    use std::ffi::CStr;
    use std::path::PathBuf;

    fn math_module_path() -> PathBuf {
        // Relative from crate root (rust/crates/dyn-loader) to cpp build output
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("../../cpp/build/math_module");
        p.push("libmath_module.so");
        p
    }

    #[test]
    fn load_cpp_math_module_via_vtable() {
        let path = math_module_path();
        if !path.exists() {
            eprintln!("SKIP: {} not found — build cpp/ first", path.display());
            return;
        }

        let plugin =
            unsafe { AbiTable::<MathModuleVtable>::load(&path, b"math_module_get_vtable\0") }
                .expect("failed to load math_module");
        let vt = plugin.vtable();

        // Module info
        unsafe {
            let name = CStr::from_ptr((vt.module_name)());
            assert_eq!(name.to_str().unwrap(), "core-ast-math");
            assert_eq!((vt.module_version)(), 1);
        }

        // Create session
        let session = unsafe { (vt.create_session)() };
        assert!(!session.is_null());

        // No functions generated yet
        assert_eq!(unsafe { (vt.generated_count)(session) }, 0);

        // Call add_i32 — marks it as "used"
        let result = unsafe { (vt.add_i32)(session, 10, 20) };
        assert_eq!(result, 30);
        assert_eq!(unsafe { (vt.generated_count)(session) }, 1);

        // Call mul_f64 — marks it as "used"
        let result = unsafe { (vt.mul_f64)(session, 3.0, 7.0) };
        assert!((result - 21.0).abs() < 1e-10);
        assert_eq!(unsafe { (vt.generated_count)(session) }, 2);

        // Call pi
        let pi_val = unsafe { (vt.pi)(session) };
        assert!((pi_val - std::f64::consts::PI).abs() < 1e-10);
        assert_eq!(unsafe { (vt.generated_count)(session) }, 3);

        // Introspect generated functions
        let func0 = unsafe { (vt.generated_at)(session, 0) };
        let func0_name = unsafe { CStr::from_ptr(func0.name) }.to_str().unwrap();
        assert_eq!(func0_name, "add_i32");

        let func1 = unsafe { (vt.generated_at)(session, 1) };
        let func1_name = unsafe { CStr::from_ptr(func1.name) }.to_str().unwrap();
        assert_eq!(func1_name, "mul_f64");

        let func2 = unsafe { (vt.generated_at)(session, 2) };
        let func2_name = unsafe { CStr::from_ptr(func2.name) }.to_str().unwrap();
        assert_eq!(func2_name, "pi");

        // Call add_i32 again — should NOT add duplicate
        let _ = unsafe { (vt.add_i32)(session, 1, 2) };
        assert_eq!(unsafe { (vt.generated_count)(session) }, 3);

        // Destroy session
        unsafe { (vt.destroy_session)(session) };
    }
}
