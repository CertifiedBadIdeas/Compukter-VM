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

//! Logical checkpoint primitives. These never encode Rust layouts or addresses.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointError {
    Limit,
    Allocation,
    Truncated,
    InvalidState,
    TrailingBytes,
    Incompatible,
    Integrity,
}

pub(crate) type Result<T> = core::result::Result<T, CheckpointError>;

pub(crate) struct Writer {
    bytes: Vec<u8>,
    maximum: usize,
}

impl Writer {
    pub(crate) fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
        }
    }

    pub(crate) fn put(&mut self, bytes: &[u8]) -> Result<()> {
        let length = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(CheckpointError::Limit)?;
        if length > self.maximum {
            return Err(CheckpointError::Limit);
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| CheckpointError::Allocation)?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    allocation_left: usize,
    depth: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(
        bytes: &'a [u8],
        maximum_bytes: usize,
        maximum_allocation: usize,
    ) -> Result<Self> {
        if bytes.len() > maximum_bytes {
            return Err(CheckpointError::Limit);
        }
        Ok(Self {
            bytes,
            allocation_left: maximum_allocation,
            depth: 0,
        })
    }

    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let result = self.bytes.get(..length).ok_or(CheckpointError::Truncated)?;
        self.bytes = &self.bytes[length..];
        Ok(result)
    }

    pub(crate) fn allocate<T>(&mut self, length: usize) -> Result<()> {
        // Charge even zero-sized values, preventing a tiny input from requesting
        // an unbounded number of allocations or decode iterations.
        let bytes = length
            .checked_mul(core::mem::size_of::<T>().max(1))
            .ok_or(CheckpointError::Limit)?;
        self.allocation_left = self
            .allocation_left
            .checked_sub(bytes)
            .ok_or(CheckpointError::Limit)?;
        Ok(())
    }

    pub(crate) fn nested<T>(&mut self, read: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.depth == 64 {
            return Err(CheckpointError::Limit);
        }
        self.depth += 1;
        let result = read(self);
        self.depth -= 1;
        result
    }

    pub(crate) fn finish(self) -> Result<()> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(CheckpointError::TrailingBytes)
        }
    }
}

