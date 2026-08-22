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
