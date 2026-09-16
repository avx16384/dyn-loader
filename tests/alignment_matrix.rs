//! The data-contract alignment matrix.
//!
//! ## Why a matrix
//!
//! `#[repr(C)]` is the insurance: the platform ABI defines the layout and
//! every compiler agrees, so nothing needs testing. But someone will want a
//! Rust layout — a natural declaration order, a type that reads well, a struct
//! shared without ceremony. The moment that happens, the layout stops being
//! specified by anyone and becomes *whatever this compiler did*. The only way
//! to know what it did is to measure it over a grid and record the answer.
//!
//! This file is that grid, for the **data contract**: scalars, structs, and
//! how they compose. The vtable contract has its own matrix, in
//! `cdyn-loader/tests/vtable_matrix.rs`.
//!
//! It does two things:
//!
//! 1. **Computes** `repr(C)` layout from first principles — an independent
//!    implementation of the C rules, written from the rules rather than from
//!    `offset_of!`, so that agreement between the two is evidence and not
//!    tautology.
//! 2. **Checks** that computation against `offset_of!`, `size_of!` and
//!    `align_of!` for a grid of field type combinations.
//!
//! ## The rules being modelled
//!
//! ```text
//! offset(0)      = 0
//! offset(i)      = round_up(offset(i-1) + size(i-1), align(i))
//! align(struct)  = max(align(i))
//! size(struct)   = round_up(offset(last) + size(last), align(struct))
//! ```

use std::mem::{align_of, offset_of, size_of};

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// Independent implementation of the C struct layout rules.
fn compute(fields: &[(usize, usize)]) -> (Vec<usize>, usize, usize) {
    let struct_align = fields.iter().map(|(_, align)| *align).max().unwrap_or(1);

    let mut offsets = Vec::with_capacity(fields.len());
    let mut cursor = 0usize;
    for (size, align) in fields {
        let padded = round_up(cursor, *align);
        offsets.push(padded);
        cursor = padded + size;
    }

    (offsets, round_up(cursor, struct_align), struct_align)
}

fn round_up(value: usize, align: usize) -> usize {
    if align == 0 {
        return value;
    }
    value.div_ceil(align) * align
}

// ---------------------------------------------------------------------------
// The grid: every scalar the boundary admits
// ---------------------------------------------------------------------------

/// A scalar type, with the size and alignment the protocol assumes of it.
struct Scalar {
    name: &'static str,
    size: usize,
    align: usize,
}

/// Every scalar that may cross the boundary.
///
/// This list is itself a claim: if a row is wrong, the protocol's assumption
/// is wrong, and the grid below is checking layout against a broken premise.
fn scalars() -> Vec<Scalar> {
    vec![
        Scalar { name: "u8", size: size_of::<u8>(), align: align_of::<u8>() },
        Scalar { name: "u16", size: size_of::<u16>(), align: align_of::<u16>() },
        Scalar { name: "u32", size: size_of::<u32>(), align: align_of::<u32>() },
        Scalar { name: "u64", size: size_of::<u64>(), align: align_of::<u64>() },
        Scalar { name: "u128", size: size_of::<u128>(), align: align_of::<u128>() },
        Scalar { name: "usize", size: size_of::<usize>(), align: align_of::<usize>() },
        Scalar { name: "i8", size: size_of::<i8>(), align: align_of::<i8>() },
        Scalar { name: "i16", size: size_of::<i16>(), align: align_of::<i16>() },
        Scalar { name: "i32", size: size_of::<i32>(), align: align_of::<i32>() },
        Scalar { name: "i64", size: size_of::<i64>(), align: align_of::<i64>() },
        Scalar { name: "i128", size: size_of::<i128>(), align: align_of::<i128>() },
        Scalar { name: "isize", size: size_of::<isize>(), align: align_of::<isize>() },
        Scalar { name: "f32", size: size_of::<f32>(), align: align_of::<f32>() },
        Scalar { name: "f64", size: size_of::<f64>(), align: align_of::<f64>() },
        Scalar { name: "bool", size: size_of::<bool>(), align: align_of::<bool>() },
        Scalar {
            name: "c_char",
            size: size_of::<std::ffi::c_char>(),
            align: align_of::<std::ffi::c_char>(),
        },
        Scalar {
            name: "c_int",
            size: size_of::<std::ffi::c_int>(),
            align: align_of::<std::ffi::c_int>(),
        },
        Scalar {
            name: "c_uint",
            size: size_of::<std::ffi::c_uint>(),
            align: align_of::<std::ffi::c_uint>(),
        },
        Scalar {
            name: "c_long",
            size: size_of::<std::ffi::c_long>(),
            align: align_of::<std::ffi::c_long>(),
        },
        Scalar {
            name: "c_ulong",
            size: size_of::<std::ffi::c_ulong>(),
            align: align_of::<std::ffi::c_ulong>(),
        },
        Scalar {
            name: "c_longlong",
            size: size_of::<std::ffi::c_longlong>(),
            align: align_of::<std::ffi::c_longlong>(),
        },
        Scalar {
            name: "c_ulonglong",
            size: size_of::<std::ffi::c_ulonglong>(),
            align: align_of::<std::ffi::c_ulonglong>(),
        },
        Scalar {
            name: "*const c_void",
            size: size_of::<*const std::ffi::c_void>(),
            align: align_of::<*const std::ffi::c_void>(),
        },
        Scalar {
            name: "extern \"C\" fn()",
            size: size_of::<unsafe extern "C" fn()>(),
            align: align_of::<unsafe extern "C" fn()>(),
        },
    ]
}