pub(crate) trait Checkpoint: Sized {
    fn write(&self, writer: &mut Writer) -> Result<()>;
    fn read(reader: &mut Reader<'_>) -> Result<Self>;
}

macro_rules! checkpoint_struct {
    ($ty:ident { $($field:ident),* $(,)? } $(defaults { $($(#[$attr:meta])* $extra:ident : $value:expr),* $(,)? })?) => {
        impl $crate::execution::checkpoint::Checkpoint for $ty {
            fn write(&self, writer: &mut $crate::execution::checkpoint::Writer) -> $crate::execution::checkpoint::Result<()> {
                $($crate::execution::checkpoint::Checkpoint::write(&self.$field, writer)?;)*
                Ok(())
            }
            fn read(reader: &mut $crate::execution::checkpoint::Reader<'_>) -> $crate::execution::checkpoint::Result<Self> {
                $(let $field = $crate::execution::checkpoint::Checkpoint::read(reader)?;)*
                $($($(#[$attr])* let $extra = $value;)*)?
                Ok(Self { $($field,)* $($($(#[$attr])* $extra,)*)? })
            }
        }
    };
}
pub(crate) use checkpoint_struct;

macro_rules! checkpoint_enum {
    ($ty:ident { $($tag:literal => $variant:ident $(($($tuple:ident),*))? $({$($field:ident),*})?;)* }) => {
        impl $crate::execution::checkpoint::Checkpoint for $ty {
            fn write(&self, writer: &mut $crate::execution::checkpoint::Writer) -> $crate::execution::checkpoint::Result<()> {
                match self {
                    $(Self::$variant $(($($tuple),*))? $({$($field),*})? => {
                        $crate::execution::checkpoint::Checkpoint::write(&($tag as u8), writer)?;
                        $($($crate::execution::checkpoint::Checkpoint::write($tuple, writer)?;)*)?
                        $($($crate::execution::checkpoint::Checkpoint::write($field, writer)?;)*)?
                    },)*
                }
                Ok(())
            }
            fn read(reader: &mut $crate::execution::checkpoint::Reader<'_>) -> $crate::execution::checkpoint::Result<Self> {
                reader.nested(|reader| match <u8 as $crate::execution::checkpoint::Checkpoint>::read(reader)? {
                    $($tag => Ok(Self::$variant $(( $( {let $tuple = $crate::execution::checkpoint::Checkpoint::read(reader)?; $tuple} ),* ))? $({ $($field: $crate::execution::checkpoint::Checkpoint::read(reader)?),* })?),)*
                    _ => Err($crate::execution::checkpoint::CheckpointError::InvalidState),
                })
            }
        }
    };
}
pub(crate) use checkpoint_enum;

macro_rules! integers {
    ($($ty:ty),*) => { $(
        impl Checkpoint for $ty {
            fn write(&self, writer: &mut Writer) -> Result<()> { writer.put(&self.to_le_bytes()) }
            fn read(reader: &mut Reader<'_>) -> Result<Self> {
                let bytes = reader.take(core::mem::size_of::<Self>())?;
                Ok(Self::from_le_bytes(bytes.try_into().map_err(|_| CheckpointError::Truncated)?))
            }
        }
    )* };
}
integers!(u8, u16, u32, u64, i32, i64);

impl Checkpoint for usize {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        // The internal absent-index sentinel has one portable representation.
        let value = if *self == usize::MAX {
            u64::MAX
        } else {
            *self as u64
        };
        value.write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        match u64::read(reader)? {
            u64::MAX => Ok(usize::MAX),
            value => usize::try_from(value).map_err(|_| CheckpointError::Limit),
        }
    }
}

impl Checkpoint for bool {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        u8::from(*self).write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        match u8::read(reader)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(CheckpointError::InvalidState),
        }
    }
}

impl<T: Checkpoint> Checkpoint for Option<T> {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.is_some().write(writer)?;
        if let Some(value) = self {
            value.write(writer)?;
        }
        Ok(())
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        reader.nested(|reader| {
            if bool::read(reader)? {
                Ok(Some(T::read(reader)?))
            } else {
                Ok(None)
            }
        })
    }
}

impl<T: Checkpoint> Checkpoint for Vec<T> {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.len().write(writer)?;
        for value in self {
            value.write(writer)?;
        }
        Ok(())
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        reader.nested(|reader| {
            let length = usize::read(reader)?;
            reader.allocate::<T>(length)?;
            let mut values = Vec::new();
            values
                .try_reserve_exact(length)
                .map_err(|_| CheckpointError::Allocation)?;
            for _ in 0..length {
                values.push(T::read(reader)?);
            }
            Ok(values)
        })
    }
}

impl<T: Checkpoint> Checkpoint for Box<[T]> {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.len().write(writer)?;
        for value in self {
            value.write(writer)?;
        }
        Ok(())
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Ok(Vec::<T>::read(reader)?.into_boxed_slice())
    }
}

impl<T: Checkpoint> Checkpoint for Box<T> {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.as_ref().write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        reader.allocate::<T>(1)?;
        reader.nested(|reader| Ok(Box::new(T::read(reader)?)))
    }
}

impl Checkpoint for String {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.len().write(writer)?;
        writer.put(self.as_bytes())
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        String::from_utf8(Vec::<u8>::read(reader)?).map_err(|_| CheckpointError::InvalidState)
    }
}

impl Checkpoint for Box<str> {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.len().write(writer)?;
        writer.put(self.as_bytes())
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Ok(String::read(reader)?.into_boxed_str())
    }
}

impl<T: Checkpoint> Checkpoint for std::collections::VecDeque<T> {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.len().write(writer)?;
        for value in self {
            value.write(writer)?;
        }
        Ok(())
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Ok(Vec::<T>::read(reader)?.into())
    }
}

impl<T: Checkpoint, const N: usize> Checkpoint for [T; N] {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        for value in self {
            value.write(writer)?;
        }
        Ok(())
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        reader.allocate::<T>(N)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(N)
            .map_err(|_| CheckpointError::Allocation)?;
        for _ in 0..N {
            values.push(T::read(reader)?);
        }
        values.try_into().map_err(|_| CheckpointError::InvalidState)
    }
}

impl<A: Checkpoint, B: Checkpoint> Checkpoint for (A, B) {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.0.write(writer)?;
        self.1.write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Ok((A::read(reader)?, B::read(reader)?))
    }
}

impl<A: Checkpoint, B: Checkpoint, C: Checkpoint> Checkpoint for (A, B, C) {
    fn write(&self, writer: &mut Writer) -> Result<()> {
        self.0.write(writer)?;
        self.1.write(writer)?;
        self.2.write(writer)
    }
    fn read(reader: &mut Reader<'_>) -> Result<Self> {
        Ok((A::read(reader)?, B::read(reader)?, C::read(reader)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_lengths_are_bounded_before_allocating() {
        let bytes = u64::MAX.to_le_bytes();
        let mut reader = Reader::new(&bytes, 8, 64).unwrap();
        assert_eq!(Err(CheckpointError::Limit), Vec::<u64>::read(&mut reader));
        let bytes = 9_u64.to_le_bytes();
        let mut reader = Reader::new(&bytes, 8, 64).unwrap();
        assert_eq!(Err(CheckpointError::Limit), Vec::<u64>::read(&mut reader));
    }

    #[test]
    fn checkpoint_primitives_reject_truncation_tags_and_trailing_data() {
        let mut reader = Reader::new(&[2], 1, 0).unwrap();
        assert_eq!(Err(CheckpointError::InvalidState), bool::read(&mut reader));
        let mut reader = Reader::new(&[1, 2, 3], 3, 0).unwrap();
        assert_eq!(Err(CheckpointError::Truncated), u32::read(&mut reader));
        assert_eq!(Err(CheckpointError::TrailingBytes), reader.finish());
        let mut writer = Writer::new(3);
        assert_eq!(Err(CheckpointError::Limit), 1_u32.write(&mut writer));
    }
}
