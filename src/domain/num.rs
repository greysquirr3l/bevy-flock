//! The one place this crate converts between integers and floats.
//!
//! The simulation keeps positions and directions in `f32`, and cell coordinates, counts, ticks and
//! byte lengths in integers, so the boundary between the two is crossed thousands of times a frame.
//! `as` at each of those crossings is precisely what `clippy::cast_precision_loss`,
//! `cast_possible_truncation`, `cast_sign_loss` and `cast_possible_wrap` exist to forbid: every one
//! of them saturates, truncates, rounds or wraps *silently*, and none of them says which.
//!
//! So the crossing happens here, once per direction, with the behaviour written down and tested:
//!
//! * **Integer to integer, narrowing** saturates. `usize_to_u32(usize::MAX) == u32::MAX`, never a
//!   wrapped remainder. A count that cannot be represented is a bug in the caller, but it is not a
//!   silent one, and the value it produces is still bounded and usable.
//! * **Integer to float** rounds to nearest. Every integer above 2^24 loses precision, because an
//!   `f32` mantissa is 23 bits wide; the helpers exist so that the loss is a decision made here
//!   rather than an accident made in a kernel.
//! * **Float to integer** truncates toward zero and saturates at both ends, which is Rust's `as`
//!   behaviour and therefore the behaviour every caller already assumed. `NaN` maps to zero.
//!
//! Widening integer conversions (`u8` to `u32`, `i32` to `i64`) are exact and use `From`, so they
//! are not here. Same-width sign changes (`u32` to `i32`, `i64` to `u64`) are exact too and use the
//! standard library's `cast_signed` and `cast_unsigned`, so they are not here either.
//!
//! Each helper carries one `#[expect]` naming the lint it deliberately satisfies. That is the whole
//! point of the module: the crate denies those four lints everywhere else, so a cast can only appear
//! in a function that has said out loud what it does with the bits.

// Every cast in this file is deliberate: that is what the file is for. The alternative — an
// expectation on each helper — was tried and is worse, because a helper's bound expression casts
// too, so the set of lints differs per function and drifts the moment a bound changes. One
// attribute, one reason, and the doc comment above each helper says what the bits do.
#![expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "this module is the crate's only numeric conversion boundary; each helper documents its rounding"
)]

/// Widest integer an `f32` represents exactly; beyond it, rounding begins.
pub const F32_EXACT_INTEGER: u32 = 1 << 24;

/// Converts an element count to `u32`, saturating at [`u32::MAX`].
#[inline]
#[must_use]
pub const fn i64_to_count_u64(value: i64) -> u64 {
    if value < 0 { 0 } else { value as u64 }
}

/// Reinterprets a signed value's bits as unsigned, without changing them.
#[inline]
#[must_use]
pub const fn i64_to_u64(value: i64) -> u64 {
    value.cast_unsigned()
}

/// Converts an `f32` to `u32`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f32_to_u32(value: f32) -> u32 {
    value as u32
}

/// Converts an `f32` to `u16`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f32_to_u16(value: f32) -> u16 {
    value as u16
}

/// Converts an `f32` to `u8`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f32_to_u8(value: f32) -> u8 {
    value as u8
}

/// Converts an `f32` to `u64`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f32_to_u64(value: f32) -> u64 {
    value as u64
}

/// Converts an `f32` to `usize`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f32_to_usize(value: f32) -> usize {
    value as usize
}

/// Converts an `f32` to `i32`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f32_to_i32(value: f32) -> i32 {
    value as i32
}

/// Converts an `f32` to `i64`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f32_to_i64(value: f32) -> i64 {
    value as i64
}

/// Converts an `f32` to `i16`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f32_to_i16(value: f32) -> i16 {
    value as i16
}

/// Converts an `f32` to `i8`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f32_to_i8(value: f32) -> i8 {
    value as i8
}

/// Converts an `f64` to `u32`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f64_to_u32(value: f64) -> u32 {
    value as u32
}

/// Converts an `f64` to `i64`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f64_to_i64(value: f64) -> i64 {
    value as i64
}

/// Converts an `f64` to `i16`, truncating toward zero and saturating; `NaN` becomes zero.
#[inline]
#[must_use]
pub const fn f64_to_i16(value: f64) -> i16 {
    value as i16
}

/// Keeps the low 32 bits of a 64-bit word.
///
/// Distinct from [`u64_to_u32`], which saturates: a hash, a PRNG output or a mixed seed has no
/// "too large" case, and folding it to `u32::MAX` would collapse every such value to one number.
/// This is a truncation by definition, so it says so.
#[inline]
#[must_use]
pub const fn u64_to_low_u32(value: u64) -> u32 {
    value as u32
}

/// Keeps the low 16 bits of a 64-bit word, for the same reason as [`u64_to_low_u32`].
#[inline]
#[must_use]
pub const fn u64_to_low_u16(value: u64) -> u16 {
    value as u16
}

