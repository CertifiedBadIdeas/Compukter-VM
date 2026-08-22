use super::value::RuntimeValue;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdmissionError {
    CompilerAbiMismatch,
    StandardLibraryAbiMismatch,
    MissingCapability { index: u8 },
    HeapLimit { required: u32, available: u32 },
    InvalidHeapSize { supplied: u32 },
    FrameStorageLimit { required: u64, available: u64 },
    CallDepthLimit { required: u32, available: u32 },
    CoroutineLimit { required: u32, available: u32 },
    HostRequestLimit { required: u32, available: u32 },
    EventLimit { required: u32, available: u32 },
    SliceLimit { required: u32, available: u32 },
    StoragePlanOverflow,
    AllocationFailed,
    InvalidEntry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RunError {
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
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GuestTrap {
    DivisionByZero,
    StackOverflow,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VmFault {
    InvalidResolvedId,
    InvalidValueType,
    AccountingOverflow,
    InvalidStoragePlan,
    CorruptLifecycle,
    ReachedUnreachable,
    UnsupportedInstruction,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Outcome {
    SliceExhausted,
    Halted(Option<RuntimeValue>),
    Crashed(GuestTrap),
    Faulted(VmFault),
}

impl Outcome {
    pub(super) fn is_terminal(self) -> bool {
        !matches!(self, Self::SliceExhausted)
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
