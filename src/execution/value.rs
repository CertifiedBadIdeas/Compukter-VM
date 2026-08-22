use super::TypeKey;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReferenceValue {
    pub image: [u8; 32],
    pub ty: TypeKey,
    pub handle: u32,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RuntimeValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(u16),
    Null,
    Reference(ReferenceValue),
}

impl RuntimeValue {
    pub(super) fn trace_bits_u64(self) -> u64 {
        match self {
            Self::I32(value) => value as u32 as u64,
            Self::I64(value) => value as u64,
            Self::F32(bits) => bits as u64,
            Self::F64(bits) => bits,
            Self::Bool(value) => u64::from(value),
            Self::Char(value) => u64::from(value),
            Self::Null => 0,
            Self::Reference(value) => value.handle as u64,
        }
    }

    pub(super) fn trace_tag(self) -> u8 {
        match self {
            Self::I32(_) => 1,
            Self::I64(_) => 2,
            Self::F32(_) => 3,
            Self::F64(_) => 4,
            Self::Bool(_) => 5,
            Self::Char(_) => 6,
            Self::Null => 7,
            Self::Reference(_) => 8,
        }
    }

    pub(super) fn trace_payload_len(self) -> u32 {
        match self {
            Self::I32(_) | Self::F32(_) => 4,
            Self::Char(_) => 2,
            Self::I64(_) | Self::F64(_) => 8,
            Self::Bool(_) => 1,
            Self::Null => 0,
            Self::Reference(_) => 16,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct EntryArgument(pub RuntimeValue);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RegisterValue {
    Uninitialized,
    Initialized(RuntimeValue),
}
