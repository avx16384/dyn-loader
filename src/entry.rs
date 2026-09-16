//! # entry — the one thing every head package has in common
//!
//! Head packages differ in almost everything. What interface they declare,
//! which entry symbol they expect, whether they open freely or check
//! something first — all of that is theirs to decide, and none of it belongs
//! in a shared signature.
//!
//! One thing they share is an identity. A host that carries a module needs to
//! be able to name it: in a log line, in an error, in a crash report, in a
//! list of what is loaded. That is all this trait is for.
//!
//! ```
//! use dyn_loader::HeadEntry;
//!
//! pub struct Math;
//!
//! impl HeadEntry for Math {
//!     const ID: &'static str = "com.example.math";
//! }
//! ```
//!
//! The identifier is a reverse-domain name, the same shape a package manager
//! or an application id uses. Nothing here parses it, validates it, or gives
//! it meaning — it is a name, and a name is what the host asked for.

/// The identity of a library, as declared by its head package.
///
/// Implemented once per module. Reverse-domain by convention:
/// `com.example.math`.
pub trait HeadEntry {
    /// The library's identity, e.g. `com.example.math`.
    const ID: &'static str;
}

#[cfg(test)]
mod tests {
    use super::HeadEntry;

    struct Math;
    struct Notes;

    impl HeadEntry for Math {
        const ID: &'static str = "com.example.math";
    }

    impl HeadEntry for Notes {
        const ID: &'static str = "org.example.notes";
    }

    #[test]
    fn a_head_reports_its_identity() {
        assert_eq!(Math::ID, "com.example.math");
        assert_eq!(Notes::ID, "org.example.notes");
    }

    /// Usable as a bound, which is the only thing a host needs it for.
    fn name_of<T: HeadEntry>() -> &'static str {
        T::ID
    }

    #[test]
    fn the_identity_is_reachable_through_a_bound() {
        assert_eq!(name_of::<Math>(), "com.example.math");
    }
}