/// Converts a length or index to `u32`, saturating.
///
/// A count that cannot be represented is a caller's bug, but it stays bounded and visible rather
/// than wrapping to a small number that silently addresses the wrong agent.
#[inline]
#[must_use]
pub const fn usize_to_u32(value: usize) -> u32 {
    if value > u32::MAX as usize {
        u32::MAX
    } else {
        value as u32
    }
}

/// Converts a length or index to `u16`, saturating.
#[inline]
#[must_use]
pub const fn usize_to_u16(value: usize) -> u16 {
    if value > u16::MAX as usize {
        u16::MAX
    } else {
        value as u16
    }
}

/// Converts a length or index to `u8`, saturating.
#[inline]
#[must_use]
pub const fn usize_to_u8(value: usize) -> u8 {
    if value > u8::MAX as usize {
        u8::MAX
    } else {
        value as u8
    }
}

/// Converts a length or index to `i32`, saturating.
#[inline]
#[must_use]
pub const fn usize_to_i32(value: usize) -> i32 {
    if value > i32::MAX as usize {
        i32::MAX
    } else {
        value as i32
    }
}

/// Converts a length or index to `i64`, saturating.
#[inline]
#[must_use]
pub const fn usize_to_i64(value: usize) -> i64 {
    if value > i64::MAX as usize {
        i64::MAX
    } else {
        value as i64
    }
}

/// Converts a 64-bit word to `u32`, saturating.
///
/// Use [`u64_to_low_u32`] where the word is a hash or a PRNG output, because there truncation is
/// the operation rather than an overflow.
#[inline]
#[must_use]
pub const fn u64_to_u32(value: u64) -> u32 {
    if value > u32::MAX as u64 {
        u32::MAX
    } else {
        value as u32
    }
}

/// Converts a 64-bit word to `usize`, saturating.
#[inline]
#[must_use]
pub const fn u64_to_usize(value: u64) -> usize {
    if value > usize::MAX as u64 {
        usize::MAX
    } else {
        value as usize
    }
}

/// Converts a signed index to `usize`, clamping negatives to zero.
///
/// A negative cell coordinate is off the grid rather than large, so clamping keeps a lookup on the
/// grid instead of wrapping it to `usize::MAX` and reading out of bounds.
#[inline]
#[must_use]
pub const fn i32_to_usize(value: i32) -> usize {
    if value < 0 { 0 } else { value as usize }
}

/// Converts a 32-bit count to `u16`, saturating.
#[inline]
#[must_use]
pub const fn u32_to_u16(value: u32) -> u16 {
    if value > u16::MAX as u32 {
        u16::MAX
    } else {
        value as u16
    }
}

/// Converts a signed index to `u16`, clamping negatives to zero and large values to `u16::MAX`.
#[inline]
#[must_use]
pub const fn i32_to_u16(value: i32) -> u16 {
    if value < 0 {
        0
    } else if value > u16::MAX as i32 {
        u16::MAX
    } else {
        value as u16
    }
}

/// Converts a signed index to `u32`, clamping negatives to zero and large values to `u32::MAX`.
#[inline]
#[must_use]
pub const fn i64_to_u32(value: i64) -> u32 {
    if value < 0 {
        0
    } else if value > u32::MAX as i64 {
        u32::MAX
    } else {
        value as u32
    }
}

/// Converts a signed index to `usize`, clamping negatives to zero.
#[inline]
#[must_use]
pub const fn i64_to_usize(value: i64) -> usize {
    if value < 0 { 0 } else { value as usize }
}

/// Converts an `f64` result to `f32`, rounding to nearest and saturating on overflow.
///
/// The simulation computes trigonometry and normalisation in `f64` where the extra bits matter,
/// then stores the result as `f32`. Doing that conversion here keeps the rounding a decision
/// rather than an accident, and keeps `NaN` a `NaN` rather than a surprise.
#[inline]
#[must_use]
pub const fn f64_to_f32(value: f64) -> f32 {
    value as f32
}

/// Converts an unsigned count to `f32`, rounding to nearest.
///
/// Lossy above [`F32_EXACT_INTEGER`]: the caller is asking for a float coordinate, and an `f32`
/// cannot hold a 32-bit index exactly.
#[inline]
#[must_use]
pub const fn u32_to_f32(value: u32) -> f32 {
    value as f32
}

/// Converts an unsigned counter to `f32`, rounding to nearest.
#[inline]
#[must_use]
pub const fn u64_to_f32(value: u64) -> f32 {
    value as f32
}

/// Converts an unsigned counter to `f64`, rounding to nearest.
#[inline]
#[must_use]
pub const fn u64_to_f64(value: u64) -> f64 {
    value as f64
}

/// Converts an element count to `f32`, rounding to nearest.
#[inline]
#[must_use]
pub const fn usize_to_f32(value: usize) -> f32 {
    value as f32
}

