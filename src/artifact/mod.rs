pub(crate) mod format;

use std::sync::Arc;

use crate::decode::container::Header;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub(crate) struct $name(pub u32);
    };
}

id_type!(ModuleId);
id_type!(TypeId);
id_type!(FunctionId);
id_type!(BlockId);
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ImportId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ByteRange {
    pub start: usize,
    pub end: usize,
}

impl ByteRange {
    pub(crate) fn slice<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        &bytes[self.start..self.end]
    }
}

#[derive(Debug)]
pub(crate) struct DecodedArtifact {
    pub bytes: Arc<[u8]>,
    pub content_hash: [u8; 32],
    pub header: Header,
    pub manifest: Manifest,
    pub capabilities: Vec<Capability>,
    pub modules: Vec<DecodedModule>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Manifest {
    pub required_heap_bytes: u32,
    pub required_stack_bytes: u32,
    pub maximum_coroutines: u32,
    pub maximum_call_depth: u32,
    pub maximum_host_requests: u32,
    pub maximum_events: u32,
    pub maximum_block_cost: u32,
    pub minimum_slice_cost: u32,
    pub required_capabilities: u32,
    pub optional_capabilities: u32,
    pub compiler_abi: [u8; 32],
    pub standard_library_abi: [u8; 32],
}

#[derive(Debug)]
pub(crate) struct DecodedModule {
    pub name_string: u32,
    pub flags: u32,
    pub semantic_hash: [u8; 32],
    pub declared_imports: u32,
    pub declared_exports: u32,
    pub declared_types: u32,
    pub declared_functions: u32,
    pub strings: Vec<ByteRange>,
    pub types: Vec<NominalType>,
    pub constants: Vec<Constant>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub fields: Vec<Field>,
    pub functions: Vec<Function>,
    pub blocks: Vec<Block>,
    pub code: Vec<ByteRange>,
    pub exceptions: Vec<ExceptionEntry>,
    pub debug: Vec<DebugEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ValueType {
    pub kind: u8,
    pub flags: u8,
    pub nominal_type: TypeId,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum NominalType {
    Class {
        flags: u8,
        generic_arity: u16,
        name: u32,
        super_type: TypeId,
        interfaces: Vec<TypeId>,
        field_start: u32,
        field_count: u32,
        method_start: u32,
        method_count: u32,
    },
    Interface {
        flags: u8,
        generic_arity: u16,
        name: u32,
        super_type: TypeId,
        interfaces: Vec<TypeId>,
        method_start: u32,
        method_count: u32,
    },
    Array {
        name: u32,
        element: ValueType,
    },
    Function {
        name: u32,
        flags: u16,
        result: ValueType,
        parameters: Vec<ValueType>,
    },
}

#[derive(Debug, PartialEq)]
pub(crate) enum Constant {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(char),
    String(u32),
    Null,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Capability {
    pub namespace: u32,
    pub name: u32,
    pub abi_major: u16,
    pub minimum_abi_minor: u16,
    pub flags: u32,
    pub operation_count: u32,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Import {
    pub kind: u8,
    pub target_module: ModuleId,
    pub target_name: u32,
    pub expected_signature: TypeId,
    pub target_hash: [u8; 32],
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Export {
    pub kind: u8,
    pub visibility: u8,
    pub name: u32,
    pub local_symbol: u32,
    pub signature: TypeId,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Field {
    pub owner: TypeId,
    pub name: u32,
    pub value_type: ValueType,
    pub flags: u32,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Function {
    pub owner: TypeId,
    pub name: u32,
    pub signature: TypeId,
    pub flags: u32,
    pub register_count: u16,
    pub parameter_count: u16,
    pub first_block: BlockId,
    pub block_count: u32,
    pub first_exception: u32,
    pub exception_count: u32,
    pub registers: Vec<ValueType>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Block {
    pub owner_function: FunctionId,
    pub code_record: BlockId,
    pub instruction_count: u32,
    pub declared_fixed_cost: u32,
    pub flags: u32,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ExceptionEntry {
    pub owner_function: FunctionId,
    pub first_protected_block: BlockId,
    pub protected_block_count: u32,
    pub catch_type: TypeId,
    pub handler_block: BlockId,
    pub exception_register: u16,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct DebugEntry {
    pub function: FunctionId,
    pub block: BlockId,
    pub instruction: u32,
    pub start_utf16: u32,
    pub end_utf16: u32,
    pub inline_parent: u32,
    pub source_path: ByteRange,
}