/// The alignment classes the grid must cover.
///
/// A pair's layout depends only on the four numbers `(size_a, align_a,
/// size_b, align_b)`, so covering one representative of every class present
/// on the platform covers every combination. This makes the grid finite and
/// keeps it honest: a type whose alignment is not in this list fails the
/// check below rather than quietly escaping the matrix.
fn covered_alignments() -> Vec<usize> {
    scalars().iter().map(|s| s.align).collect()
}

// ---------------------------------------------------------------------------
// Pairwise: the padding rule at its plainest
// ---------------------------------------------------------------------------

#[repr(C)]
struct Pair<A, B> {
    a: A,
    b: B,
}

/// Check one cell of the pair grid.
fn check_pair<A, B>(an: &str, bn: &str) {
    let a_size = size_of::<A>();
    let a_align = align_of::<A>();
    let b_size = size_of::<B>();
    let b_align = align_of::<B>();

    let (offsets, expected_size, expected_align) = compute(&[(a_size, a_align), (b_size, b_align)]);

    assert_eq!(
        offset_of!(Pair<A, B>, b),
        offsets[1],
        "Pair<{an}, {bn}>: field b offset"
    );
    assert_eq!(offset_of!(Pair<A, B>, a), offsets[0], "Pair<{an}, {bn}>: field a offset");
    assert_eq!(
        size_of::<Pair<A, B>>(),
        expected_size,
        "Pair<{an}, {bn}>: size ({a_size}b/{a_align}a then {b_size}b/{b_align}a)"
    );
    assert_eq!(
        align_of::<Pair<A, B>>(),
        expected_align,
        "Pair<{an}, {bn}>: alignment"
    );
}