/// Converts an element count to `f64`, rounding to nearest.
#[inline]
#[must_use]
pub const fn usize_to_f64(value: usize) -> f64 {
    value as f64
}

/// Converts a signed coordinate to `f32`, rounding to nearest.
#[inline]
#[must_use]
pub const fn i32_to_f32(value: i32) -> f32 {
    value as f32
}

/// Converts a signed counter to `f32`, rounding to nearest.
#[inline]
#[must_use]
pub const fn i64_to_f32(value: i64) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_bits_are_kept_where_the_word_is_a_hash() {
        assert_eq!(u64_to_low_u32(0x1_2345_6789), 0x2345_6789);
        assert_eq!(u64_to_low_u32(u64::MAX), u32::MAX);
        assert_eq!(u64_to_low_u32(0), 0);
        assert_eq!(u64_to_low_u16(0x1_2345_6789), 0x6789);
        // The point of the helper: the high word is dropped, the low word is kept exactly.
        assert_eq!(u64_to_low_u32(0xFFFF_FFFF_0000_0001), 1);
        assert_ne!(
            u64_to_low_u32(0xFFFF_FFFF_0000_0001),
            u64_to_low_u32(0xFFFF_FFFF_0000_0002)
        );
    }

    #[test]
    fn integer_narrowing_saturates_instead_of_wrapping() {
        assert_eq!(usize_to_u32(7), 7);
        assert_eq!(usize_to_u32(usize::MAX), u32::MAX);
        assert_eq!(usize_to_u16(usize::MAX), u16::MAX);
        assert_eq!(usize_to_u8(usize::MAX), u8::MAX);
        assert_eq!(usize_to_i32(usize::MAX), i32::MAX);
        assert_eq!(u64_to_u32(u64::MAX), u32::MAX);
        assert_eq!(u64_to_usize(u64::MAX), usize::MAX);
    }

    #[test]
    fn signed_narrowing_clamps_at_zero() {
        assert_eq!(i32_to_usize(-3), 0);
        assert_eq!(i32_to_u16(-1), 0);
        assert_eq!(i64_to_u32(-1), 0);
        assert_eq!(i64_to_usize(-1), 0);
        assert_eq!(i64_to_count_u64(-1), 0);
        assert_eq!(i32_to_usize(12), 12);
    }

    #[test]
    fn bit_reinterpretation_keeps_the_bits() {
        assert_eq!(i64_to_u64(-1), u64::MAX);
        assert_eq!(i64_to_u64(0), 0);
        assert_eq!((-1_i32).cast_unsigned(), u32::MAX);
        assert_eq!(u32::MAX.cast_signed(), -1_i32);
    }

    #[test]
    fn float_to_integer_truncates_toward_zero() {
        assert_eq!(f32_to_u32(3.9), 3);
        assert_eq!(f32_to_i32(-3.9), -3);
        assert_eq!(f32_to_u16(2.5), 2);
        assert_eq!(f32_to_i64(1.99), 1);
        assert_eq!(f32_to_usize(0.5), 0);
    }

    #[test]
    fn float_to_integer_saturates_at_both_ends() {
        assert_eq!(f32_to_u32(-5.0), 0);
        assert_eq!(f32_to_u16(f32::MAX), u16::MAX);
        assert_eq!(f32_to_i32(f32::MIN), i32::MIN);
        assert_eq!(f32_to_u64(f32::MAX), u64::MAX);
        assert_eq!(f32_to_i8(-300.0), i8::MIN);
        assert_eq!(f64_to_u32(-1.0), 0);
        assert_eq!(f64_to_i64(f64::MAX), i64::MAX);
    }

    #[test]
    fn nan_becomes_zero_rather_than_an_undefined_value() {
        assert_eq!(f32_to_u32(f32::NAN), 0);
        assert_eq!(f32_to_i32(f32::NAN), 0);
        assert_eq!(f32_to_usize(f32::NAN), 0);
        assert_eq!(f64_to_i64(f64::NAN), 0);
    }

    #[test]
    fn integer_to_float_is_exact_up_to_the_mantissa() {
        assert_eq!(
            u32_to_f32(F32_EXACT_INTEGER).to_bits(),
            16_777_216.0_f32.to_bits()
        );
        assert_eq!(u32_to_f32(1).to_bits(), 1.0_f32.to_bits());
        assert_eq!(i32_to_f32(-2).to_bits(), (-2.0_f32).to_bits());
        assert_eq!(usize_to_f32(3).to_bits(), 3.0_f32.to_bits());
        assert_eq!(
            u64_to_f64(1_u64 << 53).to_bits(),
            9_007_199_254_740_992.0_f64.to_bits()
        );
        assert_eq!(i64_to_f32(-4).to_bits(), (-4.0_f32).to_bits());
        assert_eq!(u64_to_f32(0).to_bits(), 0.0_f32.to_bits());
        assert_eq!(usize_to_f64(5).to_bits(), 5.0_f64.to_bits());
    }
}
