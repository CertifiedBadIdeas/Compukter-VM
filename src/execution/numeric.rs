use super::error::GuestTrap;

pub(super) const CANONICAL_F32_NAN: u32 = 0x7fc0_0000;
pub(super) const CANONICAL_F64_NAN: u64 = 0x7ff8_0000_0000_0000;

macro_rules! wrapping_binary {
    ($name:ident, $ty:ty, $method:ident) => {
        pub(super) fn $name(lhs: $ty, rhs: $ty) -> $ty {
            lhs.$method(rhs)
        }
    };
}

wrapping_binary!(add_i32, i32, wrapping_add);
wrapping_binary!(add_i64, i64, wrapping_add);
wrapping_binary!(sub_i32, i32, wrapping_sub);
wrapping_binary!(sub_i64, i64, wrapping_sub);
wrapping_binary!(mul_i32, i32, wrapping_mul);
wrapping_binary!(mul_i64, i64, wrapping_mul);

pub(super) fn neg_i32(value: i32) -> i32 {
    value.wrapping_neg()
}

pub(super) fn neg_i64(value: i64) -> i64 {
    value.wrapping_neg()
}

pub(super) fn shl_i32(value: i32, count: i32) -> i32 {
    value.wrapping_shl((count as u32) & 31)
}

pub(super) fn shl_i64(value: i64, count: i32) -> i64 {
    value.wrapping_shl((count as u32) & 63)
}

pub(super) fn shr_i32(value: i32, count: i32) -> i32 {
    value.wrapping_shr((count as u32) & 31)
}

pub(super) fn shr_i64(value: i64, count: i32) -> i64 {
    value.wrapping_shr((count as u32) & 63)
}

pub(super) fn ushr_i32(value: i32, count: i32) -> i32 {
    ((value as u32).wrapping_shr((count as u32) & 31)) as i32
}

pub(super) fn ushr_i64(value: i64, count: i32) -> i64 {
    ((value as u64).wrapping_shr((count as u32) & 63)) as i64
}

pub(super) fn div_i32(lhs: i32, rhs: i32) -> Result<i32, GuestTrap> {
    if rhs == 0 {
        Err(GuestTrap::DivisionByZero)
    } else if lhs == i32::MIN && rhs == -1 {
        Ok(i32::MIN)
    } else {
        Ok(lhs / rhs)
    }
}

pub(super) fn div_i64(lhs: i64, rhs: i64) -> Result<i64, GuestTrap> {
    if rhs == 0 {
        Err(GuestTrap::DivisionByZero)
    } else if lhs == i64::MIN && rhs == -1 {
        Ok(i64::MIN)
    } else {
        Ok(lhs / rhs)
    }
}

pub(super) fn rem_i32(lhs: i32, rhs: i32) -> Result<i32, GuestTrap> {
    if rhs == 0 {
        Err(GuestTrap::DivisionByZero)
    } else if lhs == i32::MIN && rhs == -1 {
        Ok(0)
    } else {
        Ok(lhs % rhs)
    }
}

pub(super) fn rem_i64(lhs: i64, rhs: i64) -> Result<i64, GuestTrap> {
    if rhs == 0 {
        Err(GuestTrap::DivisionByZero)
    } else if lhs == i64::MIN && rhs == -1 {
        Ok(0)
    } else {
        Ok(lhs % rhs)
    }
}

pub(super) fn canonical_f32(value: f32) -> f32 {
    if value.is_nan() {
        f32::from_bits(CANONICAL_F32_NAN)
    } else {
        value
    }
}

pub(super) fn canonical_f64(value: f64) -> f64 {
    if value.is_nan() {
        f64::from_bits(CANONICAL_F64_NAN)
    } else {
        value
    }
}

macro_rules! float_binary {
    ($name:ident, $ty:ty, $canonical:ident, $operator:tt) => {
        pub(super) fn $name(lhs: $ty, rhs: $ty) -> $ty {
            $canonical(lhs $operator rhs)
        }
    };
}

float_binary!(add_f32, f32, canonical_f32, +);
float_binary!(sub_f32, f32, canonical_f32, -);
float_binary!(mul_f32, f32, canonical_f32, *);
float_binary!(div_f32, f32, canonical_f32, /);
float_binary!(rem_f32, f32, canonical_f32, %);
float_binary!(add_f64, f64, canonical_f64, +);
float_binary!(sub_f64, f64, canonical_f64, -);
float_binary!(mul_f64, f64, canonical_f64, *);
float_binary!(div_f64, f64, canonical_f64, /);
float_binary!(rem_f64, f64, canonical_f64, %);