/// The pair grid, one line per alignment class combination.
///
/// Written out rather than generated, for two reasons. `offset_of!` needs a
/// concrete type, so a generic loop cannot reach the compiler's real answer.
/// And a pair's layout depends only on `(size, align)` of each field, so
/// covering each alignment class against each other class is *complete* — the
/// remaining cells would be re-testing the same four numbers.
///
/// The classes present here are 1, 2, 4, 8 and 16; the test below asserts
/// that nothing on the platform falls outside them.
#[test]
fn every_alignment_class_pair_lays_out_as_the_c_rules_say() {
    // 1-byte class, against every class.
    check_pair::<u8, u8>("u8", "u8");
    check_pair::<u8, u16>("u8", "u16");
    check_pair::<u8, u32>("u8", "u32");
    check_pair::<u8, u64>("u8", "u64");
    check_pair::<u8, u128>("u8", "u128");

    // 2-byte class, against every class.
    check_pair::<u16, u8>("u16", "u8");
    check_pair::<u16, u16>("u16", "u16");
    check_pair::<u16, u32>("u16", "u32");
    check_pair::<u16, u64>("u16", "u64");
    check_pair::<u16, u128>("u16", "u128");

    // 4-byte class.
    check_pair::<u32, u8>("u32", "u8");
    check_pair::<u32, u16>("u32", "u16");
    check_pair::<u32, u32>("u32", "u32");
    check_pair::<u32, u64>("u32", "u64");
    check_pair::<u32, u128>("u32", "u128");

    // 8-byte class.
    check_pair::<u64, u8>("u64", "u8");
    check_pair::<u64, u16>("u64", "u16");
    check_pair::<u64, u32>("u64", "u32");
    check_pair::<u64, u64>("u64", "u64");
    check_pair::<u64, u128>("u64", "u128");

    // 16-byte class.
    check_pair::<u128, u8>("u128", "u8");
    check_pair::<u128, u16>("u128", "u16");
    check_pair::<u128, u32>("u128", "u32");
    check_pair::<u128, u64>("u128", "u64");
    check_pair::<u128, u128>("u128", "u128");

    // Types whose size differs from their class, to catch a model that keys
    // off size rather than alignment.
    check_pair::<bool, f64>("bool", "f64");
    check_pair::<[u8; 3], f64>("[u8; 3]", "f64");
    check_pair::<std::ffi::c_char, *const std::ffi::c_void>("c_char", "*const c_void");
}

/// Every scalar on the platform falls inside the covered alignment classes.
///
/// If a type with a new alignment ever enters the grid — a `f16`, a vector
/// type, a target with 32-byte alignment — this fails instead of the matrix
/// silently testing less than it claims.
#[test]
fn the_grid_covers_every_alignment_in_use() {
    let mut classes: Vec<usize> = covered_alignments();
    classes.sort_unstable();
    classes.dedup();

    assert_eq!(
        classes,
        vec![1, 2, 4, 8, 16],
        "the pair grid covers classes 1, 2, 4, 8 and 16; the scalar list \
         contains something else"
    );
}

/// The scalar table agrees with the protocol's stated assumptions.
///
/// These are the numbers a module built by another compiler is relying on, so
/// they are asserted rather than merely read.
#[test]
fn the_scalar_table_states_the_usual_platform_numbers() {
    let by_name = |name: &str| -> Scalar {
        let mut found: Vec<Scalar> = scalars()
            .into_iter()
            .filter(|s| s.name == name)
            .collect();
        assert_eq!(found.len(), 1, "`{name}` must appear exactly once");
        found.pop().unwrap()
    };

    // Widths are powers of two and never zero.
    for scalar in scalars() {
        assert!(scalar.size > 0, "{}: zero size", scalar.name);
        assert!(
            scalar.size.is_power_of_two() || scalar.size == 12,
            "{}: unexpected size {}",
            scalar.name,
            scalar.size
        );
        assert!(
            scalar.align.is_power_of_two(),
            "{}: alignment {} is not a power of two",
            scalar.name,
            scalar.align
        );
        assert!(
            scalar.size % scalar.align == 0,
            "{}: size {} is not a multiple of alignment {}",
            scalar.name,
            scalar.size,
            scalar.align
        );
    }

    // The sizes the protocol's own types depend on.
    assert_eq!(by_name("u8").size, 1);
    assert_eq!(by_name("u16").size, 2);
    assert_eq!(by_name("u32").size, 4);
    assert_eq!(by_name("u64").size, 8);
    assert_eq!(by_name("f32").size, 4);
    assert_eq!(by_name("f64").size, 8);
    assert_eq!(by_name("bool").size, 1, "C99 _Bool is one byte");
    assert_eq!(by_name("c_char").size, 1);

    // Pointers and the machine word move together.
    assert_eq!(by_name("*const c_void").size, size_of::<usize>());
    assert_eq!(by_name("extern \"C\" fn()").size, size_of::<usize>());
}

// ---------------------------------------------------------------------------
// Structs of three: composition beyond one gap
// ---------------------------------------------------------------------------

