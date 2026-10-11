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
}

impl MatrixOperation {
    pub(crate) fn decode(selector: u32) -> Option<Self> {
        Some(match selector {
            1 => Self::Transpose,
            2 => Self::MultiplyMatrix,
            3 => Self::MultiplyVector,
            4 => Self::TransformPoint,
            5 => Self::TransformDirection,
            _ => return None,
        })
    }
    pub(crate) fn right_components(self) -> Option<usize> {
        match self {
            Self::Transpose => None,
            Self::MultiplyMatrix => Some(16),
            Self::MultiplyVector => Some(4),
            Self::TransformPoint | Self::TransformDirection => Some(3),
        }
    }
    pub(crate) fn result_components(self) -> usize {
        match self {
            Self::Transpose | Self::MultiplyMatrix => 16,
            _ => 4,
        }
    }
    pub(crate) fn fixed_cost(self) -> u32 {
        match self {
            Self::Transpose => 18,
            Self::MultiplyMatrix => 128,
            _ => 32,
        }
    }
}
