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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MatrixOperation {
    Transpose = 1,
    MultiplyMatrix = 2,
    MultiplyVector = 3,
    TransformPoint = 4,
    TransformDirection = 5,
    Transpose3f = 6,
    MultiplyMatrix3f = 7,
    MultiplyVector3f = 8,
    Transpose3d = 9,
    MultiplyMatrix3d = 10,
    MultiplyVector3d = 11,
    Transpose4d = 12,
    MultiplyMatrix4d = 13,
    MultiplyVector4d = 14,
    TransformPoint4d = 15,
    TransformDirection4d = 16,
}

impl MatrixOperation {
    pub(crate) fn decode(selector: u32) -> Option<Self> {
        Some(match selector {
            1 => Self::Transpose,
            2 => Self::MultiplyMatrix,
            3 => Self::MultiplyVector,
            4 => Self::TransformPoint,
            5 => Self::TransformDirection,
            6 => Self::Transpose3f,
            7 => Self::MultiplyMatrix3f,
            8 => Self::MultiplyVector3f,
            9 => Self::Transpose3d,
            10 => Self::MultiplyMatrix3d,
            11 => Self::MultiplyVector3d,
            12 => Self::Transpose4d,
            13 => Self::MultiplyMatrix4d,
            14 => Self::MultiplyVector4d,
            15 => Self::TransformPoint4d,
            16 => Self::TransformDirection4d,
            _ => return None,
        })
    }
    pub(crate) fn dimension(self) -> usize {
        match self {
            Self::Transpose3f
            | Self::MultiplyMatrix3f
            | Self::MultiplyVector3f
            | Self::Transpose3d
            | Self::MultiplyMatrix3d
            | Self::MultiplyVector3d => 3,
            _ => 4,
        }
    }
    pub(crate) fn scalar_kind(self) -> u8 {
        if (self as u32) >= 9 {
            4
        } else {
            3
        }
    }
    pub(crate) fn runtime_minor(self) -> u16 {
        if (self as u32) >= 6 {
            19
        } else {
            17
        }
    }
    pub(crate) fn is_transpose(self) -> bool {
        matches!(
            self,
            Self::Transpose | Self::Transpose3f | Self::Transpose3d | Self::Transpose4d
        )
    }
    pub(crate) fn is_matrix_product(self) -> bool {
        matches!(
            self,
            Self::MultiplyMatrix
                | Self::MultiplyMatrix3f
                | Self::MultiplyMatrix3d
                | Self::MultiplyMatrix4d
        )
    }
    pub(crate) fn homogeneous_w(self) -> Option<bool> {
        match self {
            Self::TransformPoint | Self::TransformPoint4d => Some(true),
            Self::TransformDirection | Self::TransformDirection4d => Some(false),
            _ => None,
        }
    }
    pub(crate) fn left_components(self) -> usize {
        self.dimension() * self.dimension()
    }
    pub(crate) fn right_components(self) -> Option<usize> {
        if self.is_transpose() {
            None
        } else if self.is_matrix_product() {
            Some(self.left_components())
        } else if self.homogeneous_w().is_some() {
            Some(3)
        } else {
            Some(self.dimension())
        }
    }
    pub(crate) fn result_components(self) -> usize {
        if self.is_transpose() || self.is_matrix_product() {
            self.left_components()
        } else {
            self.dimension()
        }
    }
    pub(crate) fn fixed_cost(self) -> u32 {
        if self.is_transpose() {
            self.left_components() as u32 + 2
        } else if self.is_matrix_product() {
            2 * self.dimension().pow(3) as u32
        } else {
            2 * self.left_components() as u32
        }
    }
}
