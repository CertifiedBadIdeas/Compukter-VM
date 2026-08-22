//! Host policy limits applied before artifact-controlled allocation.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactLimits {
    pub artifact_bytes: usize,
    pub sections: usize,
    pub modules: usize,
    pub records_per_section: usize,
    pub strings_bytes: usize,
    pub utf16_literal_code_units: usize,
    pub code_bytes: usize,
    pub functions: usize,
    pub blocks: usize,
    pub registers_per_function: usize,
    pub imports: usize,
    pub exceptions: usize,
    pub capabilities: usize,
    pub debug_bytes: usize,
    pub diagnostics: usize,
}

impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            artifact_bytes: 16 * 1024 * 1024,
            sections: 4_096,
            modules: 1_024,
            records_per_section: 1_000_000,
            strings_bytes: 8 * 1024 * 1024,
            utf16_literal_code_units: 4 * 1024 * 1024,
            code_bytes: 12 * 1024 * 1024,
            functions: 1_000_000,
            blocks: 4_000_000,
            registers_per_function: u16::MAX as usize,
            imports: 1_000_000,
            exceptions: 1_000_000,
            capabilities: 65_536,
            debug_bytes: 16 * 1024 * 1024,
            diagnostics: 32,
        }
    }
}
