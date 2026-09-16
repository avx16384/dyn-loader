//! The two layout facts that make `#[repr(C)]` mandatory.
//!
//! These are not assertions of intent — they are the measurements behind the
//! first law's requirement that every struct crossing the boundary carries
//! `#[repr(C)]`. If either ever stops holding, this file fails and the rule
//! can be revisited with evidence instead of memory.

use std::mem::{align_of, offset_of, size_of};

/// A declaration deliberately ordered to expose reordering: a wide field
/// between two narrow ones, so a layout-minimizing pass has something to do.
struct RustLayout {
    a: u8,
    b: u64,
    c: u8,
}

#[repr(C)]
struct CLayout {
    a: u8,
    b: u64,
    c: u8,
}

/// The default representation does **not** keep declaration order.
///
/// This is the first reason a `repr(Rust)` struct cannot cross the boundary:
/// the offsets a producer compiled against are not the offsets a consumer
/// will read, and the compiler is entitled to change its mind in any release.
#[test]
fn the_default_representation_reorders_fields() {
    // Declaration order would be 0, 8, 16. The real layout is not that.
    let a = offset_of!(RustLayout, a);
    let b = offset_of!(RustLayout, b);
    let c = offset_of!(RustLayout, c);

    assert!(
        !(a < b && b < c),
        "repr(Rust) kept declaration order ({a}, {b}, {c}); the reordering this \
         rule is based on no longer happens — re-measure before relaxing anything"
    );

    // And it is not merely reordered: the size differs from the C layout,
    // so the two representations are not interchangeable even in extent.
    assert_ne!(size_of::<RustLayout>(), size_of::<CLayout>());
}

/// `repr(C)` keeps declaration order, which is what makes it a contract.
#[test]
fn the_c_representation_keeps_declaration_order() {
    assert_eq!(offset_of!(CLayout, a), 0);
    assert!(
        offset_of!(CLayout, a) < offset_of!(CLayout, b)
            && offset_of!(CLayout, b) < offset_of!(CLayout, c),
        "repr(C) must lay fields out in declaration order"
    );
    assert_eq!(align_of::<CLayout>(), align_of::<u64>());
}

/// With no larger alignment to force it, a homogeneous struct has no padding
/// and its bytes are fully defined.
#[repr(C)]
struct Packed {
    a: u64,
    b: u64,
}

/// Padding bytes hold whatever was there before — they are not zeroed.
///
/// This is the second reason: even a correctly declared `repr(C)` struct
/// cannot be compared or hashed as raw bytes, because two instances with
/// identical fields can have different padding.
#[test]
fn padding_bytes_carry_previous_memory_contents() {
    #[repr(C)]
    struct Padded {
        a: u8,
        b: u64,
    }

    const SIZE: usize = size_of::<Padded>();
    // Bytes 1..8 are padding.
    let padding_start = offset_of!(Padded, a) + size_of::<u8>();
    let padding_end = offset_of!(Padded, b);
    assert!(
        padding_start < padding_end,
        "this struct is expected to have padding; without it the test proves nothing"
    );

    /// A buffer aligned like the type it will hold.
    ///
    /// Plain `[u8; N]` is only byte-aligned, and forming a reference to a
    /// misaligned value is undefined behaviour — which is the same class of
    /// hazard this whole file is about, so it would be poor form to trip over
    /// it here.
    #[repr(C, align(8))]
    struct Aligned([u8; SIZE]);

    let mut first = Aligned([0xAAu8; SIZE]);
    let mut second = Aligned([0xBBu8; SIZE]);
    unsafe {
        std::ptr::write(first.0.as_mut_ptr() as *mut Padded, Padded { a: 1, b: 2 });
        std::ptr::write(second.0.as_mut_ptr() as *mut Padded, Padded { a: 1, b: 2 });
    }

    let left = first.0;
    let right = second.0;

    // The fields agree.
    let left_value = unsafe { &*(first.0.as_ptr() as *const Padded) };
    let right_value = unsafe { &*(second.0.as_ptr() as *const Padded) };
    assert_eq!(
        (left_value.a, left_value.b),
        (right_value.a, right_value.b),
        "the fields are meant to be equal here"
    );

    // The bytes do not — which is why raw byte comparison is not an option.
    assert_ne!(
        left, right,
        "padding came out equal, so this compiler zeroes it; the warning against \
         byte-wise comparison would be less urgent, but still not wrong"
    );
    assert_eq!(&left[padding_start..padding_end], &[0xAA; 7]);
    assert_eq!(&right[padding_start..padding_end], &[0xBB; 7]);
}

/// A struct whose fields tile its size has no padding, and its bytes are
/// fully determined by its fields.
///
/// This is the shape a payload should take when a byte-level digest is
/// genuinely needed.
#[test]
fn a_struct_without_padding_is_fully_defined() {
    assert_eq!(size_of::<Packed>(), 16);
    assert_eq!(offset_of!(Packed, a), 0);
    assert_eq!(offset_of!(Packed, b), 8);
    // No gap anywhere, so there is nothing uninitialized to leave behind.
    assert_eq!(size_of::<Packed>(), 2 * size_of::<u64>());
}