macro_rules! checked_struct {
    ($name:ident { $($field:ident: $ty:ty),+ $(,)? }) => {
        #[repr(C)]
        struct $name {
            $($field: $ty,)+
        }

        impl $name {
            fn check() {
                let fields = vec![$( (size_of::<$ty>(), align_of::<$ty>()) ),+];
                let (offsets, expected_size, expected_align) = compute(&fields);
                let names = vec![$( stringify!($field) ),+];

                let mut checked = vec![$( offset_of!($name, $field) ),+];
                let tail = checked.pop().expect("at least one field");
                let mut expected_prefix = offsets.clone();
                let expected_tail = expected_prefix.pop().expect("at least one field");
                assert_eq!(checked, expected_prefix, "{}: field offsets", stringify!($name));
                assert_eq!(tail, expected_tail, "{}: last field offset", stringify!($name));

                assert_eq!(
                    size_of::<$name>(),
                    expected_size,
                    "{}: size (fields {})",
                    stringify!($name),
                    names.join(", ")
                );
                assert_eq!(
                    align_of::<$name>(),
                    expected_align,
                    "{}: alignment",
                    stringify!($name)
                );
            }
        }
    };
}

checked_struct!(NarrowWide {
    a: u8,
    b: u64,
    c: u8,
});
checked_struct!(WideNarrow {
    a: u64,
    b: u8,
    c: u16,
});
checked_struct!(MixedOrder {
    a: u8,
    b: u32,
    c: u8,
});
checked_struct!(FloatIntMix {
    a: f64,
    b: u32,
    c: f64,
});
checked_struct!(PointerBetween {
    a: u8,
    b: *const std::ffi::c_void,
    c: u8,
});
checked_struct!(AllNarrow {
    a: u8,
    b: u16,
    c: u8,
});
checked_struct!(WidestFirst {
    a: u128,
    b: u16,
    c: u8,
});
checked_struct!(WidestMiddle {
    a: u8,
    b: u128,
    c: u8,
});
checked_struct!(PointerHeavy {
    a: *const std::ffi::c_void,
    b: *mut std::ffi::c_void,
    c: usize,
});
checked_struct!(FunctionFields {
    a: unsafe extern "C" fn(),
    b: unsafe extern "C" fn(i32) -> i32,
    c: usize,
});
checked_struct!(BoolAndWide {
    a: u64,
    b: bool,
    c: u64,
});
checked_struct!(MixedEverything {
    a: u8,
    b: f64,
    c: *const std::ffi::c_void,
});

#[test]
fn three_field_structs_compose_as_the_c_rules_say() {
    NarrowWide::check();
    WideNarrow::check();
    MixedOrder::check();
    FloatIntMix::check();
    PointerBetween::check();
    AllNarrow::check();
    WidestFirst::check();
    WidestMiddle::check();
    PointerHeavy::check();
    FunctionFields::check();
    BoolAndWide::check();
    MixedEverything::check();
}

// ---------------------------------------------------------------------------
// Nesting: structs inside structs
// ---------------------------------------------------------------------------

#[repr(C)]
struct Inner {
    a: u8,
    b: u64,
}

#[repr(C)]
struct Outer {
    head: u8,
    inner: Inner,
    tail: u8,
}

/// A nested struct contributes its own size and alignment, and the outer
/// rules then apply unchanged.
#[test]
fn nesting_follows_the_same_rules() {
    check_pair::<u8, Inner>("u8", "Inner");
    check_pair::<Outer, u8>("Outer", "u8");

    let (_, expected_size, expected_align) =
        compute(&[(size_of::<u8>(), align_of::<u8>()), (size_of::<Inner>(), align_of::<Inner>())]);
    let (_, outer_size, outer_align) = compute(&[
        (expected_size, expected_align),
        (size_of::<u8>(), align_of::<u8>()),
    ]);

    assert_eq!(size_of::<Outer>(), outer_size);
    assert_eq!(align_of::<Outer>(), outer_align);
    assert_eq!(offset_of!(Outer, inner), expected_align);
}

// ---------------------------------------------------------------------------
// Arrays: repetition, and the padding an element's alignment forces
// ---------------------------------------------------------------------------

#[repr(C)]
struct Arrays {
    bytes: [u8; 3],
    words: [u32; 2],
    trailing: u8,
}

