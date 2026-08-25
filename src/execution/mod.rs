#![allow(dead_code)]

mod error;
mod gc;
mod heap;
mod heap_ops;
mod host;
mod image;
mod layout;
mod machine;
mod numeric;
mod session;
mod text;
mod value;

pub use error::{AdmissionError, EntryArgumentLimit, GuestTrap, RunError, VmFault};
pub use host::{
    AccountingSnapshot, AdvanceOutcome, CapabilityBinding, EntryArgumentLimits, EntryValue,
    ExecutionProfile, HostArguments, HostFailure, HostFailureKind, HostRequestView, HostResponse,
    HostValueInput, HostValueType, HostValueView, ManagedAllocationFailure, OperationSchema,
    QuotaExhaustion, QuotaKind, RequestId, ResumeError, TaskId,
};
pub use session::Session;

#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod gc_tests;
#[cfg(test)]
mod heap_tests;
#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod text_tests;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct FunctionKey {
    pub module: u32,
    pub function: u32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct TypeKey {
    pub module: u32,
    pub ty: u32,
}
