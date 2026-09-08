//! # dyn — Rust fat-pointer bridge for trait objects
//!
//! Load trait objects from `.so`/`.dylib` plugins using Rust's native
//! fat-pointer representation (data pointer + vtable pointer), wrapped with
//! Arc-like retain/release for cross-boundary memory safety.
//!
//! This module follows the two cross-module invariants shared by all modes
//! of this crate (see [`crate::cdyn`] for the full statement):
//!
//! 1. **Whoever allocates, deallocates** — the plugin's `retain`/`release`
//!    function pointers execute inside the plugin module (for Rust plugins
//!    they wrap `Arc` ref-count ops on the plugin's own heap allocation).
//! 2. **Whoever creates, operates** — the host receives the fat pointer and
//!    calls through it; every method dispatch goes through the vtable the
//!    plugin created, executing plugin-side code.
//!
//! - **AbiDynFatPtr**: ABI-stable representation of a Rust fat pointer,
//!   `#[repr(C)]` for C ABI compatibility.
//! - **AbiStableDynRef**: fat pointer + retain/release function pointers.
//! - **SafeArcDyn<T>**: safe, cloneable handle over an `AbiStableDynRef`.
//! - **NativeModule<T>**: loaded plugin dereferencing to `&T`.
//!
//! ## Usage
//!
//! ```ignore
//! // In the plugin .so:
//! #[no_mangle]
//! pub extern "C" fn core_ast_transform_entry() -> AbiStableDynRef {
//!     SafeArcDyn::from_arc(Arc::new(MyTransform) as Arc<dyn Transform>).into_abi()
//! }
//!
//! // In the host:
//! use dyn_loader::dyn_mod::{NativeModule, AbiStableDynRef, SafeArcDyn};
//! let plugin = NativeModule::<dyn Transform>::load("libmy_transform.so", b"core_ast_transform_entry\0")?;
//! let transform: &dyn Transform = plugin.trait_ref();
//! ```

use std::ffi::c_void;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::DynLib;
use crate::helpers::display_symbol;

// ---------------------------------------------------------------------------
// AbiDynFatPtr — ABI-stable fat pointer
// ---------------------------------------------------------------------------

/// ABI-stable representation of a Rust dyn trait fat pointer.
/// Two words: data pointer + vtable pointer.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbiDynFatPtr {
    pub data: *const c_void,
    pub vtable: *const c_void,
}

impl AbiDynFatPtr {
    pub const fn null() -> Self {
        Self {
            data: std::ptr::null(),
            vtable: std::ptr::null(),
        }
    }

    pub fn is_null(self) -> bool {
        self.data.is_null() || self.vtable.is_null()
    }
}

// SAFETY: AbiDynFatPtr is just two raw pointers; the actual thread-safety
// is governed by the enclosing SafeArcDyn<T> where T: Send + Sync.
unsafe impl Send for AbiDynFatPtr {}
unsafe impl Sync for AbiDynFatPtr {}

// ---------------------------------------------------------------------------
// AbiStableDynRef — fat pointer + retain/release for Arc-like semantics
// ---------------------------------------------------------------------------

pub type RetainFn = unsafe extern "C" fn(AbiDynFatPtr);
pub type ReleaseFn = unsafe extern "C" fn(AbiDynFatPtr);

/// ABI-stable reference to a dyn trait object, with retain/release for
/// safe reference counting across dynamic library boundaries.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AbiStableDynRef {
    pub object: AbiDynFatPtr,
    pub retain: RetainFn,
    pub release: ReleaseFn,
}

impl AbiStableDynRef {
    pub const fn null() -> Self {
        Self {
            object: AbiDynFatPtr::null(),
            retain: retain_noop,
            release: release_noop,
        }
    }

    pub fn is_null(self) -> bool {
        self.object.is_null()
    }
}

// SAFETY: The retain/release functions manage the Arc ref-count;
// thread-safety follows T: Send + Sync.
unsafe impl Send for AbiStableDynRef {}
unsafe impl Sync for AbiStableDynRef {}