pub(super) fn neg_f32(value: f32) -> f32 {
    canonical_f32(-value)
}

pub(super) fn neg_f64(value: f64) -> f64 {
    canonical_f64(-value)
}

pub(super) fn i32_to_i64(value: i32) -> i64 {
    i64::from(value)
}

pub(super) fn i64_to_i32(value: i64) -> i32 {
    value as i32
}

pub(super) fn i32_to_f32(value: i32) -> f32 {
    value as f32
}

pub(super) fn i32_to_f64(value: i32) -> f64 {
    f64::from(value)
}

pub(super) fn i64_to_f32(value: i64) -> f32 {
    value as f32
}

pub(super) fn i64_to_f64(value: i64) -> f64 {
    value as f64
}

pub(super) fn f32_to_f64(value: f32) -> f64 {
    canonical_f64(f64::from(value))
}

pub(super) fn f64_to_f32(value: f64) -> f32 {
    canonical_f32(value as f32)
}

macro_rules! float_to_integer {
    ($name:ident, $source:ty, $target:ty) => {
        pub(super) fn $name(value: $source) -> $target {
            if value.is_nan() {
                0
            } else if value >= <$target>::MAX as $source {
                <$target>::MAX
            } else if value <= <$target>::MIN as $source {
                <$target>::MIN
            } else {
                value.trunc() as $target
            }
        }
    };
}

float_to_integer!(f32_to_i32, f32, i32);
float_to_integer!(f32_to_i64, f32, i64);
float_to_integer!(f64_to_i32, f64, i32);
float_to_integer!(f64_to_i64, f64, i64);

pub(super) fn i32_to_char(value: i32) -> u16 {
    value as u16
}

pub(super) fn char_to_i32(value: u16) -> i32 {
    i32::from(value)
}

macro_rules! float_comparisons {
    ($eq:ident, $ne:ident, $lt:ident, $le:ident, $gt:ident, $ge:ident, $ty:ty) => {
        pub(super) fn $eq(lhs: $ty, rhs: $ty) -> bool {
            lhs == rhs
        }
        pub(super) fn $ne(lhs: $ty, rhs: $ty) -> bool {
            lhs != rhs
        }
        pub(super) fn $lt(lhs: $ty, rhs: $ty) -> bool {
            lhs < rhs
        }
        pub(super) fn $le(lhs: $ty, rhs: $ty) -> bool {
            lhs <= rhs
        }
        pub(super) fn $gt(lhs: $ty, rhs: $ty) -> bool {
            lhs > rhs
        }
        pub(super) fn $ge(lhs: $ty, rhs: $ty) -> bool {
            lhs >= rhs
        }
    };
}

