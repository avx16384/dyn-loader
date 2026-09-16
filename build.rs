//! Capture the toolchain that produces this build.
//!
//! The reconciliation needs one fact the compiler does not hand over directly:
//! which rustc version built this crate. Cargo exposes the *package's*
//! `rust-version` field, which is a statement of intent, not the toolchain
//! actually in use. The toolchain is what matters, so it is asked.
//!
//! The result is emitted as `DYN_HOST_RUSTC_VERSION` in two forms:
//!
//! - `MAJOR.MINOR.PATCH`, parsed out of `rustc --version`
//! - the target triple the build is for, from `TARGET`
//!
//! Both are absent, rather than wrong, when they cannot be determined: a host
//! that does not know its own producer facts must be able to say so.

use std::process::Command;

fn main() {
    // The version of the compiler running this build script is the version
    // that will produce the crate, because cargo uses one toolchain for both.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RUSTC");

    if let Some(version) = rustc_version() {
        println!("cargo:rustc-env=DYN_HOST_RUSTC_VERSION={version}");
    }

    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo:rustc-env=DYN_HOST_TARGET={target}");
    }
}

/// `rustc --version` prints `rustc 1.83.0 (90b35a623 2024-11-26)`.
///
/// A dated toolchain prints `rustc 1.85.0-nightly (…)`; the suffix is dropped
/// so the recorded fact is a plain version, matching what a package author
/// writes in a triple.
fn rustc_version() -> Option<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = Command::new(rustc).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut words = text.split_whitespace();
    if words.next()? != "rustc" {
        return None;
    }
    let version = words.next()?.split('-').next()?;
    (!version.is_empty()).then(|| version.to_string())
}
