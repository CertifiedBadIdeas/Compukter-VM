use super::host::{HostMergeSchema, HostValueType, OperationSchema};

pub const MAXIMUM_RECORD_DEPTH: usize = 8;
pub const MAXIMUM_RECORD_FIELDS: usize = 64;
pub const MAXIMUM_RECORD_NODES: usize = 32;
pub const MAXIMUM_RECORD_CODE_UNITS: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostRecordSchema {
    pub type_name: String,
    pub fields: Vec<HostRecordField>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostRecordField {
    pub name: String,
    pub value_type: HostValueType,
    pub record: Option<Box<HostRecordSchema>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostRecordValue {
    pub type_name: String,
    pub fields: Vec<HostRecordMember>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostRecordMember {
    pub name: String,
    pub value: HostRecordScalar,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HostRecordScalar {
    I32(i32),
    I64(i64),
    F32(u32),
    F64(u64),
    Bool(bool),
    Char(u16),
    String(Vec<u16>),
    Record(Box<HostRecordValue>),
}

pub(super) fn valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && (name.as_bytes()[0].is_ascii_alphabetic() || name.as_bytes()[0] == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub(super) fn valid_type_name(name: &str) -> bool {
    name.len() <= 256 && name.contains('.') && name.split('.').all(valid_identifier)
}

pub(super) fn valid_operation(operation: &OperationSchema<'_>) -> bool {
    if matches!(operation.result, HostValueType::CharArrayRange { .. })
        || operation.arguments.iter().any(|ty| match ty {
            HostValueType::CharArrayRange { start, end } => {
                operation.arguments.get(*start as usize) != Some(&HostValueType::I32)
                    || operation.arguments.get(*end as usize) != Some(&HostValueType::I32)
            }
            _ => false,
        })
        || operation.arguments.contains(&HostValueType::Record)
    {
        return false;
    }
    match operation.result_record {
        None => operation.result != HostValueType::Record,
        Some(record) => {
            operation.result == HostValueType::Record
                && operation.asynchronous
                && operation.merge == HostMergeSchema::Ordinary
                && record.validate()
        }
    }
}

impl HostRecordSchema {
    pub fn validate(&self) -> bool {
        fn visit(
            record: &HostRecordSchema,
            depth: usize,
            fields: &mut usize,
            nodes: &mut usize,
        ) -> bool {
            if depth > MAXIMUM_RECORD_DEPTH
                || !valid_type_name(&record.type_name)
                || record.fields.is_empty()
            {
                return false;
            }
            *fields += record.fields.len();
            *nodes += 1;
            if *fields > MAXIMUM_RECORD_FIELDS || *nodes > MAXIMUM_RECORD_NODES {
                return false;
            }
            for (i, field) in record.fields.iter().enumerate() {
                if !valid_identifier(&field.name)
                    || record.fields[..i]
                        .iter()
                        .any(|other| other.name == field.name)
                {
                    return false;
                }
                match (&field.value_type, &field.record) {
                    (HostValueType::Record, Some(child))
                        if visit(child, depth + 1, fields, nodes) => {}
                    (
                        HostValueType::Record
                        | HostValueType::Unit
                        | HostValueType::ByteArray
                        | HostValueType::CharArrayRange { .. },
                        _,
                    )
                    | (_, Some(_)) => return false,
                    (HostValueType::String, None) => {
                        *nodes += 1;
                        if *nodes > MAXIMUM_RECORD_NODES {
                            return false;
                        }
                    }
                    (_, None) => {}
                }
            }
            true
        }
        visit(self, 1, &mut 0, &mut 0)
    }

    pub(super) fn accepts(&self, value: &HostRecordValue, maximum_units: usize) -> bool {
        fn visit(
            schema: &HostRecordSchema,
            value: &HostRecordValue,
            units: &mut usize,
            maximum: usize,
        ) -> bool {
            if schema.type_name != value.type_name || schema.fields.len() != value.fields.len() {
                return false;
            }
            schema
                .fields
                .iter()
                .zip(&value.fields)
                .all(|(field, member)| {
                    if field.name != member.name {
                        return false;
                    }
                    match (&member.value, field.value_type) {
                        (HostRecordScalar::I32(_), HostValueType::I32)
                        | (HostRecordScalar::I64(_), HostValueType::I64)
                        | (HostRecordScalar::F32(_), HostValueType::F32)
                        | (HostRecordScalar::F64(_), HostValueType::F64)
                        | (HostRecordScalar::Bool(_), HostValueType::Bool)
                        | (HostRecordScalar::Char(_), HostValueType::Char) => true,
                        (HostRecordScalar::String(text), HostValueType::String) => {
                            *units += text.len();
                            *units <= maximum
                        }
                        (HostRecordScalar::Record(child), HostValueType::Record) => field
                            .record
                            .as_ref()
                            .is_some_and(|schema| visit(schema, child, units, maximum)),
                        _ => false,
                    }
                })
        }
        visit(
            self,
            value,
            &mut 0,
            maximum_units.min(MAXIMUM_RECORD_CODE_UNITS),
        )
    }
}

#[derive(Clone, Debug)]
pub(super) struct RecordLayout {
    pub ty: super::TypeKey,
    pub fields: Vec<RecordFieldLayout>,
    pub inline: bool,
}

#[derive(Clone, Debug)]
pub(super) struct RecordFieldLayout {
    pub offset: u32,
    pub width: super::layout::ValueWidth,
    pub record: Option<Box<RecordLayout>>,
    pub path: Vec<usize>,
}

pub(super) fn admit_record(
    artifact: &crate::artifact::DecodedArtifact,
    ty: super::TypeKey,
    schema: &HostRecordSchema,
    fields: &[super::image::ResolvedField],
    field_offsets: &[usize],
    string_type: Option<super::TypeKey>,
) -> Option<RecordLayout> {
    use super::{image::ResolvedValueType, layout::ValueWidth};
    use crate::artifact::NominalType;
    let module = artifact.modules.get(ty.module as usize)?;
    let (inline, flattened, slots): (bool, bool, Vec<(ResolvedValueType, u32)>) =
        match module.types.get(ty.ty as usize)? {
            NominalType::InlineValue { name, components } => {
                if module.strings.get(*name as usize)?.slice(&artifact.bytes)
                    != format!("{}.<inline-value>", schema.type_name).as_bytes()
                {
                    return None;
                }
                let mut slots = Vec::with_capacity(components.len());
                for (index, component) in components.iter().enumerate() {
                    let nominal = if component.kind == 7 {
                        Some(super::image::resolve_type(
                            artifact,
                            ty.module as usize,
                            component.nominal_type,
                        )?)
                    } else {
                        None
                    };
                    slots.push((
                        ResolvedValueType {
                            kind: component.kind,
                            nullable: component.flags & 1 != 0,
                            nominal,
                        },
                        index as u32,
                    ));
                }
                (true, true, slots)
            }
            NominalType::Class {
                name,
                generic_arity,
                interfaces,
                field_start,
                field_count,
                initializer,
                ..
            } => {
                if *generic_arity != 0
                    || !interfaces.is_empty()
                    || initializer.is_some()
                    || module.strings.get(*name as usize)?.slice(&artifact.bytes)
                        != schema.type_name.as_bytes()
                {
                    return None;
                }
                let declared = module.fields.get(
                    *field_start as usize
                        ..(*field_start as usize).checked_add(*field_count as usize)?,
                )?;
                let flattened = declared.first().is_some_and(|field| {
                    module.strings[field.name as usize].slice(&artifact.bytes) == b"<boxed-value>"
                });
                let mut slots = Vec::new();
                if flattened {
                    if artifact.header.runtime_minor < 20 {
                        return None;
                    }
                    for (index, field) in declared.iter().enumerate() {
                        let expected = "<boxed-value>";
                        if module.strings[field.name as usize].slice(&artifact.bytes)
                            != expected.as_bytes()
                            || field.flags & 2 != 0
                        {
                            return None;
                        }
                        let resolved = fields.get(
                            field_offsets
                                .get(ty.module as usize)?
                                .checked_add(*field_start as usize)?
                                .checked_add(index)?,
                        )?;
                        if resolved.owner != ty || resolved.value_type.nullable {
                            return None;
                        }
                        slots.push((resolved.value_type, resolved.offset?));
                    }
                } else {
                    if declared.len() != schema.fields.len() {
                        return None;
                    }
                    for expected in &schema.fields {
                        let (index, field) = declared.iter().enumerate().find(|(_, field)| {
                            module.strings[field.name as usize].slice(&artifact.bytes)
                                == expected.name.as_bytes()
                        })?;
                        if field.flags & 2 != 0 {
                            return None;
                        }
                        let resolved = fields.get(
                            field_offsets
                                .get(ty.module as usize)?
                                .checked_add(*field_start as usize)?
                                .checked_add(index)?,
                        )?;
                        if resolved.owner != ty || resolved.value_type.nullable {
                            return None;
                        }
                        slots.push((resolved.value_type, resolved.offset?));
                    }
                }
                (false, flattened, slots)
            }
            _ => return None,
        };
    struct Admission<'a> {
        artifact: &'a crate::artifact::DecodedArtifact,
        fields: &'a [super::image::ResolvedField],
        field_offsets: &'a [usize],
        string_type: Option<super::TypeKey>,
    }
    impl Admission<'_> {
        fn walk(
            &self,
            schema: &HostRecordSchema,
            slots: &[(ResolvedValueType, u32)],
            cursor: &mut usize,
            path: &mut Vec<usize>,
            result: &mut Vec<RecordFieldLayout>,
            flattened: bool,
        ) -> Option<()> {
            for (index, expected) in schema.fields.iter().enumerate() {
                path.push(index);
                let (actual, offset) = *slots.get(*cursor)?;
                if actual.nullable {
                    return None;
                }
                let (width, record) = match expected.value_type {
                    HostValueType::I32 if actual.kind == 1 => (ValueWidth::I32, None),
                    HostValueType::I64 if actual.kind == 2 => (ValueWidth::I64, None),
                    HostValueType::F32 if actual.kind == 3 => (ValueWidth::F32, None),
                    HostValueType::F64 if actual.kind == 4 => (ValueWidth::F64, None),
                    HostValueType::Bool if actual.kind == 5 => (ValueWidth::Bool, None),
                    HostValueType::Char if actual.kind == 6 => (ValueWidth::Char, None),
                    HostValueType::String
                        if actual.kind == 7 && actual.nominal == self.string_type =>
                    {
                        (ValueWidth::Ref, None)
                    }
                    HostValueType::Record => {
                        let child = expected.record.as_ref()?;
                        let managed = if actual.kind == 7 {
                            actual.nominal.and_then(|key| {
                                admit_record(
                                    self.artifact,
                                    key,
                                    child,
                                    self.fields,
                                    self.field_offsets,
                                    self.string_type,
                                )
                            })
                        } else {
                            None
                        };
                        if let Some(record) = managed {
                            (ValueWidth::Ref, Some(Box::new(record)))
                        } else if flattened {
                            self.walk(child, slots, cursor, path, result, true)?;
                            path.pop();
                            continue;
                        } else {
                            return None;
                        }
                    }
                    _ => return None,
                };
                result.push(RecordFieldLayout {
                    offset,
                    width,
                    record,
                    path: path.clone(),
                });
                *cursor += 1;
                path.pop();
            }
            Some(())
        }
    }
    let mut result = Vec::new();
    let mut cursor = 0;
    Admission {
        artifact,
        fields,
        field_offsets,
        string_type,
    }
    .walk(
        schema,
        &slots,
        &mut cursor,
        &mut Vec::new(),
        &mut result,
        flattened,
    )?;
    if cursor != slots.len() {
        return None;
    }
    Some(RecordLayout {
        ty,
        fields: result,
        inline,
    })
}

use super::{
    error::VmFault,
    heap::{AllocationRequest, Heap},
    heap_ops::{store_value, PendingAllocation, PendingState},
    image::ExecutionImage,
    text::{PendingHostReference, TextError},
    value::{Ref32, RuntimeValue},
};

#[derive(Debug)]
enum PlannedValue {
    Scalar(RuntimeValue),
    Node(usize),
}
#[derive(Debug)]
enum PlannedNode {
    String(Vec<u16>),
    Object {
        ty: super::TypeKey,
        fields: Vec<(RecordFieldLayout, PlannedValue)>,
    },
    Inline {
        ty: super::TypeKey,
        fields: Vec<(RecordFieldLayout, PlannedValue)>,
    },
}

pub(super) enum RecordOutput {
    Scalar(RuntimeValue),
    Inline(Vec<RuntimeValue>),
}

pub(super) struct PendingRecord {
    nodes: Vec<PlannedNode>,
    completed: Vec<Option<RuntimeValue>>,
    index: usize,
    field: usize,
    object: Option<Ref32>,
    allocation: Option<PendingAllocation>,
    string: Option<PendingHostReference>,
    collection_attempted: bool,
    inline_values: Vec<RuntimeValue>,
}

impl PendingRecord {
    #[cfg(test)]
    pub(super) fn test_has_staged_reference(&self) -> bool {
        self.inline_values
            .iter()
            .any(|value| matches!(value, RuntimeValue::Reference(_)))
    }

    pub(super) fn new(layout: &RecordLayout, value: HostRecordValue) -> Result<Self, VmFault> {
        fn plan(
            layout: &RecordLayout,
            mut value: HostRecordValue,
            nodes: &mut Vec<PlannedNode>,
        ) -> Result<usize, VmFault> {
            fn take(
                fields: &mut [HostRecordMember],
                path: &[usize],
            ) -> Result<HostRecordScalar, VmFault> {
                let (index, tail) = path.split_first().ok_or(VmFault::InvalidValueType)?;
                let value = &mut fields
                    .get_mut(*index)
                    .ok_or(VmFault::InvalidValueType)?
                    .value;
                if tail.is_empty() {
                    return Ok(std::mem::replace(value, HostRecordScalar::I32(0)));
                }
                let HostRecordScalar::Record(child) = value else {
                    return Err(VmFault::InvalidValueType);
                };
                take(&mut child.fields, tail)
            }
            let mut fields = Vec::with_capacity(layout.fields.len());
            for field in &layout.fields {
                let member = take(&mut value.fields, &field.path)?;
                let value = match member {
                    HostRecordScalar::I32(v) => PlannedValue::Scalar(RuntimeValue::I32(v)),
                    HostRecordScalar::I64(v) => PlannedValue::Scalar(RuntimeValue::I64(v)),
                    HostRecordScalar::F32(v) => PlannedValue::Scalar(RuntimeValue::F32(v)),
                    HostRecordScalar::F64(v) => PlannedValue::Scalar(RuntimeValue::F64(v)),
                    HostRecordScalar::Bool(v) => PlannedValue::Scalar(RuntimeValue::Bool(v)),
                    HostRecordScalar::Char(v) => PlannedValue::Scalar(RuntimeValue::Char(v)),
                    HostRecordScalar::String(v) => {
                        let index = nodes.len();
                        nodes.push(PlannedNode::String(v));
                        PlannedValue::Node(index)
                    }
                    HostRecordScalar::Record(v) => PlannedValue::Node(plan(
                        field.record.as_ref().ok_or(VmFault::InvalidValueType)?,
                        *v,
                        nodes,
                    )?),
                };
                fields.push((
                    RecordFieldLayout {
                        offset: field.offset,
                        width: field.width,
                        record: None,
                        path: field.path.clone(),
                    },
                    value,
                ));
            }
            let index = nodes.len();
            nodes.push(if layout.inline {
                PlannedNode::Inline {
                    ty: layout.ty,
                    fields,
                }
            } else {
                PlannedNode::Object {
                    ty: layout.ty,
                    fields,
                }
            });
            Ok(index)
        }
        let mut nodes = Vec::new();
        plan(layout, value, &mut nodes)?;
        if nodes.len() > MAXIMUM_RECORD_NODES {
            return Err(VmFault::InvalidStoragePlan);
        }
        Ok(Self {
            completed: vec![None; nodes.len()],
            nodes,
            index: 0,
            field: 0,
            object: None,
            allocation: None,
            string: None,
            collection_attempted: false,
            inline_values: Vec::with_capacity(if layout.inline {
                layout.fields.len()
            } else {
                0
            }),
        })
    }

    pub(super) fn resume(
        &mut self,
        image: &ExecutionImage,
        heap: &mut Heap,
        budget: u32,
    ) -> Result<(u32, Option<RecordOutput>), TextError> {
        let mut used = 0;
        while self.index < self.nodes.len() && used < budget {
            let value = match &self.nodes[self.index] {
                PlannedNode::String(units) => {
                    if units.is_empty() {
                        used += 1;
                        Some(
                            image
                                .empty_string()
                                .ok_or(TextError::Fault(VmFault::InvalidResolvedId))?,
                        )
                    } else {
                        let pending = self
                            .string
                            .get_or_insert_with(|| PendingHostReference::new(u16::MAX));
                        let (cost, result) = match pending.resume(
                            image,
                            heap,
                            super::text::HostReferenceSource::String(units),
                            budget - used,
                        ) {
                            Err(TextError::Exhausted {
                                used: cost,
                                block_bytes,
                                requested,
                                collection_attempted,
                            }) => {
                                return Err(TextError::Exhausted {
                                    used: used + cost,
                                    block_bytes,
                                    requested,
                                    collection_attempted,
                                })
                            }
                            result => result?,
                        };
                        used += cost;
                        result.map(|(_, value)| value)
                    }
                }
                PlannedNode::Inline { fields, .. } => {
                    while self.field < fields.len() && used < budget {
                        let (_, value) = &fields[self.field];
                        let value = match value {
                            PlannedValue::Scalar(v) => *v,
                            PlannedValue::Node(index) => self
                                .completed
                                .get(*index)
                                .copied()
                                .flatten()
                                .ok_or(TextError::Fault(VmFault::CorruptLifecycle))?,
                        };
                        self.inline_values.push(value);
                        self.field += 1;
                        used += 1;
                    }
                    (self.field == fields.len()).then_some(RuntimeValue::Null)
                }
                PlannedNode::Object { ty, fields } => {
                    if self.object.is_none() {
                        if self.allocation.is_none() {
                            let super::layout::RuntimeTypeLayout::Object(layout) = image
                                .type_layout(*ty)
                                .ok_or(TextError::Fault(VmFault::InvalidResolvedId))?
                            else {
                                return Err(TextError::Fault(VmFault::InvalidValueType));
                            };
                            let request = AllocationRequest {
                                block_bytes: layout.block_bytes,
                                type_id: image
                                    .type_id(*ty)
                                    .ok_or(TextError::Fault(VmFault::InvalidResolvedId))?,
                            };
                            // Allocation and every field store have a fixed dynamic charge in addition to incremental zeroing.
                            used += 1;
                            let reservation = heap
                                .reserve(request)
                                .map_err(TextError::Fault)?
                                .ok_or(TextError::Exhausted {
                                    used,
                                    block_bytes: layout.block_bytes,
                                    requested: layout.payload_bytes,
                                    collection_attempted: self.collection_attempted,
                                })?;
                            self.allocation = Some(PendingAllocation::Object(PendingState {
                                request,
                                reservation,
                                destination: u16::MAX,
                                logical_bytes: layout.payload_bytes,
                                initialized_bytes: 0,
                                fixed_cost_paid: true,
                                collection_attempted: self.collection_attempted,
                            }));
                        }
                        let (cost, reference) = self
                            .allocation
                            .as_mut()
                            .ok_or(TextError::Fault(VmFault::CorruptLifecycle))?
                            .advance(heap, budget - used)
                            .map_err(TextError::Fault)?;
                        used += cost;
                        if let Some(reference) = reference {
                            self.object = Some(reference);
                            self.allocation = None;
                        }
                    }
                    if let Some(reference) = self.object {
                        while self.field < fields.len() && used < budget {
                            let (field, value) = &fields[self.field];
                            let value = match value {
                                PlannedValue::Scalar(v) => *v,
                                PlannedValue::Node(index) => self
                                    .completed
                                    .get(*index)
                                    .copied()
                                    .flatten()
                                    .ok_or(TextError::Fault(VmFault::CorruptLifecycle))?,
                            };
                            store_value(heap, reference, field.offset, field.width, value)
                                .map_err(TextError::Fault)?;
                            self.field += 1;
                            used += 1;
                        }
                        (self.field == fields.len()).then_some(RuntimeValue::Reference(reference))
                    } else {
                        None
                    }
                }
            };
            let Some(value) = value else {
                return Ok((used, None));
            };
            self.completed[self.index] = Some(value);
            self.index += 1;
            self.field = 0;
            self.object = None;
            self.string = None;
            self.collection_attempted = false;
        }
        Ok((
            used,
            if self.index == self.nodes.len() {
                if matches!(self.nodes.last(), Some(PlannedNode::Inline { .. })) {
                    Some(RecordOutput::Inline(std::mem::take(
                        &mut self.inline_values,
                    )))
                } else {
                    self.completed
                        .last()
                        .copied()
                        .flatten()
                        .map(RecordOutput::Scalar)
                }
            } else {
                None
            },
        ))
    }

    pub(super) fn visit_roots(&self, mut visit: impl FnMut(Ref32)) {
        for value in self.completed.iter().flatten() {
            if let RuntimeValue::Reference(reference) = value {
                visit(*reference);
            }
        }
        for value in &self.inline_values {
            if let RuntimeValue::Reference(reference) = value {
                visit(*reference);
            }
        }
        if let Some(reference) = self.object {
            visit(reference);
        }
    }

    pub(super) fn mark_collection_attempted(&mut self) {
        self.collection_attempted = true;
        if let Some(string) = self.string.as_mut() {
            string.mark_collection_attempted();
        }
    }

    pub(super) fn abort(mut self, heap: &mut Heap) -> Result<(), VmFault> {
        if let Some(allocation) = self.allocation.take() {
            allocation.abort(heap)?;
        }
        if let Some(string) = self.string.take() {
            string.abort(heap)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn test_collection_safe(&self) -> bool {
        self.allocation.is_none() && self.string.is_none()
    }

    pub(super) fn resident_bytes(&self) -> u64 {
        (self.nodes.capacity() * core::mem::size_of::<PlannedNode>()
            + self.completed.capacity() * core::mem::size_of::<Option<RuntimeValue>>()
            + self.inline_values.capacity() * core::mem::size_of::<RuntimeValue>()
            + self
                .nodes
                .iter()
                .map(|node| match node {
                    PlannedNode::String(units) => units.capacity() * 2,
                    PlannedNode::Object { fields, .. } | PlannedNode::Inline { fields, .. } => {
                        fields.capacity()
                            * core::mem::size_of::<(RecordFieldLayout, PlannedValue)>()
                    }
                })
                .sum::<usize>()) as u64
    }

    pub(super) fn allocation_kind(&self) -> super::error::AllocationRequestKind {
        match self.nodes.get(self.index) {
            Some(PlannedNode::String(_)) => super::error::AllocationRequestKind::String,
            _ => super::error::AllocationRequestKind::Object,
        }
    }
}

impl HostRecordValue {
    pub(super) fn resident_bytes(&self) -> usize {
        self.type_name.capacity()
            + self.fields.capacity() * core::mem::size_of::<HostRecordMember>()
            + self
                .fields
                .iter()
                .map(|field| {
                    field.name.capacity()
                        + match &field.value {
                            HostRecordScalar::String(units) => units.capacity() * 2,
                            HostRecordScalar::Record(child) => {
                                core::mem::size_of::<HostRecordValue>() + child.resident_bytes()
                            }
                            _ => 0,
                        }
                })
                .sum::<usize>()
    }
}

#[path = "checkpoint/record.rs"]
mod checkpoint_state;