unsafe extern "C" fn retain_noop(_: AbiDynFatPtr) {}
unsafe extern "C" fn release_noop(_: AbiDynFatPtr) {}

// ---------------------------------------------------------------------------
// Pack / unpack fat pointers
// ---------------------------------------------------------------------------

/// Pack a `*const T` (where T: ?Sized) into an ABI-stable fat pointer.
///
/// # Safety
///
/// `ptr` must be a valid fat pointer (e.g., `*const dyn Trait`).
pub unsafe fn pack_fat_ptr<T: ?Sized>(ptr: *const T) -> AbiDynFatPtr {
    // Fat pointers are exactly 2 words: data + metadata (vtable for dyn Trait)
    unsafe { std::mem::transmute_copy(&ptr) }
}

/// Unpack an ABI-stable fat pointer back to `*const T`.
///
/// # Safety
///
/// `ptr` must have been created from a compatible `*const T`.
pub unsafe fn unpack_fat_ptr<T: ?Sized>(ptr: AbiDynFatPtr) -> *const T {
    unsafe { std::mem::transmute_copy(&ptr) }
}

// ---------------------------------------------------------------------------
// Arc retain/release for dyn trait objects
// ---------------------------------------------------------------------------

unsafe extern "C" fn retain_arc<T: ?Sized>(ptr: AbiDynFatPtr) {
    let raw: *const T = unsafe { unpack_fat_ptr(ptr) };
    unsafe { Arc::increment_strong_count(raw) };
}

unsafe extern "C" fn release_arc<T: ?Sized>(ptr: AbiDynFatPtr) {
    let raw: *const T = unsafe { unpack_fat_ptr(ptr) };
    unsafe { drop(Arc::from_raw(raw)) };
}

// ---------------------------------------------------------------------------
// SafeArcDyn — safe wrapper around AbiStableDynRef
// ---------------------------------------------------------------------------

/// A safe, cloneable, droppable reference to a dyn trait object loaded
/// from a dynamic library. Uses Arc-like retain/release for memory safety.
#[repr(transparent)]
pub struct SafeArcDyn<T: ?Sized> {
    raw: AbiStableDynRef,
    _marker: PhantomData<*const T>,
}

impl<T: ?Sized> SafeArcDyn<T> {
    /// Create from an `Arc<T>`. The Arc's reference count is managed
    /// via the retain/release function pointers.
    pub fn from_arc(value: Arc<T>) -> Self {
        let raw = Arc::into_raw(value);
        Self {
            raw: AbiStableDynRef {
                object: unsafe { pack_fat_ptr(raw) },
                retain: retain_arc::<T>,
                release: release_arc::<T>,
            },
            _marker: PhantomData,
        }
    }

    /// Get the ABI-stable representation (for exporting from a plugin).
    pub fn into_abi(self) -> AbiStableDynRef {
        let raw = self.raw;
        std::mem::forget(self); // Don't drop — caller takes ownership
        raw
    }

    /// Reconstruct from an ABI-stable representation (for loading in host).
    ///
    /// # Safety
    ///
    /// `raw` must have been created by `SafeArcDyn::<T>` or compatible code.
    pub unsafe fn from_abi(raw: AbiStableDynRef) -> Self {
        Self {
            raw,
            _marker: PhantomData,
        }
    }

    /// Get a reference to the trait object.
    ///
    /// # Safety
    ///
    /// The stored fat pointer must be valid for the lifetime of this reference.
    pub unsafe fn trait_ref(&self) -> &T {
        unsafe { &*unpack_fat_ptr::<T>(self.raw.object) }
    }
}

// SAFETY: SafeArcDyn<T> is Arc-like; safe to Send/Sync when T is.
unsafe impl<T: ?Sized + Send> Send for SafeArcDyn<T> {}
unsafe impl<T: ?Sized + Sync> Sync for SafeArcDyn<T> {}

impl<T: ?Sized> Clone for SafeArcDyn<T> {
    fn clone(&self) -> Self {
        unsafe { (self.raw.retain)(self.raw.object) };
        Self {
            raw: self.raw,
            _marker: PhantomData,
        }
    }
}

