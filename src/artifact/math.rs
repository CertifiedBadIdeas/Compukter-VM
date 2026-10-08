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
pub(crate) enum MathUnaryOperation {
    Abs = 1,
    Sign = 2,
    Ceil = 3,
    Floor = 4,
    Truncate = 5,
    Round = 6,
    Sin = 7,
    Cos = 8,
    Tan = 9,
    Asin = 10,
    Acos = 11,
    Atan = 12,
    Sinh = 13,
    Cosh = 14,
    Tanh = 15,
    Asinh = 16,
    Acosh = 17,
    Atanh = 18,
    Sqrt = 19,
    Cbrt = 20,
    Exp = 21,
    Expm1 = 22,
    Ln = 23,
    Ln1p = 24,
    Log10 = 25,
    Log2 = 26,
    Ulp = 27,
    NextUp = 28,
    NextDown = 29,
}

impl MathUnaryOperation {
    pub(crate) fn decode(selector: u32) -> Option<Self> {
        Some(match selector {
            1 => Self::Abs,
            2 => Self::Sign,
            3 => Self::Ceil,
            4 => Self::Floor,
            5 => Self::Truncate,
            6 => Self::Round,
            7 => Self::Sin,
            8 => Self::Cos,
            9 => Self::Tan,
            10 => Self::Asin,
            11 => Self::Acos,
            12 => Self::Atan,
            13 => Self::Sinh,
            14 => Self::Cosh,
            15 => Self::Tanh,
            16 => Self::Asinh,
            17 => Self::Acosh,
            18 => Self::Atanh,
            19 => Self::Sqrt,
            20 => Self::Cbrt,
            21 => Self::Exp,
            22 => Self::Expm1,
            23 => Self::Ln,
            24 => Self::Ln1p,
            25 => Self::Log10,
            26 => Self::Log2,
            27 => Self::Ulp,
            28 => Self::NextUp,
            29 => Self::NextDown,
            _ => return None,
        })
    }
    pub(crate) fn fixed_cost(self) -> u32 {
        match self {
            Self::Abs => 2,
            Self::Sign => 2,
            Self::Ceil => 4,
            Self::Floor => 4,
            Self::Truncate => 4,
            Self::Round => 4,
            Self::Sin => 64,
            Self::Cos => 64,
            Self::Tan => 64,
            Self::Asin => 64,
            Self::Acos => 64,
            Self::Atan => 64,
            Self::Sinh => 64,
            Self::Cosh => 64,
            Self::Tanh => 64,
            Self::Asinh => 64,
            Self::Acosh => 64,
            Self::Atanh => 64,
            Self::Sqrt => 16,
            Self::Cbrt => 64,
            Self::Exp => 64,
            Self::Expm1 => 64,
            Self::Ln => 64,
            Self::Ln1p => 64,
            Self::Log10 => 64,
            Self::Log2 => 64,
            Self::Ulp => 4,
            Self::NextUp => 4,
            Self::NextDown => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MathBinaryOperation {
    Min = 1,
    Max = 2,
    Atan2 = 3,
    Hypot = 4,
    Pow = 5,
    IeeeRem = 6,
    CopySign = 7,
    NextTowards = 8,
}

impl MathBinaryOperation {
    pub(crate) fn decode(selector: u32) -> Option<Self> {
        Some(match selector {
            1 => Self::Min,
            2 => Self::Max,
            3 => Self::Atan2,
            4 => Self::Hypot,
            5 => Self::Pow,
            6 => Self::IeeeRem,
            7 => Self::CopySign,
            8 => Self::NextTowards,
            _ => return None,
        })
    }
    pub(crate) fn fixed_cost(self) -> u32 {
        match self {
            Self::Min => 2,
            Self::Max => 2,
            Self::Atan2 => 64,
            Self::Hypot => 32,
            Self::Pow => 64,
            Self::IeeeRem => 64,
            Self::CopySign => 2,
            Self::NextTowards => 4,
        }
    }
}
