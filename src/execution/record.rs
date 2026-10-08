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
    if operation.arguments.contains(&HostValueType::Record) {
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
                    (HostValueType::Record | HostValueType::Unit, _) | (_, Some(_)) => {
                        return false
                    }
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
}

#[derive(Clone, Debug)]
pub(super) struct RecordFieldLayout {
    pub offset: u32,
    pub width: super::layout::ValueWidth,
    pub record: Option<Box<RecordLayout>>,
}

pub(super) fn admit_record(
    artifact: &crate::artifact::DecodedArtifact,
    ty: super::TypeKey,
    schema: &HostRecordSchema,
    fields: &[super::image::ResolvedField],
    field_offsets: &[usize],
    string_type: Option<super::TypeKey>,
) -> Option<RecordLayout> {
    use super::layout::ValueWidth;
    use crate::artifact::NominalType;
    let module = artifact.modules.get(ty.module as usize)?;
    let NominalType::Class {
        name,
        generic_arity,
        interfaces,
        field_start,
        field_count,
        initializer,
        ..
    } = module.types.get(ty.ty as usize)?
    else {
        return None;
    };
    if *generic_arity != 0
        || !interfaces.is_empty()
        || initializer.is_some()
        || *field_count as usize != schema.fields.len()
        || module.strings.get(*name as usize)?.slice(&artifact.bytes) != schema.type_name.as_bytes()
    {
        return None;
    }
    let declared = module
        .fields
        .get(*field_start as usize..(*field_start as usize).checked_add(*field_count as usize)?)?;
    let mut result = Vec::with_capacity(schema.fields.len());
    for expected in &schema.fields {
        let (local, field) = declared.iter().enumerate().find(|(_, field)| {
            module
                .strings
                .get(field.name as usize)
                .is_some_and(|name| name.slice(&artifact.bytes) == expected.name.as_bytes())
        })?;
        // Instance storage only. Artifact writability includes constructor initialization of Guest val fields;
        // source-level immutability is checked by SDK authoring. Physical layout rejects inherited hidden state.
        if field.flags & 2 != 0 {
            return None;
        }
        let resolved = fields.get(
            field_offsets
                .get(ty.module as usize)?
                .checked_add(*field_start as usize)?
                .checked_add(local)?,
        )?;
        if resolved.owner != ty || resolved.value_type.nullable {
            return None;
        }
        let (width, nested) = match expected.value_type {
            HostValueType::I32 if resolved.value_type.kind == 1 => (ValueWidth::I32, None),
            HostValueType::I64 if resolved.value_type.kind == 2 => (ValueWidth::I64, None),
            HostValueType::F32 if resolved.value_type.kind == 3 => (ValueWidth::F32, None),
            HostValueType::F64 if resolved.value_type.kind == 4 => (ValueWidth::F64, None),
            HostValueType::Bool if resolved.value_type.kind == 5 => (ValueWidth::Bool, None),
            HostValueType::Char if resolved.value_type.kind == 6 => (ValueWidth::Char, None),
            HostValueType::String
                if resolved.value_type.kind == 7 && resolved.value_type.nominal == string_type =>
            {
                (ValueWidth::Ref, None)
            }
            HostValueType::Record if resolved.value_type.kind == 7 => (
                ValueWidth::Ref,
                Some(Box::new(admit_record(
                    artifact,
                    resolved.value_type.nominal?,
                    expected.record.as_ref()?,
                    fields,
                    field_offsets,
                    string_type,
                )?)),
            ),
            _ => return None,
        };
        result.push(RecordFieldLayout {
            offset: resolved.offset?,
            width,
            record: nested,
        });
    }
    Some(RecordLayout { ty, fields: result })
}

use super::{
    error::VmFault,
    heap::{AllocationRequest, Heap},
    heap_ops::{store_value, PendingAllocation, PendingState},
    image::ExecutionImage,
    text::{PendingHostString, TextError},
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
}

pub(super) struct PendingRecord {
    nodes: Vec<PlannedNode>,
    completed: Vec<Option<RuntimeValue>>,
    index: usize,
    field: usize,
    object: Option<Ref32>,
    allocation: Option<PendingAllocation>,
    string: Option<PendingHostString>,
    collection_attempted: bool,
}

impl PendingRecord {
    pub(super) fn new(layout: &RecordLayout, value: HostRecordValue) -> Result<Self, VmFault> {
        fn plan(
            layout: &RecordLayout,
            value: HostRecordValue,
            nodes: &mut Vec<PlannedNode>,
        ) -> Result<usize, VmFault> {
            if layout.fields.len() != value.fields.len() {
                return Err(VmFault::InvalidValueType);
            }
            let mut fields = Vec::with_capacity(layout.fields.len());
            for (field, member) in layout.fields.iter().zip(value.fields) {
                let value = match member.value {
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
                    },
                    value,
                ));
            }
            let index = nodes.len();
            nodes.push(PlannedNode::Object {
                ty: layout.ty,
                fields,
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
        })
    }

    pub(super) fn resume(
        &mut self,
        image: &ExecutionImage,
        heap: &mut Heap,
        budget: u32,
    ) -> Result<(u32, Option<RuntimeValue>), TextError> {
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
                            .get_or_insert_with(|| PendingHostString::new(u16::MAX));
                        let (cost, result) = match pending.resume(image, heap, units, budget - used)
                        {
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
                self.completed.last().copied().flatten()
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
            + self
                .nodes
                .iter()
                .map(|node| match node {
                    PlannedNode::String(units) => units.capacity() * 2,
                    PlannedNode::Object { fields, .. } => {
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
