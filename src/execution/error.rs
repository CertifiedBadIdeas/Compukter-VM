use super::value::{ReferenceValue, RuntimeValue};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    CompilerAbiMismatch,
    StandardLibraryAbiMismatch,
    MissingCapability {
        index: u8,
    },
    HeapLimit {
        required: u32,
        available: u32,
    },
    InvalidHeapSize {
        supplied: u32,
    },
    FrameStorageLimit {
        required: u64,
        available: u64,
    },
    CallDepthLimit {
        required: u32,
        available: u32,
    },
    CoroutineLimit {
        required: u32,
        available: u32,
    },
    HostRequestLimit {
        required: u32,
        available: u32,
    },
    EventLimit {
        required: u32,
        available: u32,
    },
    SliceLimit {
        required: u32,
        available: u32,
    },
    StoragePlanOverflow,
    AllocationFailed,
    InvalidEntry,
    DuplicateCapabilityBinding,
    CapabilityOperationCount {
        capability: u32,
        required: u32,
        available: u32,
    },
    CapabilitySchema {
        capability: u32,
        operation: u32,
    },
    SynchronousCapabilityUnsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunError {
    AlreadyStarted,
    NotStarted,
    NotRunnable,
    InvalidSliceBudget {
        minimum: u32,
        maximum: u32,
        supplied: u32,
    },
    EntryArity {
        expected: u16,
        supplied: u16,
    },
    EntryType {
        parameter: u16,
    },
    ForeignReference {
        parameter: u16,
    },
    DeadReference {
        parameter: u16,
    },
    EntryArgumentLimit(EntryArgumentLimit),
    EntryAllocationFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryArgumentLimit {
    Count,
    ArgumentCodeUnits,
    TotalCodeUnits,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestTrap {
    DivisionByZero,
    StackOverflow,
    NegativeArraySize,
    NullReference,
    IndexOutOfBounds,
    ClassCast,
    InvalidExitCode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllocationRequestKind {
    Object,
    Array,
    String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AllocationSource {
    pub module: u32,
    pub function: u32,
    pub block: u32,
    pub instruction: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationDiagnostic {
    pub request_kind: AllocationRequestKind,
    pub requested: u32,
    pub live: u32,
    pub total_free: u32,
    pub largest_free_block: u32,
    pub source: AllocationSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AllocationExhaustion {
    pub exception: ReferenceValue,
    pub diagnostic: AllocationDiagnostic,
    pub collection_attempted: bool,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmFault {
    InvalidResolvedId,
    InvalidValueType,
    AccountingOverflow,
    InvalidStoragePlan,
    CorruptLifecycle,
    ReachedUnreachable,
    UnsupportedInstruction,
    HandleExhausted,
    CorruptHeap,
    InvalidReference,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Outcome {
    SliceExhausted,
    HostRequest,
    AllocationExhausted(AllocationExhaustion),
    Halted(Option<RuntimeValue>),
    Crashed(GuestTrap),
    Faulted(VmFault),
}

impl Outcome {
    pub(super) fn is_terminal(self) -> bool {
        !matches!(self, Self::SliceExhausted | Self::HostRequest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_outcomes_are_stable_and_distinct() {
        let halted = Outcome::Halted(None);
        assert!(halted.is_terminal());
        assert!(Outcome::Crashed(GuestTrap::DivisionByZero).is_terminal());
        assert!(Outcome::Faulted(VmFault::ReachedUnreachable).is_terminal());
        assert!(!Outcome::SliceExhausted.is_terminal());
    }

    #[test]
    fn failures_have_bounded_scalar_payloads() {
        assert!(core::mem::size_of::<GuestTrap>() <= 8);
        assert!(core::mem::size_of::<AdmissionError>() <= 48);
        assert!(core::mem::size_of::<RunError>() <= 32);
    }
}