impl<T: ?Sized> Drop for SafeArcDyn<T> {
    fn drop(&mut self) {
        unsafe { (self.raw.release)(self.raw.object) };
    }
}

// ---------------------------------------------------------------------------
// NativeModule — loaded plugin with trait object access
// ---------------------------------------------------------------------------

/// A loaded dynamic library plugin that exposes a trait object via
/// the dyn-fat-pointer-bridge pattern.
pub struct NativeModule<T: ?Sized> {
    _lib: DynLib,
    plugin: SafeArcDyn<T>,
}

/// Type of the entry point function that plugins must export.
pub type ModuleDynEntryPoint = unsafe extern "C" fn() -> AbiStableDynRef;

impl<T: ?Sized> NativeModule<T> {
    /// Load a plugin from a dynamic library file.
    ///
    /// The library must export a function with the given symbol name
    /// that returns an `AbiStableDynRef` created via `SafeArcDyn::into_abi()`.
    ///
    /// # Safety
    ///
    /// - The target file must be a valid dynamic library for the current process.
    /// - The entry point must return a valid `AbiStableDynRef` for trait `T`.
    pub unsafe fn load(path: &Path, entry_symbol: &[u8]) -> Result<Self> {
        let lib = unsafe { DynLib::load(path) }?;
        unsafe { Self::from_lib(lib, entry_symbol) }
    }
    /// Load from an already-loaded `DynLib`.
    ///
    /// # Safety
    ///
    /// The entry point must return a valid `AbiStableDynRef` for trait `T`.
    pub unsafe fn from_lib(lib: DynLib, entry_symbol: &[u8]) -> Result<Self> {
        let entry: ModuleDynEntryPoint = unsafe { lib.symbol(entry_symbol) }
            .with_context(|| format!("entry point '{}' not found", display_symbol(entry_symbol)))?;
        let abi_ref = unsafe { entry() };
        if abi_ref.is_null() {
            anyhow::bail!("entry point returned null AbiStableDynRef");
        }
        let plugin = unsafe { SafeArcDyn::<T>::from_abi(abi_ref) };
        Ok(Self { _lib: lib, plugin })
    }

    /// Get a reference to the loaded trait object.
    pub fn trait_ref(&self) -> &T {
        unsafe { self.plugin.trait_ref() }
    }

    /// Get a cloned SafeArcDyn (for sharing across threads).
    pub fn clone_handle(&self) -> SafeArcDyn<T> {
        self.plugin.clone()
    }
}

// SAFETY: NativeModule<T> owns a DynLib (Arc<Library>) + SafeArcDyn<T>;
// safe to Send/Sync when T is.
unsafe impl<T: ?Sized + Send> Send for NativeModule<T> {}
unsafe impl<T: ?Sized + Sync> Sync for NativeModule<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    trait Demo: Send + Sync {
        fn value(&self) -> i32;
    }

    struct DemoValue(i32);

    impl Demo for DemoValue {
        fn value(&self) -> i32 {
            self.0
        }
    }

    #[test]
    fn safe_arc_dyn_round_trip() {
        let arc: Arc<dyn Demo> = Arc::new(DemoValue(42));
        let wrapped = SafeArcDyn::from_arc(arc);
        let abi = wrapped.into_abi();
        let restored = unsafe { SafeArcDyn::<dyn Demo>::from_abi(abi) };
        assert_eq!(unsafe { restored.trait_ref() }.value(), 42);
    }

    #[test]
    fn safe_arc_dyn_clone() {
        let arc: Arc<dyn Demo> = Arc::new(DemoValue(7));
        let wrapped = SafeArcDyn::from_arc(arc);
        let cloned = wrapped.clone();
        assert_eq!(unsafe { wrapped.trait_ref() }.value(), 7);
        assert_eq!(unsafe { cloned.trait_ref() }.value(), 7);
        drop(wrapped);
        assert_eq!(unsafe { cloned.trait_ref() }.value(), 7);
    }
}
