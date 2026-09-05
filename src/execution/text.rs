use super::{
    error::{GuestTrap, VmFault},
    heap::{AllocationRequest, Heap, ReservedAllocation},
    image::{ExecutionImage, ResolvedLiteral},
    layout::{string_layout, StringEncoding, StringLayout},
    value::{Ref32, RuntimeValue},
};

#[derive(Clone, Copy, Debug)]
pub(super) enum StringBacking {
    Inline {
        units: [u16; 11],
        start: u8,
        length: u8,
    },
    Literal(ResolvedLiteral),
    Managed {
        reference: Ref32,
        length: u32,
        encoding: StringEncoding,
    },
    CharArray {
        reference: Ref32,
        length: u32,
    },
}

impl StringBacking {
    fn length(self) -> u32 {
        match self {
            Self::Inline { length, .. } => u32::from(length),
            Self::Literal(literal) => literal.code_units,
            Self::Managed { length, .. } => length,
            Self::CharArray { length, .. } => length,
        }
    }

    fn visit_root(self, visit: &mut impl FnMut(Ref32)) {
        match self {
            Self::Managed { reference, .. } | Self::CharArray { reference, .. } => visit(reference),
            Self::Inline { .. } | Self::Literal(_) => {}
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum PendingText {
    Hash {
        value: StringBacking,
        index: u32,
        hash: i32,
        destination: u16,
    },
    Equals {
        lhs: StringBacking,
        rhs: StringBacking,
        index: u32,
        destination: u16,
    },
    Compare {
        lhs: StringBacking,
        rhs: StringBacking,
        index: u32,
        destination: u16,
    },
}

impl PendingText {
    pub(super) fn visit_roots(self, mut visit: impl FnMut(Ref32)) {
        match self {
            Self::Hash { value, .. } => value.visit_root(&mut visit),
            Self::Equals { lhs, rhs, .. } | Self::Compare { lhs, rhs, .. } => {
                lhs.visit_root(&mut visit);
                rhs.visit_root(&mut visit);
            }
        }
    }

    pub(super) fn hash(
        image: &ExecutionImage,
        heap: &Heap,
        value: RuntimeValue,
        destination: u16,
    ) -> Result<Self, TextError> {
        Ok(Self::Hash {
            value: backing(image, heap, value)?,
            index: 0,
            hash: 0,
            destination,
        })
    }

    pub(super) fn equals(
        image: &ExecutionImage,
        heap: &Heap,
        lhs: RuntimeValue,
        rhs: RuntimeValue,
        destination: u16,
    ) -> Result<Self, TextError> {
        Ok(Self::Equals {
            lhs: backing(image, heap, lhs)?,
            rhs: backing(image, heap, rhs)?,
            index: 0,
            destination,
        })
    }

    pub(super) fn compare(
        image: &ExecutionImage,
        heap: &Heap,
        lhs: RuntimeValue,
        rhs: RuntimeValue,
        destination: u16,
    ) -> Result<Self, TextError> {
        Ok(Self::Compare {
            lhs: backing(image, heap, lhs)?,
            rhs: backing(image, heap, rhs)?,
            index: 0,
            destination,
        })
    }

    pub(super) fn resume(
        &mut self,
        image: &ExecutionImage,
        heap: &Heap,
        budget: u32,
    ) -> Result<(u32, Option<(u16, RuntimeValue)>), TextError> {
        let mut used = 0;
        match self {
            Self::Hash {
                value,
                index,
                hash,
                destination,
            } => {
                while *index < value.length() && used < budget {
                    let end = index.saturating_add(8).min(value.length());
                    while *index < end {
                        *hash = hash
                            .wrapping_mul(31)
                            .wrapping_add(i32::from(code_unit(image, heap, *value, *index)?));
                        *index += 1;
                    }
                    used += 1;
                }
                Ok((
                    used,
                    (*index == value.length()).then_some((*destination, RuntimeValue::I32(*hash))),
                ))
            }
            Self::Equals {
                lhs,
                rhs,
                index,
                destination,
            } => {
                if lhs.length() != rhs.length() {
                    return Ok((0, Some((*destination, RuntimeValue::Bool(false)))));
                }
                while *index < lhs.length() && used < budget {
                    let end = index.saturating_add(8).min(lhs.length());
                    while *index < end {
                        if code_unit(image, heap, *lhs, *index)?
                            != code_unit(image, heap, *rhs, *index)?
                        {
                            return Ok((used + 1, Some((*destination, RuntimeValue::Bool(false)))));
                        }
                        *index += 1;
                    }
                    used += 1;
                }
                Ok((
                    used,
                    (*index == lhs.length()).then_some((*destination, RuntimeValue::Bool(true))),
                ))
            }
            Self::Compare {
                lhs,
                rhs,
                index,
                destination,
            } => {
                let common = lhs.length().min(rhs.length());
                while *index < common && used < budget {
                    let end = index.saturating_add(8).min(common);
                    while *index < end {
                        let left = code_unit(image, heap, *lhs, *index)?;
                        let right = code_unit(image, heap, *rhs, *index)?;
                        if left != right {
                            return Ok((
                                used + 1,
                                Some((
                                    *destination,
                                    RuntimeValue::I32(i32::from(left) - i32::from(right)),
                                )),
                            ));
                        }
                        *index += 1;
                    }
                    used += 1;
                }
                Ok((
                    used,
                    (*index == common).then_some((
                        *destination,
                        RuntimeValue::I32(lhs.length() as i32 - rhs.length() as i32),
                    )),
                ))
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PendingConcat {
    lhs: StringBacking,
    lhs_start: u32,
    lhs_length: u32,
    rhs: StringBacking,
    rhs_start: u32,
    rhs_length: u32,
    destination: u16,
    scan: u32,
    latin1: bool,
    reservation: Option<ReservedAllocation>,
    layout: Option<StringLayout>,
    written: u32,
    collection_attempted: bool,
}

impl PendingConcat {
    pub(super) fn visit_roots(self, mut visit: impl FnMut(Ref32)) {
        self.lhs.visit_root(&mut visit);
        self.rhs.visit_root(&mut visit);
    }

    pub(super) fn scalar(
        value: RuntimeValue,
        form: u8,
        destination: u16,
    ) -> Result<Self, TextError> {
        let (units, start, length) = scalar_units(value, form)?;
        let source = StringBacking::Inline {
            units,
            start,
            length,
        };
        Ok(Self {
            lhs: source,
            lhs_start: 0,
            lhs_length: u32::from(length),
            rhs: StringBacking::Inline {
                units: [0; 11],
                start: 0,
                length: 0,
            },
            rhs_start: 0,
            rhs_length: 0,
            destination,
            scan: 0,
            latin1: true,
            reservation: None,
            layout: None,
            written: 0,
            collection_attempted: false,
        })
    }

    pub(super) fn new(
        image: &ExecutionImage,
        heap: &Heap,
        lhs: RuntimeValue,
        rhs: RuntimeValue,
        destination: u16,
    ) -> Result<Self, TextError> {
        let lhs = backing(image, heap, lhs)?;
        let rhs = backing(image, heap, rhs)?;
        lhs.length()
            .checked_add(rhs.length())
            .ok_or(TextError::Fault(VmFault::AccountingOverflow))?;
        Ok(Self {
            lhs,
            lhs_start: 0,
            lhs_length: lhs.length(),
            rhs,
            rhs_start: 0,
            rhs_length: rhs.length(),
            destination,
            scan: 0,
            latin1: true,
            reservation: None,
            layout: None,
            written: 0,
            collection_attempted: false,
        })
    }

    pub(super) fn substring(
        image: &ExecutionImage,
        heap: &Heap,
        value: RuntimeValue,
        start: i32,
        end: i32,
        destination: u16,
    ) -> Result<SubstringPlan, TextError> {
        let source = backing(image, heap, value)?;
        let length = i32::try_from(source.length())
            .map_err(|_| TextError::Fault(VmFault::InvalidReference))?;
        if start < 0 || end < start || end > length {
            return Err(TextError::Trap(GuestTrap::IndexOutOfBounds));
        }
        if start == 0 && end == length {
            return Ok(SubstringPlan::Identity(value));
        }
        if start == end {
            return Ok(SubstringPlan::Empty);
        }
        Ok(SubstringPlan::Build(Self {
            lhs: source,
            lhs_start: start as u32,
            lhs_length: (end - start) as u32,
            rhs: source,
            rhs_start: 0,
            rhs_length: 0,
            destination,
            scan: 0,
            latin1: true,
            reservation: None,
            layout: None,
            written: 0,
            collection_attempted: false,
        }))
    }

    pub(super) fn char_array(
        reference: Ref32,
        length: i32,
        start: i32,
        end: i32,
        destination: u16,
    ) -> Result<Self, TextError> {
        if start < 0 || end < start || end > length {
            return Err(TextError::Trap(GuestTrap::IndexOutOfBounds));
        }
        let source = StringBacking::CharArray {
            reference,
            length: length as u32,
        };
        Ok(Self {
            lhs: source,
            lhs_start: start as u32,
            lhs_length: (end - start) as u32,
            rhs: source,
            rhs_start: 0,
            rhs_length: 0,
            destination,
            scan: 0,
            latin1: true,
            reservation: None,
            layout: None,
            written: 0,
            collection_attempted: false,
        })
    }

    pub(super) fn resume(
        &mut self,
        image: &ExecutionImage,
        heap: &mut Heap,
        budget: u32,
    ) -> Result<(u32, Option<(u16, RuntimeValue)>), TextError> {
        let length = self.lhs_length + self.rhs_length;
        let mut used = 0;
        while self.scan < length && used < budget {
            let end = self.scan.saturating_add(8).min(length);
            while self.scan < end {
                self.latin1 &= self.source_unit(image, heap, self.scan)? <= 0xff;
                self.scan += 1;
            }
            used += 1;
        }
        if self.scan < length {
            return Ok((used, None));
        }
        if self.reservation.is_none() {
            let encoding = if self.latin1 {
                StringEncoding::Latin1
            } else {
                StringEncoding::Utf16
            };
            let layout = string_layout(encoding, length)
                .map_err(|_| TextError::Fault(VmFault::AccountingOverflow))?;
            let ty = image
                .string_type()
                .ok_or(TextError::Fault(VmFault::InvalidResolvedId))?;
            self.reservation = match heap.reserve(AllocationRequest {
                block_bytes: layout.block_bytes,
                type_id: image
                    .type_id(ty)
                    .ok_or(TextError::Fault(VmFault::InvalidResolvedId))?,
            }) {
                Ok(reservation) => reservation,
                Err(fault) => return Err(TextError::Fault(fault)),
            };
            if self.reservation.is_none() {
                return Err(TextError::Exhausted {
                    used,
                    block_bytes: layout.block_bytes,
                    requested: layout.payload_bytes,
                    collection_attempted: self.collection_attempted,
                });
            }
            self.layout = Some(layout);
        }
        let layout = self
            .layout
            .ok_or(TextError::Fault(VmFault::CorruptLifecycle))?;
        let reservation = self
            .reservation
            .ok_or(TextError::Fault(VmFault::CorruptLifecycle))?;
        let initialized_bytes = layout.block_bytes - 24;
        while self.written < initialized_bytes && used < budget {
            let end = self.written.saturating_add(16).min(initialized_bytes);
            let mut chunk = [0_u8; 16];
            for offset in self.written..end {
                chunk[(offset - self.written) as usize] = if offset >= layout.payload_bytes {
                    0
                } else if offset < 4 {
                    length.to_le_bytes()[offset as usize]
                } else if offset == 4 {
                    u8::from(layout.encoding == StringEncoding::Utf16)
                } else if offset < 8 {
                    0
                } else {
                    let data = offset - 8;
                    match layout.encoding {
                        StringEncoding::Latin1 => self.source_unit(image, heap, data)? as u8,
                        StringEncoding::Utf16 => {
                            let unit = self.source_unit(image, heap, data / 2)?.to_le_bytes();
                            unit[(data % 2) as usize]
                        }
                    }
                };
            }
            heap.write_reserved(
                reservation,
                self.written,
                &chunk[..(end - self.written) as usize],
            )
            .map_err(TextError::Fault)?;
            self.written = end;
            used += 1;
        }
        if self.written < initialized_bytes {
            return Ok((used, None));
        }
        let reference = heap.commit(reservation).map_err(TextError::Fault)?;
        self.reservation = None;
        Ok((
            used,
            Some((self.destination, RuntimeValue::Reference(reference))),
        ))
    }

    pub(super) fn abort(self, heap: &mut Heap) -> Result<(), VmFault> {
        if let Some(reservation) = self.reservation {
            heap.abort(reservation)?;
        }
        Ok(())
    }

    pub(super) fn mark_collection_attempted(&mut self) {
        self.collection_attempted = true;
    }

    fn source_unit(
        &self,
        image: &ExecutionImage,
        heap: &Heap,
        index: u32,
    ) -> Result<u16, TextError> {
        if index < self.lhs_length {
            code_unit(image, heap, self.lhs, self.lhs_start + index)
        } else {
            code_unit(
                image,
                heap,
                self.rhs,
                self.rhs_start + index - self.lhs_length,
            )
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PendingHostString {
    destination: u16,
    scan: u32,
    latin1: bool,
    reservation: Option<ReservedAllocation>,
    layout: Option<StringLayout>,
    written: u32,
    collection_attempted: bool,
}

impl PendingHostString {
    pub(super) const fn new(destination: u16) -> Self {
        Self {
            destination,
            scan: 0,
            latin1: true,
            reservation: None,
            layout: None,
            written: 0,
            collection_attempted: false,
        }
    }

    pub(super) fn resume(
        &mut self,
        image: &ExecutionImage,
        heap: &mut Heap,
        source: &[u16],
        budget: u32,
    ) -> Result<(u32, Option<(u16, RuntimeValue)>), TextError> {
        let length = u32::try_from(source.len())
            .map_err(|_| TextError::Fault(VmFault::AccountingOverflow))?;
        let mut used = 0;
        while self.scan < length && used < budget {
            let end = self.scan.saturating_add(8).min(length);
            while self.scan < end {
                self.latin1 &= source[self.scan as usize] <= 0xff;
                self.scan += 1;
            }
            used += 1;
        }
        if self.scan < length {
            return Ok((used, None));
        }
        if self.reservation.is_none() {
            let encoding = if self.latin1 {
                StringEncoding::Latin1
            } else {
                StringEncoding::Utf16
            };
            let layout = string_layout(encoding, length)
                .map_err(|_| TextError::Fault(VmFault::AccountingOverflow))?;
            let ty = image
                .string_type()
                .ok_or(TextError::Fault(VmFault::InvalidResolvedId))?;
            self.reservation = match heap.reserve(AllocationRequest {
                block_bytes: layout.block_bytes,
                type_id: image
                    .type_id(ty)
                    .ok_or(TextError::Fault(VmFault::InvalidResolvedId))?,
            }) {
                Ok(reservation) => reservation,
                Err(fault) => return Err(TextError::Fault(fault)),
            };
            if self.reservation.is_none() {
                return Err(TextError::Exhausted {
                    used,
                    block_bytes: layout.block_bytes,
                    requested: layout.payload_bytes,
                    collection_attempted: self.collection_attempted,
                });
            }
            self.layout = Some(layout);
        }
        let layout = self
            .layout
            .ok_or(TextError::Fault(VmFault::CorruptLifecycle))?;
        let reservation = self
            .reservation
            .ok_or(TextError::Fault(VmFault::CorruptLifecycle))?;
        let initialized_bytes = layout.block_bytes - 24;
        while self.written < initialized_bytes && used < budget {
            let end = self.written.saturating_add(16).min(initialized_bytes);
            let mut chunk = [0_u8; 16];
            for offset in self.written..end {
                chunk[(offset - self.written) as usize] = if offset >= layout.payload_bytes {
                    0
                } else if offset < 4 {
                    length.to_le_bytes()[offset as usize]
                } else if offset == 4 {
                    u8::from(layout.encoding == StringEncoding::Utf16)
                } else if offset < 8 {
                    0
                } else {
                    let data = offset - 8;
                    match layout.encoding {
                        StringEncoding::Latin1 => source[data as usize] as u8,
                        StringEncoding::Utf16 => {
                            let unit = source[(data / 2) as usize].to_le_bytes();
                            unit[(data % 2) as usize]
                        }
                    }
                };
            }
            heap.write_reserved(
                reservation,
                self.written,
                &chunk[..(end - self.written) as usize],
            )
            .map_err(TextError::Fault)?;
            self.written = end;
            used += 1;
        }
        if self.written < initialized_bytes {
            return Ok((used, None));
        }
        let reference = heap.commit(reservation).map_err(TextError::Fault)?;
        self.reservation = None;
        Ok((
            used,
            Some((self.destination, RuntimeValue::Reference(reference))),
        ))
    }

    pub(super) fn abort(self, heap: &mut Heap) -> Result<(), VmFault> {
        if let Some(reservation) = self.reservation {
            heap.abort(reservation)?;
        }
        Ok(())
    }

    pub(super) fn mark_collection_attempted(&mut self) {
        self.collection_attempted = true;
    }
}

pub(super) enum SubstringPlan {
    Identity(RuntimeValue),
    Empty,
    Build(PendingConcat),
}

pub(super) enum TextError {
    Trap(GuestTrap),
    Fault(VmFault),
    Exhausted {
        used: u32,
        block_bytes: u32,
        requested: u32,
        collection_attempted: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Utf8Error {
    Invalid,
    InsufficientCapacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConversionStatus {
    Pending,
    Complete(usize),
    Failed(Utf8Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ConversionStep {
    pub units: u32,
    pub status: ConversionStatus,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Utf16ToUtf8Cursor {
    input: usize,
    output: usize,
    strict: bool,
    terminal: bool,
}

impl Utf16ToUtf8Cursor {
    pub(super) const fn new(strict: bool) -> Self {
        Self {
            input: 0,
            output: 0,
            strict,
            terminal: false,
        }
    }

    pub(super) fn step(
        &mut self,
        input: &[u16],
        unpublished_output: &mut [u8],
        budget: u32,
    ) -> ConversionStep {
        if self.terminal {
            return ConversionStep {
                units: 0,
                status: ConversionStatus::Failed(Utf8Error::Invalid),
            };
        }
        let mut units = 0;
        while self.input < input.len() && units < budget {
            let chunk_start = self.input;
            while self.input - chunk_start < 8 && self.input < input.len() {
                let first = input[self.input];
                let (scalar, consumed) = if (0xd800..=0xdbff).contains(&first)
                    && input
                        .get(self.input + 1)
                        .is_some_and(|low| (0xdc00..=0xdfff).contains(low))
                {
                    if self.input - chunk_start == 7 {
                        break;
                    }
                    let low = input[self.input + 1];
                    (
                        char::from_u32(
                            0x1_0000
                                + ((u32::from(first) - 0xd800) << 10)
                                + (u32::from(low) - 0xdc00),
                        )
                        .unwrap(),
                        2,
                    )
                } else if (0xd800..=0xdfff).contains(&first) {
                    if self.strict {
                        self.terminal = true;
                        return ConversionStep {
                            units: units + 1,
                            status: ConversionStatus::Failed(Utf8Error::Invalid),
                        };
                    }
                    ('\u{fffd}', 1)
                } else {
                    (char::from_u32(u32::from(first)).unwrap(), 1)
                };
                let mut encoded = [0; 4];
                let bytes = scalar.encode_utf8(&mut encoded).as_bytes();
                let Some(end) = self.output.checked_add(bytes.len()) else {
                    self.terminal = true;
                    return ConversionStep {
                        units: units + 1,
                        status: ConversionStatus::Failed(Utf8Error::InsufficientCapacity),
                    };
                };
                let Some(destination) = unpublished_output.get_mut(self.output..end) else {
                    self.terminal = true;
                    return ConversionStep {
                        units: units + 1,
                        status: ConversionStatus::Failed(Utf8Error::InsufficientCapacity),
                    };
                };
                destination.copy_from_slice(bytes);
                self.output = end;
                self.input += consumed;
            }
            units += 1;
        }
        if self.input == input.len() {
            self.terminal = true;
            ConversionStep {
                units,
                status: ConversionStatus::Complete(self.output),
            }
        } else {
            ConversionStep {
                units,
                status: ConversionStatus::Pending,
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Utf8ToUtf16Cursor {
    input: usize,
    output: usize,
    strict: bool,
    terminal: bool,
}

impl Utf8ToUtf16Cursor {
    pub(super) const fn new(strict: bool) -> Self {
        Self {
            input: 0,
            output: 0,
            strict,
            terminal: false,
        }
    }

    pub(super) fn step(
        &mut self,
        input: &[u8],
        unpublished_output: &mut [u16],
        budget: u32,
    ) -> ConversionStep {
        if self.terminal {
            return ConversionStep {
                units: 0,
                status: ConversionStatus::Failed(Utf8Error::Invalid),
            };
        }
        let mut units = 0;
        while self.input < input.len() && units < budget {
            let chunk_start = self.output;
            while self.output - chunk_start < 8 && self.input < input.len() {
                let (scalar, consumed) = match decode_utf8_scalar(&input[self.input..]) {
                    Ok(decoded) => decoded,
                    Err(_invalid) if self.strict => {
                        self.terminal = true;
                        return ConversionStep {
                            units: units + 1,
                            status: ConversionStatus::Failed(Utf8Error::Invalid),
                        };
                    }
                    Err(invalid) => ('\u{fffd}', invalid),
                };
                let width = scalar.len_utf16();
                if self.output - chunk_start + width > 8 {
                    break;
                }
                let Some(end) = self.output.checked_add(width) else {
                    self.terminal = true;
                    return ConversionStep {
                        units: units + 1,
                        status: ConversionStatus::Failed(Utf8Error::InsufficientCapacity),
                    };
                };
                let Some(destination) = unpublished_output.get_mut(self.output..end) else {
                    self.terminal = true;
                    return ConversionStep {
                        units: units + 1,
                        status: ConversionStatus::Failed(Utf8Error::InsufficientCapacity),
                    };
                };
                scalar.encode_utf16(destination);
                self.output = end;
                self.input += consumed;
            }
            units += 1;
        }
        if self.input == input.len() {
            self.terminal = true;
            ConversionStep {
                units,
                status: ConversionStatus::Complete(self.output),
            }
        } else {
            ConversionStep {
                units,
                status: ConversionStatus::Pending,
            }
        }
    }
}

fn decode_utf8_scalar(input: &[u8]) -> Result<(char, usize), usize> {
    match core::str::from_utf8(input) {
        Ok(valid) => {
            let scalar = valid.chars().next().unwrap();
            Ok((scalar, scalar.len_utf8()))
        }
        Err(error) if error.valid_up_to() != 0 => {
            let valid = core::str::from_utf8(&input[..error.valid_up_to()]).unwrap();
            let scalar = valid.chars().next().unwrap();
            Ok((scalar, scalar.len_utf8()))
        }
        Err(error) => Err(error.error_len().unwrap_or(input.len()).max(1)),
    }
}

pub(super) fn utf16_to_utf8(
    input: &[u16],
    output: &mut [u8],
    strict: bool,
) -> Result<usize, Utf8Error> {
    let required = walk_utf16(input, strict, |_| {})?;
    if output.len() < required {
        return Err(Utf8Error::InsufficientCapacity);
    }
    let mut written = 0;
    walk_utf16(input, strict, |scalar| {
        let mut bytes = [0; 4];
        let encoded = scalar.encode_utf8(&mut bytes).as_bytes();
        output[written..written + encoded.len()].copy_from_slice(encoded);
        written += encoded.len();
    })?;
    Ok(written)
}

pub(super) fn utf8_to_utf16(
    input: &[u8],
    output: &mut [u16],
    strict: bool,
) -> Result<usize, Utf8Error> {
    let required = walk_utf8(input, strict, |_| {})?;
    if output.len() < required {
        return Err(Utf8Error::InsufficientCapacity);
    }
    let mut written = 0;
    walk_utf8(input, strict, |scalar| {
        let mut units = [0; 2];
        let encoded = scalar.encode_utf16(&mut units);
        output[written..written + encoded.len()].copy_from_slice(encoded);
        written += encoded.len();
    })?;
    Ok(written)
}

fn walk_utf16(input: &[u16], strict: bool, mut emit: impl FnMut(char)) -> Result<usize, Utf8Error> {
    let mut index = 0;
    let mut bytes = 0_usize;
    while index < input.len() {
        let unit = input[index];
        let scalar = if (0xd800..=0xdbff).contains(&unit)
            && input
                .get(index + 1)
                .is_some_and(|low| (0xdc00..=0xdfff).contains(low))
        {
            let low = input[index + 1];
            index += 2;
            char::from_u32(
                0x1_0000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00),
            )
            .unwrap()
        } else if (0xd800..=0xdfff).contains(&unit) {
            if strict {
                return Err(Utf8Error::Invalid);
            }
            index += 1;
            '\u{fffd}'
        } else {
            index += 1;
            char::from_u32(u32::from(unit)).unwrap()
        };
        bytes = bytes
            .checked_add(scalar.len_utf8())
            .ok_or(Utf8Error::InsufficientCapacity)?;
        emit(scalar);
    }
    Ok(bytes)
}

fn walk_utf8(
    mut input: &[u8],
    strict: bool,
    mut emit: impl FnMut(char),
) -> Result<usize, Utf8Error> {
    let mut units = 0_usize;
    while !input.is_empty() {
        match core::str::from_utf8(input) {
            Ok(valid) => {
                for scalar in valid.chars() {
                    units = units
                        .checked_add(scalar.len_utf16())
                        .ok_or(Utf8Error::InsufficientCapacity)?;
                    emit(scalar);
                }
                break;
            }
            Err(error) => {
                let valid = core::str::from_utf8(&input[..error.valid_up_to()]).unwrap();
                for scalar in valid.chars() {
                    units = units
                        .checked_add(scalar.len_utf16())
                        .ok_or(Utf8Error::InsufficientCapacity)?;
                    emit(scalar);
                }
                if strict {
                    return Err(Utf8Error::Invalid);
                }
                units = units
                    .checked_add(1)
                    .ok_or(Utf8Error::InsufficientCapacity)?;
                emit('\u{fffd}');
                let invalid = error
                    .error_len()
                    .unwrap_or(input.len() - error.valid_up_to());
                input = &input[error.valid_up_to() + invalid..];
            }
        }
    }
    Ok(units)
}

pub(super) fn length(
    image: &ExecutionImage,
    heap: &Heap,
    value: RuntimeValue,
) -> Result<i32, TextError> {
    i32::try_from(backing(image, heap, value)?.length())
        .map_err(|_| TextError::Fault(VmFault::InvalidReference))
}

pub(super) fn get(
    image: &ExecutionImage,
    heap: &Heap,
    value: RuntimeValue,
    index: i32,
) -> Result<u16, TextError> {
    if index < 0 {
        return Err(TextError::Trap(GuestTrap::IndexOutOfBounds));
    }
    let value = backing(image, heap, value)?;
    if index as u32 >= value.length() {
        return Err(TextError::Trap(GuestTrap::IndexOutOfBounds));
    }
    code_unit(image, heap, value, index as u32)
}

#[cfg(test)]
pub(super) fn encoding(
    image: &ExecutionImage,
    heap: &Heap,
    value: RuntimeValue,
) -> Result<Option<StringEncoding>, TextError> {
    Ok(match backing(image, heap, value)? {
        StringBacking::Inline { .. } => {
            return Err(TextError::Fault(VmFault::InvalidReference));
        }
        StringBacking::Literal(_) => None,
        StringBacking::Managed { encoding, .. } => Some(encoding),
        StringBacking::CharArray { .. } => {
            return Err(TextError::Fault(VmFault::InvalidReference));
        }
    })
}

fn backing(
    image: &ExecutionImage,
    heap: &Heap,
    value: RuntimeValue,
) -> Result<StringBacking, TextError> {
    let RuntimeValue::Reference(reference) = value else {
        return if value == RuntimeValue::Null {
            Err(TextError::Trap(GuestTrap::NullReference))
        } else {
            Err(TextError::Fault(VmFault::InvalidValueType))
        };
    };
    if let Some(literal) = image.literal_reference(reference) {
        return Ok(StringBacking::Literal(literal));
    }
    if heap.managed_type(reference).map_err(TextError::Fault)?
        != image
            .string_type()
            .and_then(|ty| image.type_id(ty))
            .ok_or(TextError::Fault(VmFault::InvalidResolvedId))?
    {
        return Err(TextError::Fault(VmFault::InvalidReference));
    }
    let header = heap
        .read_payload(reference, 0, 8)
        .map_err(TextError::Fault)?;
    let length = u32::from_le_bytes(header[..4].try_into().unwrap());
    let encoding = match header[4] {
        0 => StringEncoding::Latin1,
        1 => StringEncoding::Utf16,
        _ => return Err(TextError::Fault(VmFault::CorruptHeap)),
    };
    Ok(StringBacking::Managed {
        reference,
        length,
        encoding,
    })
}

fn code_unit(
    image: &ExecutionImage,
    heap: &Heap,
    value: StringBacking,
    index: u32,
) -> Result<u16, TextError> {
    match value {
        StringBacking::Inline { units, start, .. } => {
            Ok(units[usize::from(start) + index as usize])
        }
        StringBacking::Literal(literal) => {
            let offset = index as usize * 2;
            let bytes = image.literal_bytes(literal);
            Ok(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]))
        }
        StringBacking::Managed {
            reference,
            encoding,
            ..
        } => match encoding {
            StringEncoding::Latin1 => Ok(u16::from(
                heap.read_payload(reference, 8 + index, 1)
                    .map_err(TextError::Fault)?[0],
            )),
            StringEncoding::Utf16 => {
                let bytes = heap
                    .read_payload(reference, 8 + index * 2, 2)
                    .map_err(TextError::Fault)?;
                Ok(u16::from_le_bytes(bytes[..2].try_into().unwrap()))
            }
        },
        StringBacking::CharArray { reference, .. } => {
            let bytes = heap
                .read_payload(reference, 8 + index * 2, 2)
                .map_err(TextError::Fault)?;
            Ok(u16::from_le_bytes(bytes[..2].try_into().unwrap()))
        }
    }
}

fn scalar_units(value: RuntimeValue, form: u8) -> Result<([u16; 11], u8, u8), TextError> {
    let mut units = [0_u16; 11];
    match (form, value) {
        (1, RuntimeValue::I32(value)) => {
            let negative = value < 0;
            let mut magnitude = value.unsigned_abs();
            let mut start = units.len();
            loop {
                start -= 1;
                units[start] = u16::from(b'0') + (magnitude % 10) as u16;
                magnitude /= 10;
                if magnitude == 0 {
                    break;
                }
            }
            if negative {
                start -= 1;
                units[start] = u16::from(b'-');
            }
            Ok((units, start as u8, (units.len() - start) as u8))
        }
        (5, RuntimeValue::Bool(value)) => {
            let text: &[u8] = if value { b"true" } else { b"false" };
            for (destination, source) in units.iter_mut().zip(text.iter().copied()) {
                *destination = u16::from(source);
            }
            Ok((units, 0, text.len() as u8))
        }
        (6, RuntimeValue::Char(value)) => {
            units[0] = value;
            Ok((units, 0, 1))
        }
        _ => Err(TextError::Fault(VmFault::InvalidValueType)),
    }
}
