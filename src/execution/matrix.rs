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

use super::numeric::{add_f32, add_f64, mul_f32, mul_f64};
use crate::artifact::MatrixOperation;

// Snapshot all operands before publication, including aliases. Start the dot with its first
// product so signed zero follows scalar left-to-right arithmetic, without an extra +0 or FMA.
macro_rules! matrix_kernel {
    ($name:ident, $scalar:ty, $add:ident, $mul:ident) => {
        pub(super) fn $name(
            operation: MatrixOperation,
            left: &[$scalar; 16],
            right: &[$scalar; 16],
        ) -> [$scalar; 16] {
            let dimension = operation.dimension();
            let mut output = [0.0; 16];
            if operation.is_transpose() {
                for row in 0..dimension {
                    for column in 0..dimension {
                        output[row * dimension + column] = left[column * dimension + row];
                    }
                }
            } else {
                let columns = if operation.is_matrix_product() {
                    dimension
                } else {
                    1
                };
                for row in 0..dimension {
                    for column in 0..columns {
                        let factor = |k: usize| {
                            if operation.is_matrix_product() {
                                right[k * dimension + column]
                            } else if k == 3 {
                                match operation.homogeneous_w() {
                                    Some(true) => 1.0,
                                    Some(false) => 0.0,
                                    None => right[k],
                                }
                            } else {
                                right[k]
                            }
                        };
                        let mut dot = $mul(left[row * dimension], factor(0));
                        for k in 1..dimension {
                            dot = $add(dot, $mul(left[row * dimension + k], factor(k)));
                        }
                        output[row * columns + column] = dot;
                    }
                }
            }
            output
        }
    };
}
matrix_kernel!(evaluate, f32, add_f32, mul_f32);
matrix_kernel!(evaluate_f64, f64, add_f64, mul_f64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_preserves_scalar_rounding_nan_and_signed_zero_semantics() {
        let left = [
            16777216.0,
            1.0,
            -16777216.0,
            1.0,
            -0.0,
            -0.0,
            -0.0,
            -0.0,
            f32::INFINITY,
            0.0,
            0.0,
            0.0,
            f32::from_bits(0x7fa01234),
            1.0,
            2.0,
            3.0,
        ];
        let mut right = [1.0; 16];
        let output = evaluate(MatrixOperation::MultiplyVector, &left, &right);
        assert_eq!(1.0, output[0]);
        assert_eq!((-0.0f32).to_bits(), output[1].to_bits());
        assert_eq!(0x7fc00000, output[3].to_bits());
        right[0] = 0.0;
        assert_eq!(
            0x7fc00000,
            evaluate(MatrixOperation::MultiplyVector, &left, &right)[2].to_bits()
        );
        let mut translated = [0.0; 16];
        translated[3] = f32::INFINITY;
        assert!(evaluate(MatrixOperation::TransformDirection, &translated, &right)[0].is_nan());
        assert_eq!(
            f32::INFINITY,
            evaluate(MatrixOperation::TransformPoint, &translated, &right)[0]
        );
        let transposed = evaluate(MatrixOperation::Transpose, &left, &right);
        for row in 0..4 {
            for column in 0..4 {
                assert_eq!(
                    left[column * 4 + row].to_bits(),
                    transposed[row * 4 + column].to_bits()
                );
            }
        }
    }

    #[test]
    fn matrix_double_dot_keeps_double_rounding_signed_zero_and_nan() {
        let mut left = [0.0; 16];
        let right = [1.0; 16];
        left[..4].copy_from_slice(&[9007199254740992.0, 1.0, -9007199254740992.0, 1.0]);
        left[4..8].fill(-0.0);
        left[8] = f64::from_bits(0x7ff0123456789abc);
        let result = evaluate_f64(MatrixOperation::MultiplyVector4d, &left, &right);
        assert_eq!(1.0, result[0]);
        assert_eq!((-0.0f64).to_bits(), result[1].to_bits());
        assert_eq!(0x7ff8000000000000, result[2].to_bits());
        let transposed = evaluate_f64(MatrixOperation::Transpose4d, &left, &right);
        assert_eq!(left[8].to_bits(), transposed[2].to_bits());
        left[0] = -1.0;
        left[1] = 1.0000000000000002;
        left[2] = 0.0;
        left[3] = 0.0;
        let mut right = [0.0; 16];
        right[0] = 1.0;
        right[1] = 0.9999999999999999;
        assert_eq!(
            0.0,
            evaluate_f64(MatrixOperation::MultiplyVector4d, &left, &right)[0]
        );
        assert_ne!(0.0, left[1].mul_add(right[1], -1.0));
    }

    #[test]
    fn matrix_dot_does_not_fuse_multiplication_and_addition() {
        let mut left = [0.0; 16];
        let mut right = [0.0; 16];
        left[0] = -1.0;
        left[1] = 1.0000001;
        right[0] = 1.0;
        right[1] = 0.99999994;
        assert_eq!(
            0.0,
            evaluate(MatrixOperation::MultiplyVector, &left, &right)[0]
        );
        assert_ne!(0.0, left[1].mul_add(right[1], -1.0));
    }
}
