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

use super::numeric::{add_f32, mul_f32};
use crate::artifact::MatrixOperation;

fn dot(a: [f32; 4], b: [f32; 4]) -> f32 {
    add_f32(
        add_f32(
            add_f32(mul_f32(a[0], b[0]), mul_f32(a[1], b[1])),
            mul_f32(a[2], b[2]),
        ),
        mul_f32(a[3], b[3]),
    )
}

/// Stack-only snapshots make all verified destination/input aliases safe.
pub(super) fn evaluate(
    operation: MatrixOperation,
    left: &[f32; 16],
    right: &[f32; 16],
) -> [f32; 16] {
    let mut output = [0.0; 16];
    match operation {
        MatrixOperation::Transpose => {
            for row in 0..4 {
                for column in 0..4 {
                    output[row * 4 + column] = left[column * 4 + row];
                }
            }
        }
        MatrixOperation::MultiplyMatrix => {
            for row in 0..4 {
                for column in 0..4 {
                    output[row * 4 + column] = dot(
                        [
                            left[row * 4],
                            left[row * 4 + 1],
                            left[row * 4 + 2],
                            left[row * 4 + 3],
                        ],
                        [
                            right[column],
                            right[4 + column],
                            right[8 + column],
                            right[12 + column],
                        ],
                    );
                }
            }
        }
        _ => {
            let w = match operation {
                MatrixOperation::TransformPoint => 1.0,
                MatrixOperation::TransformDirection => 0.0,
                _ => right[3],
            };
            for row in 0..4 {
                output[row] = dot(
                    [
                        left[row * 4],
                        left[row * 4 + 1],
                        left[row * 4 + 2],
                        left[row * 4 + 3],
                    ],
                    [right[0], right[1], right[2], w],
                );
            }
        }
    }
    output
}

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
