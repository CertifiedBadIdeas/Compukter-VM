/*
 * The Compukters Developers
 *
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use super::*;
use crate::execution::checkpoint::{checkpoint_enum, checkpoint_struct};

checkpoint_struct!(HostRecordValue { type_name, fields });

checkpoint_struct!(HostRecordMember { name, value });

checkpoint_enum!(HostRecordScalar {
    0 => I32(v0);
    1 => I64(v0);
    2 => F32(v0);
    3 => F64(v0);
    4 => Bool(v0);
    5 => Char(v0);
    6 => String(v0);
    7 => Record(v0);
});

checkpoint_struct!(RecordLayout { ty, fields });

checkpoint_struct!(RecordFieldLayout {
    offset,
    width,
    record
});

checkpoint_enum!(PlannedValue {
    0 => Scalar(v0);
    1 => Node(v0);
});

checkpoint_enum!(PlannedNode {
    0 => String(v0);
    1 => Object { ty, fields };
});

checkpoint_struct!(PendingRecord {
    nodes,
    completed,
    index,
    field,
    object,
    allocation,
    string,
    collection_attempted
});

checkpoint_struct!(HostRecordSchema { type_name, fields });
checkpoint_struct!(HostRecordField {
    name,
    value_type,
    record
});

impl PendingRecord {
    pub(in crate::execution) fn validate_checkpoint(
        &self,
        image: &ExecutionImage,
        heap: &Heap,
    ) -> crate::execution::checkpoint::Result<()> {
        use super::super::layout::ValueWidth;
        use crate::execution::checkpoint::CheckpointError;
        let invalid = || CheckpointError::InvalidState;
        if self.nodes.is_empty()
            || self.nodes.len() > MAXIMUM_RECORD_NODES
            || self.completed.len() != self.nodes.len()
            || self.index > self.nodes.len()
            || self
                .completed
                .iter()
                .enumerate()
                .any(|(index, value)| value.is_some() != (index < self.index))
        {
            return Err(invalid());
        }
        let mut total_units = 0_usize;
        for (index, node) in self.nodes.iter().enumerate() {
            match node {
                PlannedNode::String(units) => {
                    total_units = total_units.checked_add(units.len()).ok_or_else(invalid)?;
                    if total_units > MAXIMUM_RECORD_CODE_UNITS {
                        return Err(invalid());
                    }
                }
                PlannedNode::Object { ty, fields } => {
                    let layout = (0..image.type_count())
                        .filter_map(|index| image.type_key(index))
                        .filter_map(|root| image.record_layout(root))
                        .find_map(|layout| checkpoint_record_layout(layout, *ty))
                        .ok_or_else(invalid)?;
                    if fields.len() != layout.fields.len() {
                        return Err(invalid());
                    }
                    for ((field, value), expected) in fields.iter().zip(&layout.fields) {
                        if field.offset != expected.offset
                            || field.width != expected.width
                            || field.record.is_some()
                        {
                            return Err(invalid());
                        }
                        match value {
                            PlannedValue::Node(child)
                                if *child < index && field.width == ValueWidth::Ref =>
                            {
                                match (&self.nodes[*child], &expected.record) {
                                    (PlannedNode::Object { ty, .. }, Some(record))
                                        if *ty == record.ty => {}
                                    (PlannedNode::String(_), None) => {}
                                    _ => return Err(invalid()),
                                }
                            }
                            PlannedValue::Scalar(value)
                                if matches!(
                                    (field.width, value),
                                    (ValueWidth::I32, RuntimeValue::I32(_))
                                        | (ValueWidth::I64, RuntimeValue::I64(_))
                                        | (ValueWidth::F32, RuntimeValue::F32(_))
                                        | (ValueWidth::F64, RuntimeValue::F64(_))
                                        | (ValueWidth::Bool, RuntimeValue::Bool(_))
                                        | (ValueWidth::Char, RuntimeValue::Char(_))
                                ) => {}
                            _ => return Err(invalid()),
                        }
                    }
                }
            }
        }
        match self.nodes.get(self.index) {
            Some(PlannedNode::String(units)) => {
                if self.object.is_some() || self.allocation.is_some() || self.field != 0 {
                    return Err(invalid());
                }
                if let Some(string) = self.string {
                    if string.validate_checkpoint(heap, units)? != u16::MAX {
                        return Err(invalid());
                    }
                }
            }
            Some(PlannedNode::Object { ty, fields }) => {
                if self.string.is_some()
                    || self.field > fields.len()
                    || self.object.is_some() && self.allocation.is_some()
                    || self.object.is_none() && self.field != 0
                {
                    return Err(invalid());
                }
                if let Some(object) = self.object {
                    if heap.managed_type(object).ok() != image.type_id(*ty) {
                        return Err(invalid());
                    }
                }
                if let Some(allocation) = self.allocation {
                    if allocation.validate_checkpoint(image, heap)? != u16::MAX
                        || allocation.state().request.type_id
                            != image.type_id(*ty).ok_or_else(invalid)?
                    {
                        return Err(invalid());
                    }
                }
            }
            None => {
                if self.object.is_some()
                    || self.allocation.is_some()
                    || self.string.is_some()
                    || self.field != 0
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}

fn checkpoint_record_layout(
    layout: &RecordLayout,
    ty: super::super::TypeKey,
) -> Option<&RecordLayout> {
    if layout.ty == ty {
        return Some(layout);
    }
    layout
        .fields
        .iter()
        .filter_map(|field| field.record.as_deref())
        .find_map(|child| checkpoint_record_layout(child, ty))
}
