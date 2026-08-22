#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostValueType {
    Unit,
    I32,
    I64,
    F32,
    F64,
    Bool,
    Char,
    String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationSchema<'a> {
    pub arguments: &'a [HostValueType],
    pub result: HostValueType,
    pub asynchronous: bool,
}

impl<'a> OperationSchema<'a> {
    pub const fn asynchronous(arguments: &'a [HostValueType], result: HostValueType) -> Self {
        Self {
            arguments,
            result,
            asynchronous: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityBinding<'a> {
    namespace: &'a str,
    name: &'a str,
    abi_major: u16,
    abi_minor: u16,
    operations: &'a [OperationSchema<'a>],
}

impl<'a> CapabilityBinding<'a> {
    pub const fn new(
        namespace: &'a str,
        name: &'a str,
        abi_major: u16,
        abi_minor: u16,
        operations: &'a [OperationSchema<'a>],
    ) -> Self {
        Self {
            namespace,
            name,
            abi_major,
            abi_minor,
            operations,
        }
    }

    pub const fn namespace(&self) -> &'a str {
        self.namespace
    }

    pub const fn name(&self) -> &'a str {
        self.name
    }

    pub const fn abi_major(&self) -> u16 {
        self.abi_major
    }

    pub const fn abi_minor(&self) -> u16 {
        self.abi_minor
    }

    pub const fn operations(&self) -> &'a [OperationSchema<'a>] {
        self.operations
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionProfile {
    pub heap_bytes: u32,
    pub frame_storage_bytes: u64,
    pub maximum_call_depth: u32,
    pub maximum_coroutines: u32,
    pub maximum_host_requests: u32,
    pub maximum_events: u32,
    pub maximum_slice_budget: u32,
    pub compiler_abi: [u8; 32],
    pub standard_library_abi: [u8; 32],
    pub maximum_host_arguments: u32,
    pub maximum_outbound_utf16_code_units: u32,
    pub maximum_inbound_utf16_code_units: u32,
    pub maximum_accepted_responses: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EntryValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(u16),
}

#[derive(Clone, Debug)]
pub(super) struct ResolvedOperation {
    pub arguments: Box<[HostValueType]>,
    pub result: HostValueType,
    pub asynchronous: bool,
}

#[derive(Clone, Debug)]
pub(super) struct ResolvedCapability {
    pub namespace: Box<str>,
    pub name: Box<str>,
    pub abi_major: u16,
    pub abi_minor: u16,
    pub operations: Box<[ResolvedOperation]>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RequestId(u64);

impl RequestId {
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HostValueInput<'a> {
    Unit,
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(u16),
    String(&'a [u16]),
}

impl HostValueInput<'_> {
    pub(super) const fn value_type(self) -> HostValueType {
        match self {
            Self::Unit => HostValueType::Unit,
            Self::I32(_) => HostValueType::I32,
            Self::I64(_) => HostValueType::I64,
            Self::F32(_) => HostValueType::F32,
            Self::F64(_) => HostValueType::F64,
            Self::Bool(_) => HostValueType::Bool,
            Self::Char(_) => HostValueType::Char,
            Self::String(_) => HostValueType::String,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HostValueView<'a> {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(u16),
    String(&'a [u16]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostFailureKind {
    EndOfFile,
    Unavailable,
    InputOutput,
    Cancelled,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostFailure {
    kind: HostFailureKind,
    code: u32,
}

impl HostFailure {
    pub const fn new(kind: HostFailureKind, code: u32) -> Self {
        Self { kind, code }
    }

    pub const fn kind(self) -> HostFailureKind {
        self.kind
    }

    pub const fn code(self) -> u32 {
        self.code
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HostResponse<'a> {
    Success(HostValueInput<'a>),
    Failure(HostFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResumeError {
    NoPendingRequest,
    WrongRequestId,
    WrongResponseType,
    ResponseTooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum HostValueSlot {
    Empty,
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(u16),
    String { start: u32, length: u32 },
}

#[derive(Clone, Copy, Debug)]
pub struct HostArguments<'a> {
    pub(super) slots: &'a [HostValueSlot],
    pub(super) utf16: &'a [u16],
}

impl<'a> HostArguments<'a> {
    pub const fn len(self) -> usize {
        self.slots.len()
    }

    pub const fn is_empty(self) -> bool {
        self.slots.is_empty()
    }

    pub fn get(self, index: usize) -> Option<HostValueView<'a>> {
        match *self.slots.get(index)? {
            HostValueSlot::Empty => None,
            HostValueSlot::I32(value) => Some(HostValueView::I32(value)),
            HostValueSlot::I64(value) => Some(HostValueView::I64(value)),
            HostValueSlot::F32(value) => Some(HostValueView::F32(value)),
            HostValueSlot::F64(value) => Some(HostValueView::F64(value)),
            HostValueSlot::Bool(value) => Some(HostValueView::Bool(value)),
            HostValueSlot::Char(value) => Some(HostValueView::Char(value)),
            HostValueSlot::String { start, length } => {
                let start = usize::try_from(start).ok()?;
                let end = start.checked_add(usize::try_from(length).ok()?)?;
                self.utf16.get(start..end).map(HostValueView::String)
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HostRequestView<'a> {
    pub(super) id: RequestId,
    pub(super) capability: &'a ResolvedCapability,
    pub(super) operation: u32,
    pub(super) arguments: HostArguments<'a>,
}

impl<'a> HostRequestView<'a> {
    pub const fn id(self) -> RequestId {
        self.id
    }
    pub fn namespace(self) -> &'a str {
        &self.capability.namespace
    }
    pub fn name(self) -> &'a str {
        &self.capability.name
    }
    pub const fn abi_major(self) -> u16 {
        self.capability.abi_major
    }
    pub const fn abi_minor(self) -> u16 {
        self.capability.abi_minor
    }
    pub const fn operation(self) -> u32 {
        self.operation
    }
    pub const fn arguments(self) -> HostArguments<'a> {
        self.arguments
    }
}

impl PartialEq for HostRequestView<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.namespace() == other.namespace()
            && self.name() == other.name()
            && self.abi_major() == other.abi_major()
            && self.abi_minor() == other.abi_minor()
            && self.operation == other.operation
            && self.arguments.len() == other.arguments.len()
            && (0..self.arguments.len())
                .all(|index| self.arguments.get(index) == other.arguments.get(index))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ManagedAllocationFailure {
    pub diagnostic: super::error::AllocationDiagnostic,
    pub collection_attempted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuotaKind {
    HostRequestCodeUnits,
    HostRequests,
    AcceptedResponses,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuotaExhaustion {
    pub kind: QuotaKind,
    pub limit: u64,
    pub consumed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AdvanceOutcome<'a> {
    SliceExhausted,
    HostRequest(HostRequestView<'a>),
    AllocationExhausted(ManagedAllocationFailure),
    QuotaExhausted(QuotaExhaustion),
    Halted(Option<HostValueView<'a>>),
    Crashed(super::error::GuestTrap),
    Faulted(super::error::VmFault),
    HostFailed(HostFailure),
}
