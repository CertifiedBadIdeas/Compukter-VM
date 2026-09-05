pub(crate) const HEADER_SIZE: usize = 64;
pub(crate) const DIRECTORY_ENTRY_SIZE: usize = 32;
pub(crate) const DIGEST_SIZE: usize = 32;

pub(crate) const FORMAT_MAJOR: u16 = 3;
pub(crate) const RUNTIME_ABI_MAJOR: u16 = 1;
pub(crate) const RUNTIME_ABI_MINOR: u16 = 0;

pub(crate) const CRITICAL: u16 = 1 << 0;
pub(crate) const SEMANTIC: u16 = 1 << 1;
pub(crate) const KNOWN_FLAGS: u16 = CRITICAL | SEMANTIC;
pub(crate) const KNOWN_FEATURES: u32 = 0b1111;

pub(crate) const MANIFEST: u16 = 0x0001;
pub(crate) const MODULES: u16 = 0x0002;
pub(crate) const CAPABILITIES: u16 = 0x0003;
pub(crate) const STRINGS: u16 = 0x0100;
pub(crate) const TYPES: u16 = 0x0101;
pub(crate) const CONSTANTS: u16 = 0x0102;
pub(crate) const IMPORTS: u16 = 0x0103;
pub(crate) const EXPORTS: u16 = 0x0104;
pub(crate) const FIELDS: u16 = 0x0105;
pub(crate) const FUNCTIONS: u16 = 0x0106;
pub(crate) const BLOCKS: u16 = 0x0107;
pub(crate) const CODE: u16 = 0x0108;
pub(crate) const EXCEPTIONS: u16 = 0x0109;
pub(crate) const UTF16_LITERALS: u16 = 0x010a;
pub(crate) const SAFEPOINT_ROOTS: u16 = 0x010b;
pub(crate) const DEBUG: u16 = 0x0110;
pub(crate) const OPTIONAL_EXTENSION_START: u16 = 0x8000;

pub(crate) fn is_global(kind: u16) -> bool {
    matches!(kind, MANIFEST | MODULES | CAPABILITIES)
}

pub(crate) fn is_module(kind: u16) -> bool {
    matches!(
        kind,
        STRINGS
            | TYPES
            | CONSTANTS
            | IMPORTS
            | EXPORTS
            | FIELDS
            | FUNCTIONS
            | BLOCKS
            | CODE
            | EXCEPTIONS
            | UTF16_LITERALS
            | SAFEPOINT_ROOTS
            | DEBUG
    )
}

pub(crate) fn valid_instruction_form(opcode: u8, form: u8) -> bool {
    match opcode {
        0x10..=0x15 => matches!(form, 1..=4),
        0x16..=0x1b => matches!(form, 1 | 2),
        0x20..=0x21 => matches!(form, 1..=6),
        0x22..=0x25 => matches!(form, 1..=4 | 6),
        0x26..=0x27 => form == 7,
        0x68 => matches!(form, 1 | 5 | 6),
        _ => form == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::is_module;

    #[test]
    fn utf16_literals_is_a_module_section() {
        assert!(is_module(0x010a));
    }
}
