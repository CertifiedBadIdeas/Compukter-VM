//! Structured diagnostics produced while loading untrusted artifacts.

const MAX_DETAIL_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Family {
    Container,
    Section,
    Limit,
    Module,
    Symbol,
    Type,
    Code,
    Cfg,
    Register,
    Exception,
    Capability,
    Cost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Code {
    UnexpectedEnd,
    IntegerOverflow,
    NonCanonicalUleb128,
    InvalidUtf8,
    LimitExceeded,
    BadMagic,
    UnsupportedVersion,
    BadLength,
    BadDigest,
    BadDirectory,
    BadSection,
    BadRecord,
    BadModule,
    BadSymbol,
    BadType,
    BadInstruction,
    BadControlFlow,
    UninitializedRegister,
    BadException,
    BadCapability,
    BadCost,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Location {
    pub offset: Option<u64>,
    pub section: Option<u16>,
    pub module: Option<u32>,
    pub function: Option<u32>,
    pub block: Option<u32>,
    pub instruction: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub family: Family,
    pub code: Code,
    pub location: Location,
    pub detail: String,
}

impl Diagnostic {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn at_offset(
        family: Family,
        code: Code,
        offset: usize,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            family,
            code,
            location: Location {
                offset: u64::try_from(offset).ok(),
                ..Location::default()
            },
            detail: truncate_detail(detail.into()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticSet {
    diagnostics: Vec<Diagnostic>,
    limit: usize,
}

impl DiagnosticSet {
    pub fn new(limit: usize) -> Self {
        Self {
            diagnostics: Vec::new(),
            limit,
        }
    }

    pub fn push(&mut self, mut diagnostic: Diagnostic) {
        if self.diagnostics.len() >= self.limit {
            return;
        }
        diagnostic.detail = truncate_detail(diagnostic.detail);
        self.diagnostics.push(diagnostic);
    }

    pub fn first(&self) -> Option<&Diagnostic> {
        self.diagnostics.first()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Diagnostic> {
        self.diagnostics.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    pub fn len(&self) -> usize {
        self.diagnostics.len()
    }
}

fn truncate_detail(mut detail: String) -> String {
    if detail.len() <= MAX_DETAIL_BYTES {
        return detail;
    }

    let mut boundary = MAX_DETAIL_BYTES;
    while !detail.is_char_boundary(boundary) {
        boundary -= 1;
    }
    detail.truncate(boundary);
    detail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_count_and_utf8_detail_bytes() {
        let diagnostic =
            Diagnostic::at_offset(Family::Container, Code::BadLength, 7, "ы".repeat(200));
        let mut set = DiagnosticSet::new(1);
        set.push(diagnostic.clone());
        set.push(diagnostic);

        assert_eq!(set.len(), 1);
        assert!(set.first().unwrap().detail.len() <= MAX_DETAIL_BYTES);
        assert!(set
            .first()
            .unwrap()
            .detail
            .is_char_boundary(set.first().unwrap().detail.len()));
    }
}
