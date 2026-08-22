#![allow(dead_code)]

mod error;
mod gc;
mod heap;
mod heap_ops;
mod image;
mod layout;
mod machine;
mod numeric;
mod value;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod gc_tests;
#[cfg(test)]
mod heap_tests;
#[cfg(test)]
mod tests;

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