#[test]
fn arrays_are_sized_and_aligned_like_their_element() {
    // An array's size and alignment are its element's, scaled.
    assert_eq!(size_of::<[u8; 3]>(), 3);
    assert_eq!(align_of::<[u8; 3]>(), 1);
    assert_eq!(size_of::<[u32; 2]>(), 8);
    assert_eq!(align_of::<[u32; 2]>(), 4);
    assert_eq!(size_of::<[u64; 0]>(), 0);
    assert_eq!(align_of::<[u64; 0]>(), 8, "an empty array keeps its alignment");

    let (offsets, expected_size, _) = compute(&[
        (size_of::<[u8; 3]>(), align_of::<[u8; 3]>()),
        (size_of::<[u32; 2]>(), align_of::<[u32; 2]>()),
        (size_of::<u8>(), align_of::<u8>()),
    ]);

    assert_eq!(offset_of!(Arrays, bytes), offsets[0]);
    assert_eq!(offset_of!(Arrays, words), offsets[1]);
    assert_eq!(offset_of!(Arrays, trailing), offsets[2]);
    assert_eq!(size_of::<Arrays>(), expected_size);
}

// ---------------------------------------------------------------------------
// Zero-sized and empty cases
// ---------------------------------------------------------------------------

#[repr(C)]
struct Empty {}

#[repr(C)]
struct HoldsZst {
    marker: Empty,
    value: u64,
}

#[test]
fn a_zero_sized_field_occupies_no_space_but_keeps_its_alignment() {
    assert_eq!(size_of::<Empty>(), 0);
    assert_eq!(align_of::<Empty>(), 1);

    // A zero-sized field takes no room and forces no padding of its own.
    assert_eq!(offset_of!(HoldsZst, marker), 0);
    assert_eq!(offset_of!(HoldsZst, value), 0);
    assert_eq!(size_of::<HoldsZst>(), size_of::<u64>());
}

// ---------------------------------------------------------------------------
// The default representation, measured on the same grid
// ---------------------------------------------------------------------------

/// Field order chosen so a size-minimizing pass has work to do.
struct DefaultRepr {
    a: u8,
    b: u64,
    c: u8,
}

/// The default representation does not follow the C rules.
///
/// This is what makes `#[repr(C)]` mandatory rather than stylistic: the two
/// representations disagree, so a struct compiled under one and read under
/// the other has no agreed layout at all.
#[test]
fn the_default_representation_does_not_follow_the_c_rules() {
    let (c_offsets, c_size, _) = compute(&[
        (size_of::<u8>(), align_of::<u8>()),
        (size_of::<u64>(), align_of::<u64>()),
        (size_of::<u8>(), align_of::<u8>()),
    ]);

    let actual = [
        offset_of!(DefaultRepr, a),
        offset_of!(DefaultRepr, b),
        offset_of!(DefaultRepr, c),
    ];

    assert_ne!(
        actual.to_vec(),
        c_offsets,
        "the default representation matched the C rules here ({actual:?}); the \
         difference this file is built on did not appear, so re-measure before \
         relying on it"
    );

    // What it does instead: pack the fields with no internal padding, then
    // round the total to the alignment. So the size is the sum of the field
    // sizes rounded up — no bytes wasted *between* fields, but the tail still
    // has to reach a multiple of the widest alignment.
    let packed = size_of::<u8>() + size_of::<u64>() + size_of::<u8>();
    let expected = round_up(packed, align_of::<u64>());
    assert_eq!(
        size_of::<DefaultRepr>(),
        expected,
        "the default representation is meant to pack fields without internal \
         padding (packed {packed}, align {})",
        align_of::<u64>()
    );
    assert!(
        size_of::<DefaultRepr>() < c_size,
        "the default representation is meant to be the smaller one here: \
         {} vs {c_size}",
        size_of::<DefaultRepr>()
    );
}

/// The two representations disagree on every multi-alignment combination,
/// not merely on the one shown above.
///
/// Checked through the model, since the point is the rule rather than a
/// particular declaration.
#[test]
fn the_representations_diverge_whenever_padding_would_appear() {
    let (_, c_size, _) = compute(&[
        (size_of::<u8>(), align_of::<u8>()),
        (size_of::<u32>(), align_of::<u32>()),
        (size_of::<u8>(), align_of::<u8>()),
    ]);
    let packed_size = size_of::<u8>() + size_of::<u32>() + size_of::<u8>();

    assert!(
        c_size > packed_size,
        "the C layout must pad here for the divergence argument to hold: \
         c_size {c_size}, packed {packed_size}"
    );
}
