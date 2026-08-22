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

pub use error::{AdmissionError, RunError};
pub use host::{CapabilityBinding, EntryValue, ExecutionProfile, HostValueType, OperationSchema};
pub use session::Session;

#[cfg(test)]
mod fixtures;
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