float_comparisons!(eq_f32, ne_f32, lt_f32, le_f32, gt_f32, ge_f32, f32);
float_comparisons!(eq_f64, ne_f64, lt_f64, le_f64, gt_f64, ge_f64, f64);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{error::GuestTrap, value::RuntimeValue};

    #[test]
    fn integers_wrap_mask_shifts_and_handle_min_division() {
        assert_eq!(i32::MAX.wrapping_add(1), add_i32(i32::MAX, 1));
        assert_eq!(i64::MIN, add_i64(i64::MAX, 1));
        assert_eq!(i32::MAX, sub_i32(i32::MIN, 1));
        assert_eq!(i64::MAX, sub_i64(i64::MIN, 1));
        assert_eq!(-2, mul_i32(i32::MAX, 2));
        assert_eq!(-2, mul_i64(i64::MAX, 2));
        assert_eq!(i32::MIN, neg_i32(i32::MIN));
        assert_eq!(i64::MIN, neg_i64(i64::MIN));
        assert_eq!(1, shl_i32(1, 32));
        assert_eq!(2, shl_i64(1, 65));
        assert_eq!(-1, shr_i32(-1, 33));
        assert_eq!(-1, shr_i64(-1, 65));
        assert_eq!(i32::MAX, ushr_i32(-1, 1));
        assert_eq!(i64::MAX, ushr_i64(-1, 1));
        assert_eq!(Ok(i32::MIN), div_i32(i32::MIN, -1));
        assert_eq!(Ok(i64::MIN), div_i64(i64::MIN, -1));
        assert_eq!(Ok(0), rem_i32(i32::MIN, -1));
        assert_eq!(Ok(0), rem_i64(i64::MIN, -1));
        assert_eq!(Err(GuestTrap::DivisionByZero), rem_i32(1, 0));
        assert_eq!(Err(GuestTrap::DivisionByZero), div_i64(1, 0));
    }

    #[test]
    fn produced_nans_are_canonical_and_constants_are_not_rewritten() {
        assert_eq!(CANONICAL_F32_NAN, canonical_f32(f32::NAN).to_bits());
        assert_eq!(CANONICAL_F64_NAN, rem_f64(f64::INFINITY, 1.0).to_bits());
        assert_eq!(1.5_f32.to_bits(), add_f32(1.0, 0.5).to_bits());
        assert_eq!(0.5_f32.to_bits(), sub_f32(1.0, 0.5).to_bits());
        assert_eq!(2.0_f32.to_bits(), mul_f32(4.0, 0.5).to_bits());
        assert_eq!(f32::INFINITY.to_bits(), div_f32(1.0, 0.0).to_bits());
        assert_eq!((-0.0_f32).to_bits(), neg_f32(0.0).to_bits());
        assert_eq!(1.5_f64.to_bits(), add_f64(1.0, 0.5).to_bits());
        assert_eq!(0.5_f64.to_bits(), sub_f64(1.0, 0.5).to_bits());
        assert_eq!(2.0_f64.to_bits(), mul_f64(4.0, 0.5).to_bits());
        assert_eq!(f64::INFINITY.to_bits(), div_f64(1.0, 0.0).to_bits());
        assert_eq!((-0.0_f64).to_bits(), neg_f64(0.0).to_bits());
        assert_eq!(1.0_f32.to_bits(), rem_f32(5.0, 2.0).to_bits());
        assert_eq!(5.0_f64.to_bits(), rem_f64(5.0, f64::INFINITY).to_bits());
        assert_eq!(
            0x7fa0_0001,
            RuntimeValue::F32(0x7fa0_0001).trace_bits_u64() as u32
        );
    }

    #[test]
    fn float_to_integer_matches_jvm_truncation_and_saturation() {
        assert_eq!(0, f64_to_i32(f64::NAN));
        assert_eq!(i32::MAX, f64_to_i32(f64::INFINITY));
        assert_eq!(i32::MIN, f64_to_i32(f64::NEG_INFINITY));
        assert_eq!(3, f64_to_i32(3.99));
        assert_eq!(-3, f64_to_i64(-3.99));
        assert_eq!(i64::from(i32::MIN), i32_to_i64(i32::MIN));
        assert_eq!(-1, i64_to_i32(u32::MAX as i64));
        assert_eq!(16_777_216.0_f32, i32_to_f32(16_777_217));
        assert_eq!(-2_147_483_648.0_f64, i32_to_f64(i32::MIN));
        assert_eq!(9_223_372_036_854_775_808.0_f32, i64_to_f32(i64::MAX));
        assert_eq!(9_007_199_254_740_992.0, i64_to_f64(9_007_199_254_740_993));
        assert_eq!(1.5_f64, f32_to_f64(1.5));
        assert_eq!(1.5_f32, f64_to_f32(1.5));
        assert_eq!(3, f32_to_i32(3.99));
        assert_eq!(0, f32_to_i64(f32::NAN));
        assert_eq!(i64::MAX, f32_to_i64(f32::INFINITY));
        assert_eq!(0x0041, i32_to_char(65));
        assert_eq!(0xffff, i32_to_char(-1));
        assert_eq!(0xffff, i32_to_char(65_535));
        assert_eq!(0x0000, i32_to_char(65_536));
        assert_eq!(0x0000, i32_to_char(i32::MIN));
        assert_eq!(0xffff, i32_to_char(i32::MAX));
        assert_eq!(0xd800, i32_to_char(0xd800));
        assert_eq!(0xd800, char_to_i32(0xd800));
    }

    #[test]
    fn primitive_float_comparison_uses_kotlin_rules() {
        assert!(!eq_f64(f64::NAN, f64::NAN));
        assert!(eq_f32(-0.0, 0.0));
        assert!(ne_f32(f32::NAN, f32::NAN));
        assert!(!lt_f64(f64::NAN, 1.0));
        assert!(ne_f64(f64::NAN, f64::NAN));
        assert!(lt_f32(1.0, 2.0));
        assert!(le_f32(2.0, 2.0));
        assert!(gt_f32(2.0, 1.0));
        assert!(ge_f32(2.0, 2.0));
        assert!(le_f64(2.0, 2.0));
        assert!(gt_f64(2.0, 1.0));
        assert!(ge_f64(2.0, 2.0));
    }
}
