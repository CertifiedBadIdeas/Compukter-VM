#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(super) struct Ref32(u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub(super) enum ReferenceDomain {
    Managed = 0,
    Image = 1,
    External = 2,
    Reserved = 3,
}

impl Ref32 {
    const DOMAIN_SHIFT: u32 = 30;
    pub(super) const MAX_PAYLOAD: u32 = (1 << Self::DOMAIN_SHIFT) - 1;

    const fn new(domain: ReferenceDomain, payload: u32) -> Option<Self> {
        if payload > Self::MAX_PAYLOAD {
            return None;
        }
        let bits = ((domain as u32) << Self::DOMAIN_SHIFT) | payload;
        if bits == 0 {
            return None;
        }
        Some(Self(bits))
    }

    pub(super) const fn managed(object_header_offset: u32) -> Option<Self> {
        Self::new(ReferenceDomain::Managed, object_header_offset)
    }

    pub(super) const fn image(index: u32) -> Option<Self> {
        Self::new(ReferenceDomain::Image, index)
    }

    pub(super) const fn external(slot: u32) -> Option<Self> {
        Self::new(ReferenceDomain::External, slot)
    }

    pub(super) const fn reserved(payload: u32) -> Option<Self> {
        Self::new(ReferenceDomain::Reserved, payload)
    }

    pub(super) const fn domain(self) -> ReferenceDomain {
        match self.0 >> Self::DOMAIN_SHIFT {
            0 => ReferenceDomain::Managed,
            1 => ReferenceDomain::Image,
            2 => ReferenceDomain::External,
            _ => ReferenceDomain::Reserved,
        }
    }

    pub(super) const fn payload(self) -> u32 {
        self.0 & Self::MAX_PAYLOAD
    }

    pub(super) const fn to_bits(self) -> u32 {
        self.0
    }

    pub(super) const fn from_bits(bits: u32) -> Option<Self> {
        if bits == 0 {
            None
        } else {
            Some(Self(bits))
        }
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
    Reference(Ref32),
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
            Self::Reference(value) => value.payload() as u64,
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
            Self::Reference(_) => 12,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct EntryArgument {
    pub owner: Option<[u8; 32]>,
    pub value: RuntimeValue,
    pub external_handle: Option<super::external_roots::ExternalHandle>,
}

impl EntryArgument {
    pub(super) const fn unowned(value: RuntimeValue) -> Self {
        Self {
            owner: None,
            value,
            external_handle: None,
        }
    }

    pub(super) const fn owned(owner: [u8; 32], value: RuntimeValue) -> Self {
        Self {
            owner: Some(owner),
            value,
            external_handle: None,
        }
    }

    pub(super) const fn owned_external(
        owner: [u8; 32],
        value: Ref32,
        handle: super::external_roots::ExternalHandle,
    ) -> Self {
        Self {
            owner: Some(owner),
            value: RuntimeValue::Reference(value),
            external_handle: Some(handle),
        }
    }
}
