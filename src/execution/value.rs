#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReferenceValue {
    tagged_slot: u32,
    generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub(super) enum ReferenceDomain {
    Managed = 0,
    Literal = 1,
    Emergency = 2,
    Host = 3,
}

impl ReferenceValue {
    const DOMAIN_SHIFT: u32 = 30;
    pub(super) const MAX_SLOT: u32 = (1 << Self::DOMAIN_SHIFT) - 1;

    pub(super) const fn new(domain: ReferenceDomain, slot: u32, generation: u32) -> Option<Self> {
        if slot > Self::MAX_SLOT {
            return None;
        }
        Some(Self {
            tagged_slot: ((domain as u32) << Self::DOMAIN_SHIFT) | slot,
            generation,
        })
    }

    pub(super) const fn managed(slot: u32, generation: u32) -> Option<Self> {
        Self::new(ReferenceDomain::Managed, slot, generation)
    }

    pub(super) const fn literal(slot: u32) -> Option<Self> {
        Self::new(ReferenceDomain::Literal, slot, 0)
    }

    pub(super) const fn emergency() -> Self {
        Self {
            tagged_slot: (ReferenceDomain::Emergency as u32) << Self::DOMAIN_SHIFT,
            generation: 0,
        }
    }

    pub(super) const fn host(slot: u32, generation: u32) -> Option<Self> {
        Self::new(ReferenceDomain::Host, slot, generation)
    }

    pub(super) const fn domain(self) -> ReferenceDomain {
        match self.tagged_slot >> Self::DOMAIN_SHIFT {
            0 => ReferenceDomain::Managed,
            1 => ReferenceDomain::Literal,
            2 => ReferenceDomain::Emergency,
            _ => ReferenceDomain::Host,
        }
    }

    pub(super) const fn slot(self) -> u32 {
        self.tagged_slot & Self::MAX_SLOT
    }

    pub(super) const fn generation(self) -> u32 {
        self.generation
    }
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
            Self::Reference(value) => value.slot() as u64,
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
pub(super) struct EntryArgument {
    pub owner: Option<[u8; 32]>,
    pub value: RuntimeValue,
}

impl EntryArgument {
    pub(super) const fn unowned(value: RuntimeValue) -> Self {
        Self { owner: None, value }
    }

    pub(super) const fn owned(owner: [u8; 32], value: RuntimeValue) -> Self {
        Self {
            owner: Some(owner),
            value,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RegisterValue {
    Uninitialized,
    Initialized(RuntimeValue),
}
