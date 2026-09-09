//! Shared `DynLib` wrapper and helpers used by both [`crate::native`]
//! (Rust fat-pointer bridge) and [`crate::abi`] (interface-table loading).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use libloading::Library;

/// Type of the entry point function that modules must export.
pub type ModuleEntryPointRaw = unsafe extern "C" fn() -> std::ffi::c_void;

// ---------------------------------------------------------------------------
// DynLib — Arc-shared dynamic library
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct DynLib {
    library: Arc<Library>,
    path: std::path::PathBuf,
}

impl DynLib {
    /// Load a dynamic library from the given path.
    ///
    /// # Safety
    ///
    /// The target file must be a valid dynamic library for the current process.
    pub unsafe fn load(path: &Path) -> Result<Self> {
        let library = unsafe { Library::new(path) }
            .with_context(|| format!("failed to load dynamic library: {}", path.display()))?;
        Ok(Self {
            library: Arc::new(library),
            path: path.to_path_buf(),
        })
    }

    /// Wrap an already-loaded `libloading::Library`.
    ///
    /// Useful when the library was loaded elsewhere (e.g. by the OS or another
    /// loader) and only reference-counting ownership is needed here.
    pub fn from_library(library: Library, path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            library: Arc::new(library),
            path: path.into(),
        }
    }

    /// A handle that does NOT own the library (no unload on drop).
    ///
    /// For refs obtained from modules whose library lifetime is managed
    /// externally (e.g. statically linked, or kept alive by another handle).
    /// Uses `dlopen(NULL)` — a handle to the current process, which never
    /// fails and whose "unload" is a no-op.
    pub fn unowned() -> Self {
        let library = unsafe { Library::new(std::ffi::OsStr::new("")) }
            .expect("dlopen(NULL) — handle to the current process — cannot fail");
        Self {
            library: Arc::new(library),
            path: std::path::PathBuf::from("<unowned>"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load a symbol from the library.
    ///
    /// # Safety
    ///
    /// The caller must ensure `T` matches the actual exported symbol type.
    pub unsafe fn symbol<T: Copy>(&self, name: &[u8]) -> Result<T> {
        let sym = unsafe { self.library.get::<T>(name) }.with_context(|| {
            format!(
                "symbol '{}' not found in {}",
                display_symbol(name),
                self.path.display()
            )
        })?;
        Ok(*sym)
    }

    /// Try to load a symbol; returns `None` if not found.
    ///
    /// # Safety
    ///
    /// The caller must ensure `T` matches the actual exported symbol type.
    pub unsafe fn try_symbol<T: Copy>(&self, name: &[u8]) -> Option<T> {
        unsafe { self.library.get::<T>(name) }.ok().map(|s| *s)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn display_symbol(symbol: &[u8]) -> String {
    let end = symbol.iter().position(|&b| b == 0).unwrap_or(symbol.len());
    String::from_utf8_lossy(&symbol[..end]).into_owned()
}

/// Quick check: does the file expose the given entry symbol?
///
/// # Safety
///
/// Probes an arbitrary dynamic library for a specific exported symbol.
pub unsafe fn looks_like_module(path: &Path, entry_symbol: &[u8]) -> bool {
    match unsafe { DynLib::load(path) } {
        Ok(lib) => unsafe { lib.try_symbol::<ModuleEntryPointRaw>(entry_symbol) }.is_some(),
        Err(_) => false,
    }
}
