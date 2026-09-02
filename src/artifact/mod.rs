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
id_type!(Utf16LiteralId);
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
pub enum EntryArguments {
    None,
    StringArray,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryPoint {
    pub module: u32,
    pub function: u32,
    pub arguments: EntryArguments,
}

/// A fully decoded and verified artifact.
///
/// It cannot be constructed without passing [`crate::verify_artifact`].
///
/// ```compile_fail
/// let artifact = compukter_vm::VerifiedArtifact {};
/// ```
#[derive(Clone)]
pub struct VerifiedArtifact {
    inner: Arc<VerifiedArtifactInner>,
}

impl std::fmt::Debug for VerifiedArtifact {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedArtifact")
            .field("content_hash", &self.content_hash())
            .field("entry", &self.entry())
            .field("module_count", &self.module_count())
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct VerifiedArtifactInner {
    decoded: DecodedArtifact,
}

impl VerifiedArtifact {
    pub(crate) fn new(decoded: DecodedArtifact) -> Self {
        Self {
            inner: Arc::new(VerifiedArtifactInner { decoded }),
        }
    }

    pub fn content_hash(&self) -> [u8; 32] {
        self.inner.decoded.content_hash
    }

    pub fn entry(&self) -> EntryPoint {
        EntryPoint {
            module: self.inner.decoded.header.entry_module,
            function: self.inner.decoded.header.entry_function,
            arguments: self.inner.decoded.header.entry_arguments,
        }
    }

    pub fn module_count(&self) -> usize {
        self.inner.decoded.modules.len()
    }

    pub(crate) fn decoded(&self) -> &DecodedArtifact {
        &self.inner.decoded
    }
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
    pub platform_abi: [u8; 32],
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
    pub utf16_literals: Vec<ByteRange>,
    pub types: Vec<NominalType>,
    pub constants: Vec<Constant>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub fields: Vec<Field>,
    pub functions: Vec<Function>,
    pub blocks: Vec<Block>,
    pub code: Vec<DecodedCode>,
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
    Char(u16),
    String(Utf16LiteralId),
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

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct DecodedCode {
    pub bytes: ByteRange,
    pub instructions: Box<[Instruction]>,
    pub fixed_cost: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SwitchCase {
    pub value: i32,
    pub target: u32,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Instruction {
    Nop,
    Move {
        dst: u16,
        src: u16,
    },
    Const {
        dst: u16,
        constant: u32,
    },
    Null {
        dst: u16,
    },
    Convert {
        dst: u16,
        src: u16,
    },
    Add {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Sub {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Mul {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Div {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Rem {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Neg {
        form: u8,
        dst: u16,
        src: u16,
    },
    BitAnd {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    BitOr {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    BitXor {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    ShiftLeft {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    ShiftRight {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    ShiftUnsigned {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Equal {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    NotEqual {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Less {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    LessEqual {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    Greater {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    GreaterEqual {
        form: u8,
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    RefEqual {
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    RefNotEqual {
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    NewObject {
        dst: u16,
        type_ref: u32,
    },
    NewArray {
        dst: u16,
        type_ref: u32,
        length: u16,
    },
    ArrayLength {
        dst: u16,
        array: u16,
    },
    ArrayLoad {
        dst: u16,
        array: u16,
        index: u16,
    },
    ArrayStore {
        array: u16,
        index: u16,
        value: u16,
    },
    FieldGet {
        dst: u16,
        receiver: u16,
        field_ref: u32,
    },
    FieldSet {
        receiver: u16,
        field_ref: u32,
        value: u16,
    },
    StaticGet {
        dst: u16,
        field_ref: u32,
    },
    StaticSet {
        field_ref: u32,
        value: u16,
    },
    IsType {
        dst: u16,
        value: u16,
        type_ref: u32,
    },
    CheckedCast {
        dst: u16,
        value: u16,
        type_ref: u32,
    },
    CallDirect {
        dst: u16,
        function_ref: u32,
        args: Box<[u16]>,
    },
    CallVirtual {
        dst: u16,
        function_ref: u32,
        args: Box<[u16]>,
    },
    CallInterface {
        dst: u16,
        function_ref: u32,
        args: Box<[u16]>,
    },
    CoroutineSpawn {
        dst: u16,
        function_ref: u32,
        args: Box<[u16]>,
    },
    CapabilityCallSync {
        dst: u16,
        capability: u32,
        operation: u32,
        args: Box<[u16]>,
    },
    StringLength {
        dst: u16,
        string: u16,
    },
    StringGet {
        dst: u16,
        string: u16,
        index: u16,
    },
    StringEquals {
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    StringCompare {
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    StringHash {
        dst: u16,
        string: u16,
    },
    StringConcat {
        dst: u16,
        lhs: u16,
        rhs: u16,
    },
    StringValueOf {
        form: u8,
        dst: u16,
        source: u16,
    },
    StringSubstring {
        dst: u16,
        string: u16,
        start: u16,
        end: u16,
    },
    StringFromCharArray {
        dst: u16,
        array: u16,
        start: u16,
        end: u16,
    },
    Jump {
        target: u32,
    },
    Branch {
        condition: u16,
        true_block: u32,
        false_block: u32,
    },
    SwitchI32 {
        key: u16,
        default_block: u32,
        cases: Box<[SwitchCase]>,
    },
    Return {
        value: u16,
    },
    Throw {
        exception: u16,
    },
    CallSuspend {
        dst: u16,
        function_ref: u32,
        args: Box<[u16]>,
        resume_block: u32,
    },
    Yield {
        resume_block: u32,
    },
    Sleep {
        duration: u16,
        resume_block: u32,
    },
    CoroutineJoin {
        dst: u16,
        coroutine: u16,
        resume_block: u32,
    },
    CapabilityCallAsync {
        dst: u16,
        capability: u32,
        operation: u32,
        args: Box<[u16]>,
        resume_block: u32,
    },
    Unreachable,
}
