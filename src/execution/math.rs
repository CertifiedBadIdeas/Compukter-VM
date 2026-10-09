/*
 * The Compukters Developers
 *
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use super::numeric::{canonical_f32, canonical_f64};
use crate::artifact::{MathBinaryOperation, MathUnaryOperation};

pub(super) fn unary_f32(operation: MathUnaryOperation, x: f32) -> f32 {
    canonical_f32(match operation {
        MathUnaryOperation::Abs => libm::fabsf(x),
        MathUnaryOperation::Ceil => libm::ceilf(x),
        MathUnaryOperation::Floor => libm::floorf(x),
        MathUnaryOperation::Truncate => libm::truncf(x),
        MathUnaryOperation::Round => libm::roundevenf(x),
        MathUnaryOperation::Sin => libm::sinf(x),
        MathUnaryOperation::Cos => libm::cosf(x),
        MathUnaryOperation::Tan => libm::tanf(x),
        MathUnaryOperation::Asin => libm::asinf(x),
        MathUnaryOperation::Acos => libm::acosf(x),
        MathUnaryOperation::Atan => libm::atanf(x),
        MathUnaryOperation::Sinh => libm::sinhf(x),
        MathUnaryOperation::Cosh => libm::coshf(x),
        MathUnaryOperation::Tanh => libm::tanhf(x),
        MathUnaryOperation::Asinh => libm::asinhf(x),
        MathUnaryOperation::Acosh => libm::acoshf(x),
        MathUnaryOperation::Atanh => libm::atanhf(x),
        MathUnaryOperation::Sqrt => libm::sqrtf(x),
        MathUnaryOperation::Cbrt => libm::cbrtf(x),
        MathUnaryOperation::Exp => libm::expf(x),
        MathUnaryOperation::Expm1 => libm::expm1f(x),
        MathUnaryOperation::Ln => libm::logf(x),
        MathUnaryOperation::Ln1p => libm::log1pf(x),
        MathUnaryOperation::Log10 => libm::log10f(x),
        MathUnaryOperation::Log2 => libm::log2f(x),
        MathUnaryOperation::Sign => {
            if x == 0.0 || x.is_nan() {
                x
            } else {
                libm::copysignf(1.0, x)
            }
        }
        MathUnaryOperation::Ulp => ulp_f32(x),
        MathUnaryOperation::NextUp => libm::nextafterf(x, f32::INFINITY),
        MathUnaryOperation::NextDown => libm::nextafterf(x, f32::NEG_INFINITY),
    })
}

pub(super) fn binary_f32(operation: MathBinaryOperation, x: f32, y: f32) -> f32 {
    canonical_f32(match operation {
        MathBinaryOperation::Atan2 => libm::atan2f(x, y),
        MathBinaryOperation::Hypot => libm::hypotf(x, y),
        MathBinaryOperation::IeeeRem => libm::remainderf(x, y),
        MathBinaryOperation::CopySign => libm::copysignf(x, y),
        MathBinaryOperation::NextTowards => libm::nextafterf(x, y),
        MathBinaryOperation::Pow => {
            if y.is_nan() || (y.is_infinite() && libm::fabsf(x) == 1.0) {
                f32::NAN
            } else {
                libm::powf(x, y)
            }
        }
        MathBinaryOperation::Min => {
            if x.is_nan() || y.is_nan() {
                f32::NAN
            } else if x == 0.0 && y == 0.0 {
                f32::from_bits(x.to_bits() | y.to_bits())
            } else if x < y {
                x
            } else {
                y
            }
        }
        MathBinaryOperation::Max => {
            if x.is_nan() || y.is_nan() {
                f32::NAN
            } else if x == 0.0 && y == 0.0 {
                f32::from_bits(x.to_bits() & y.to_bits())
            } else if x > y {
                x
            } else {
                y
            }
        }
    })
}

fn ulp_f32(x: f32) -> f32 {
    if x.is_nan() {
        return f32::NAN;
    }
    if x.is_infinite() {
        return f32::INFINITY;
    }
    let exponent = (x.to_bits() >> 23) & 255;
    if exponent <= 23 {
        f32::from_bits(1 << exponent.saturating_sub(1))
    } else {
        f32::from_bits((exponent - 23) << 23)
    }
}

pub(super) fn unary_f64(operation: MathUnaryOperation, x: f64) -> f64 {
    canonical_f64(match operation {
        MathUnaryOperation::Abs => libm::fabs(x),
        MathUnaryOperation::Ceil => libm::ceil(x),
        MathUnaryOperation::Floor => libm::floor(x),
        MathUnaryOperation::Truncate => libm::trunc(x),
        MathUnaryOperation::Round => libm::roundeven(x),
        MathUnaryOperation::Sin => libm::sin(x),
        MathUnaryOperation::Cos => libm::cos(x),
        MathUnaryOperation::Tan => libm::tan(x),
        MathUnaryOperation::Asin => libm::asin(x),
        MathUnaryOperation::Acos => libm::acos(x),
        MathUnaryOperation::Atan => libm::atan(x),
        MathUnaryOperation::Sinh => libm::sinh(x),
        MathUnaryOperation::Cosh => libm::cosh(x),
        MathUnaryOperation::Tanh => libm::tanh(x),
        MathUnaryOperation::Asinh => libm::asinh(x),
        MathUnaryOperation::Acosh => libm::acosh(x),
        MathUnaryOperation::Atanh => libm::atanh(x),
        MathUnaryOperation::Sqrt => libm::sqrt(x),
        MathUnaryOperation::Cbrt => libm::cbrt(x),
        MathUnaryOperation::Exp => libm::exp(x),
        MathUnaryOperation::Expm1 => libm::expm1(x),
        MathUnaryOperation::Ln => libm::log(x),
        MathUnaryOperation::Ln1p => libm::log1p(x),
        MathUnaryOperation::Log10 => libm::log10(x),
        MathUnaryOperation::Log2 => libm::log2(x),
        MathUnaryOperation::Sign => {
            if x == 0.0 || x.is_nan() {
                x
            } else {
                libm::copysign(1.0, x)
            }
        }
        MathUnaryOperation::Ulp => ulp_f64(x),
        MathUnaryOperation::NextUp => libm::nextafter(x, f64::INFINITY),
        MathUnaryOperation::NextDown => libm::nextafter(x, f64::NEG_INFINITY),
    })
}

pub(super) fn binary_f64(operation: MathBinaryOperation, x: f64, y: f64) -> f64 {
    canonical_f64(match operation {
        MathBinaryOperation::Atan2 => libm::atan2(x, y),
        MathBinaryOperation::Hypot => libm::hypot(x, y),
        MathBinaryOperation::IeeeRem => libm::remainder(x, y),
        MathBinaryOperation::CopySign => libm::copysign(x, y),
        MathBinaryOperation::NextTowards => libm::nextafter(x, y),
        MathBinaryOperation::Pow => {
            if y.is_nan() || (y.is_infinite() && libm::fabs(x) == 1.0) {
                f64::NAN
            } else {
                libm::pow(x, y)
            }
        }
        MathBinaryOperation::Min => {
            if x.is_nan() || y.is_nan() {
                f64::NAN
            } else if x == 0.0 && y == 0.0 {
                f64::from_bits(x.to_bits() | y.to_bits())
            } else if x < y {
                x
            } else {
                y
            }
        }
        MathBinaryOperation::Max => {
            if x.is_nan() || y.is_nan() {
                f64::NAN
            } else if x == 0.0 && y == 0.0 {
                f64::from_bits(x.to_bits() & y.to_bits())
            } else if x > y {
                x
            } else {
                y
            }
        }
    })
}

fn ulp_f64(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x.is_infinite() {
        return f64::INFINITY;
    }
    let exponent = (x.to_bits() >> 52) & 2047;
    if exponent <= 52 {
        f64::from_bits(1 << exponent.saturating_sub(1))
    } else {
        f64::from_bits((exponent - 52) << 52)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{MathBinaryOperation as B, MathUnaryOperation as U};

    #[test]
    fn math_preserves_signed_zero_and_canonicalizes_domains() {
        for op in [
            U::Sign,
            U::Round,
            U::Truncate,
            U::Sin,
            U::Tan,
            U::Asin,
            U::Atan,
            U::Sinh,
            U::Tanh,
            U::Asinh,
            U::Atanh,
            U::Sqrt,
            U::Cbrt,
            U::Expm1,
            U::Ln1p,
        ] {
            assert_eq!(
                unary_f64(op, -0.0).to_bits(),
                (-0.0_f64).to_bits(),
                "{op:?}"
            );
            assert_eq!(
                unary_f32(op, -0.0).to_bits(),
                (-0.0_f32).to_bits(),
                "{op:?}"
            );
        }
        assert_eq!(unary_f64(U::Abs, -0.0).to_bits(), 0);
        assert_eq!(unary_f32(U::Abs, -0.0).to_bits(), 0);
        for (op, x) in [
            (U::Sqrt, -1.0),
            (U::Acos, 2.0),
            (U::Ln, -1.0),
            (U::Sin, f64::INFINITY),
            (U::Atanh, 2.0),
        ] {
            assert_eq!(
                unary_f64(op, x).to_bits(),
                super::super::numeric::CANONICAL_F64_NAN
            );
            assert_eq!(unary_f32(op, x as f32).to_bits(), 0x7fc0_0000);
        }
        assert_eq!(unary_f64(U::Ln, 0.0), f64::NEG_INFINITY);
    }

    #[test]
    fn rounding_is_ties_even_and_not_libm_round_away() {
        for (input, expected) in [
            (0.5_f64, 0.0_f64),
            (1.5, 2.0),
            (2.5, 2.0),
            (-0.5, -0.0),
            (-1.5, -2.0),
            (-2.5, -2.0),
        ] {
            assert_eq!(unary_f64(U::Round, input).to_bits(), expected.to_bits());
            assert_eq!(
                unary_f32(U::Round, input as f32).to_bits(),
                (expected as f32).to_bits()
            );
        }
        assert_eq!(unary_f64(U::Floor, -1.1), -2.0);
        assert_eq!(unary_f64(U::Ceil, -1.1), -1.0);
        assert_eq!(unary_f64(U::Truncate, -1.9), -1.0);
    }

    #[test]
    fn kotlin_min_max_pow_and_hypot_special_cases() {
        assert_eq!(
            binary_f64(B::Min, 0.0, -0.0).to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(
            binary_f64(B::Min, -0.0, 0.0).to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(binary_f64(B::Max, -0.0, 0.0).to_bits(), 0);
        assert!(binary_f64(B::Min, f64::NAN, 1.0).is_nan());
        assert!(binary_f64(B::Max, 1.0, f64::NAN).is_nan());
        assert!(binary_f64(B::Pow, 1.0, f64::INFINITY).is_nan());
        assert!(binary_f64(B::Pow, -1.0, f64::NEG_INFINITY).is_nan());
        assert_eq!(binary_f64(B::Pow, f64::NAN, 0.0), 1.0);
        assert!(binary_f64(B::Pow, 1.0, f64::NAN).is_nan());
        assert!(binary_f32(B::Pow, 1.0, f32::NAN).is_nan());
        assert_eq!(binary_f64(B::Pow, -0.0, -3.0), f64::NEG_INFINITY);
        assert_eq!(binary_f64(B::Hypot, f64::INFINITY, f64::NAN), f64::INFINITY);
        assert!(binary_f64(B::Hypot, 1e300, 1e300).is_finite());
        assert_eq!(binary_f64(B::IeeeRem, 7.0, 2.0), -1.0);
        assert_eq!(binary_f64(B::CopySign, 3.0, -0.0), -3.0);
        assert_eq!(
            binary_f64(B::Atan2, -0.0, 1.0).to_bits(),
            (-0.0_f64).to_bits()
        );
    }

    #[test]
    fn ulp_and_adjacent_values_cover_subnormals_boundaries_and_zero() {
        assert_eq!(unary_f64(U::Ulp, 0.0).to_bits(), 1);
        assert_eq!(unary_f32(U::Ulp, -0.0).to_bits(), 1);
        assert_eq!(unary_f64(U::Ulp, f64::MIN_POSITIVE).to_bits(), 1);
        assert_eq!(unary_f64(U::Ulp, 1.0), f64::EPSILON);
        assert_eq!(unary_f32(U::Ulp, 1.0), f32::EPSILON);
        assert_eq!(unary_f64(U::Ulp, f64::MAX), libm::scalbn(1.0, 971));
        assert_eq!(unary_f64(U::Ulp, f64::NEG_INFINITY), f64::INFINITY);
        assert_eq!(unary_f64(U::NextUp, -0.0).to_bits(), 1);
        assert_eq!(unary_f64(U::NextDown, 0.0).to_bits(), (1u64 << 63) | 1);
        assert_eq!(unary_f64(U::NextUp, f64::MAX), f64::INFINITY);
        assert_eq!(unary_f64(U::NextDown, f64::INFINITY), f64::MAX);
        assert_eq!(
            binary_f64(B::NextTowards, 0.0, -0.0).to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(binary_f32(B::NextTowards, -0.0, 0.0).to_bits(), 0);
    }

    #[test]
    fn software_math_reference_vectors_are_accurate_for_both_widths() {
        for (op, x, expected) in [
            (U::Sin, 0.5, 0.479425538604203),
            (U::Cos, 0.5, 0.8775825618903728),
            (U::Tan, 0.5, 0.5463024898437905),
            (U::Asin, 0.5, std::f64::consts::FRAC_PI_6),
            (U::Acos, 0.5, std::f64::consts::FRAC_PI_3),
            (U::Atan, 0.5, 0.4636476090008061),
            (U::Sinh, 0.5, 0.5210953054937474),
            (U::Cosh, 0.5, 1.1276259652063807),
            (U::Tanh, 0.5, 0.46211715726000974),
            (U::Asinh, 0.5, 0.48121182505960347),
            (U::Acosh, 2.0, 1.3169578969248166),
            (U::Atanh, 0.5, 0.5493061443340549),
            (U::Sqrt, 2.0, std::f64::consts::SQRT_2),
            (U::Cbrt, -8.0, -2.0),
            (U::Exp, 0.5, 1.6487212707001282),
            (U::Expm1, 0.5, 0.6487212707001282),
            (U::Ln, 2.0, std::f64::consts::LN_2),
            (U::Ln1p, 0.5, 0.4054651081081644),
            (U::Log10, 100.0, 2.0),
            (U::Log2, 8.0, 3.0),
        ] {
            assert!((unary_f64(op, x) - expected).abs() < 2e-15, "{op:?}");
            assert!(
                (unary_f32(op, x as f32) - expected as f32).abs() < 3e-7,
                "{op:?}"
            );
        }
    }
}
