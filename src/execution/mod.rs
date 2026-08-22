#![allow(dead_code)]

mod error;
mod image;
mod numeric;
mod value;

#[cfg(test)]
mod fixtures;

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
