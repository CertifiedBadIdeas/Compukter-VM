#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum RuntimeValue {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(char),
    Null,
}
