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
    Char(char),
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
            Self::Char(value) => value as u32 as u64,
            Self::Null => 0,
            Self::Reference(value) => value.handle as u64,
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
