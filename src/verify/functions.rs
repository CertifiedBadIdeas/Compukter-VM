use std::collections::VecDeque;

use crate::{
    artifact::{
        Constant, DecodedArtifact, EntryArguments, Field, Function, Instruction, NominalType,
        ValueType,
    },
    diagnostic::{Code, Diagnostic, DiagnosticSet, Family},
    limits::ArtifactLimits,
};

use super::{
    cfg::verify_control_flow,
    exceptions::{ExceptionModel, Handler},
    modules,
};

pub(crate) fn verify_functions(
    artifact: &DecodedArtifact,
    exceptions: &ExceptionModel,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    if artifact.manifest.minimum_slice_cost < artifact.manifest.maximum_block_cost {
        return Err(failure(
            limits,
            Family::Cost,
            Code::BadCost,
            0,
            0,
            "minimum slice cost is below maximum block cost",
        ));
    }
    let entry_module = artifact.header.entry_module as usize;
    let entry_function = artifact.header.entry_function as usize;
    if artifact
        .modules
        .get(entry_module)
        .and_then(|module| module.functions.get(entry_function))
        .is_none()
    {
        return Err(failure(
            limits,
            Family::Cfg,
            Code::BadControlFlow,
            entry_module,
            entry_function,
            "entry function is out of range",
        ));
    }

    for (module_id, module) in artifact.modules.iter().enumerate() {
        for (function_id, function) in module.functions.iter().enumerate() {
            let signature = verify_signature(artifact, module_id, function_id, function, limits)?;
            if function.flags & (1 << 3) != 0 {
                if function.block_count != 0
                    || (module_id == entry_module && function_id == entry_function)
                {
                    return Err(failure(
                        limits,
                        Family::Cfg,
                        Code::BadControlFlow,
                        module_id,
                        function_id,
                        "abstract function has blocks or is the entry",
                    ));
                }
                continue;
            }
            let handlers = &exceptions.functions[module_id][function_id];
            let handler_blocks: Vec<_> = handlers
                .iter()
                .map(|handler| handler.handler_block)
                .collect();
            let control =
                verify_control_flow(module, module_id, function_id, &handler_blocks, limits)?;
            verify_dataflow(
                artifact,
                module_id,
                function_id,
                function,
                signature,
                &control.successors,
                handlers,
                limits,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn verify_entry_arguments(
    artifact: &DecodedArtifact,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let module_id = artifact.header.entry_module as usize;
    let function_id = artifact.header.entry_function as usize;
    let function = artifact
        .modules
        .get(module_id)
        .and_then(|module| module.functions.get(function_id))
        .ok_or_else(|| {
            type_failure(
                limits,
                module_id,
                function_id,
                "entry function is out of range",
            )
        })?;
    let signature = verify_signature(artifact, module_id, function_id, function, limits)?;
    if entry_arguments_match(artifact, signature.0, signature.2) {
        Ok(())
    } else {
        Err(type_failure(
            limits,
            module_id,
            function_id,
            "entry argument contract disagrees with the entry function signature",
        ))
    }
}

fn entry_arguments_match(
    artifact: &DecodedArtifact,
    signature_module: usize,
    parameters: &[ValueType],
) -> bool {
    match artifact.header.entry_arguments {
        EntryArguments::None => parameters.is_empty(),
        EntryArguments::StringArray => {
            let [parameter] = parameters else {
                return false;
            };
            if parameter.kind != 7 || parameter.flags != 0 {
                return false;
            }
            let Some((array_module, array_type)) =
                modules::resolved_type(artifact, signature_module, parameter.nominal_type)
            else {
                return false;
            };
            let NominalType::Array { element, .. } =
                &artifact.modules[array_module].types[array_type]
            else {
                return false;
            };
            if element.kind != 7 || element.flags != 0 {
                return false;
            }
            let Some((string_module, string_type)) =
                modules::resolved_type(artifact, array_module, element.nominal_type)
            else {
                return false;
            };
            let NominalType::Class { name, .. } =
                &artifact.modules[string_module].types[string_type]
            else {
                return false;
            };
            artifact.modules[string_module].strings[*name as usize].slice(&artifact.bytes)
                == b"kotlin.String"
        }
    }
}

fn verify_signature<'a>(
    artifact: &'a DecodedArtifact,
    module_id: usize,
    function_id: usize,
    function: &Function,
    limits: &ArtifactLimits,
) -> Result<(usize, &'a ValueType, &'a [ValueType]), DiagnosticSet> {
    if function.register_count as usize != function.registers.len()
        || function.parameter_count > function.register_count
    {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "function register or parameter count is inconsistent",
        ));
    }
    let identity =
        modules::resolved_type(artifact, module_id, function.signature).ok_or_else(|| {
            type_failure(
                limits,
                module_id,
                function_id,
                "function signature does not resolve",
            )
        })?;
    let NominalType::Function {
        flags,
        result,
        parameters,
        ..
    } = &artifact.modules[identity.0].types[identity.1]
    else {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "function signature is not a function type",
        ));
    };
    if function.flags & 1 != u32::from(*flags & 1)
        || parameters.len() != function.parameter_count as usize
    {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "function flags or parameter count disagree with signature",
        ));
    }
    for (register, parameter) in function.registers.iter().zip(parameters) {
        if !modules::value_types_match(artifact, module_id, *register, identity.0, *parameter) {
            return Err(type_failure(
                limits,
                module_id,
                function_id,
                "parameter register type disagrees with signature",
            ));
        }
    }
    Ok((identity.0, result, parameters))
}

#[allow(clippy::too_many_arguments)]
fn verify_dataflow(
    artifact: &DecodedArtifact,
    module_id: usize,
    function_id: usize,
    function: &Function,
    signature: (usize, &ValueType, &[ValueType]),
    successors: &[Vec<usize>],
    handlers: &[Handler],
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let words = (function.register_count as usize).div_ceil(64);
    let mut entry = vec![0_u64; words];
    for register in 0..function.parameter_count as usize {
        set(&mut entry, register);
    }
    let mut states: Vec<Option<Vec<u64>>> = vec![None; successors.len()];
    states[0] = Some(entry);
    let mut queue = VecDeque::from([0_usize]);
    let block_start = function.first_block.0 as usize;
    while let Some(local_block) = queue.pop_front() {
        let mut state = states[local_block]
            .as_ref()
            .expect("queued block state")
            .clone();
        let block_id = block_start + local_block;
        let block = &artifact.modules[module_id].blocks[block_id];
        let code = &artifact.modules[module_id].code[block_id];
        if code.instructions.len() != block.instruction_count as usize {
            return Err(failure(
                limits,
                Family::Code,
                Code::BadInstruction,
                module_id,
                function_id,
                "decoded instruction count disagrees with block",
            ));
        }
        let fixed_cost = code
            .instructions
            .iter()
            .try_fold(0_u32, |total, instruction| {
                total
                    .checked_add(instruction.fixed_cost().map_err(single)?)
                    .ok_or_else(|| {
                        failure(
                            limits,
                            Family::Cost,
                            Code::BadCost,
                            module_id,
                            function_id,
                            "block fixed cost overflows u32",
                        )
                    })
            })?;
        if fixed_cost != code.fixed_cost
            || fixed_cost != block.declared_fixed_cost
            || fixed_cost > artifact.manifest.maximum_block_cost
        {
            return Err(failure(
                limits,
                Family::Cost,
                Code::BadCost,
                module_id,
                function_id,
                "declared and recomputed block costs disagree",
            ));
        }
        let mut allocation_seen = false;
        for (instruction_index, instruction) in code.instructions.iter().enumerate() {
            let is_allocation = matches!(
                instruction,
                Instruction::NewObject { .. }
                    | Instruction::NewArray { .. }
                    | Instruction::StringConcat { .. }
                    | Instruction::StringSubstring { .. }
                    | Instruction::StringFromCharArray { .. }
            );
            if is_allocation && (instruction_index != 0 || allocation_seen) {
                return Err(code_failure(
                    limits,
                    module_id,
                    function_id,
                    "allocation must be the first and only allocating instruction in its block",
                ));
            }
            allocation_seen |= is_allocation;
            if may_throw(instruction) {
                for handler in handlers.iter().filter(|handler| {
                    block_id >= handler.protected_start && block_id < handler.protected_end
                }) {
                    let mut incoming = state.clone();
                    for parameter in 0..function.parameter_count as usize {
                        set(&mut incoming, parameter);
                    }
                    set(&mut incoming, handler.exception_register as usize);
                    let local_handler = handler.handler_block - block_start;
                    merge_state(&mut states, &mut queue, local_handler, &incoming);
                }
            }
            verify_instruction(
                artifact,
                module_id,
                function_id,
                function,
                signature,
                instruction,
                &mut state,
                limits,
            )?;
        }
        for target in &successors[local_block] {
            merge_state(&mut states, &mut queue, *target, &state);
        }
    }
    if states.iter().any(Option::is_none) {
        return Err(failure(
            limits,
            Family::Cfg,
            Code::BadControlFlow,
            module_id,
            function_id,
            "exception handler has no potentially throwing predecessor",
        ));
    }
    Ok(())
}

fn merge_state(
    states: &mut [Option<Vec<u64>>],
    queue: &mut VecDeque<usize>,
    target: usize,
    incoming: &[u64],
) {
    match &mut states[target] {
        None => {
            states[target] = Some(incoming.to_vec());
            queue.push_back(target);
        }
        Some(existing) => {
            let mut changed = false;
            for (word, incoming) in existing.iter_mut().zip(incoming) {
                let intersection = *word & *incoming;
                changed |= intersection != *word;
                *word = intersection;
            }
            if changed {
                queue.push_back(target);
            }
        }
    }
}

fn may_throw(instruction: &Instruction) -> bool {
    matches!(
        instruction,
        Instruction::Convert { .. }
            | Instruction::Div { .. }
            | Instruction::Rem { .. }
            | Instruction::NewObject { .. }
            | Instruction::NewArray { .. }
            | Instruction::ArrayLength { .. }
            | Instruction::ArrayLoad { .. }
            | Instruction::ArrayStore { .. }
            | Instruction::FieldGet { .. }
            | Instruction::FieldSet { .. }
            | Instruction::StaticGet { .. }
            | Instruction::StaticSet { .. }
            | Instruction::CheckedCast { .. }
            | Instruction::CallDirect { .. }
            | Instruction::CallVirtual { .. }
            | Instruction::CallInterface { .. }
            | Instruction::CoroutineSpawn { .. }
            | Instruction::CapabilityCallSync { .. }
            | Instruction::Throw { .. }
            | Instruction::CallSuspend { .. }
            | Instruction::Sleep { .. }
            | Instruction::CoroutineJoin { .. }
            | Instruction::CapabilityCallAsync { .. }
    )
}

#[allow(clippy::too_many_arguments)]
fn verify_instruction(
    artifact: &DecodedArtifact,
    module_id: usize,
    function_id: usize,
    function: &Function,
    signature: (usize, &ValueType, &[ValueType]),
    instruction: &Instruction,
    state: &mut [u64],
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    if function.flags & 1 == 0
        && matches!(
            instruction,
            Instruction::CallSuspend { .. }
                | Instruction::Yield { .. }
                | Instruction::Sleep { .. }
                | Instruction::CoroutineJoin { .. }
        )
    {
        return Err(failure(
            limits,
            Family::Cfg,
            Code::BadControlFlow,
            module_id,
            function_id,
            "suspending terminator appears in a non-suspending function",
        ));
    }
    match instruction {
        Instruction::Nop | Instruction::Jump { .. } | Instruction::Unreachable => {}
        Instruction::Move { dst, src } => {
            read(function, state, *src, module_id, function_id, limits)?;
            let dst_type = register_type(function, *dst, module_id, function_id, limits)?;
            let src_type = register_type(function, *src, module_id, function_id, limits)?;
            if !modules::value_types_match(artifact, module_id, dst_type, module_id, src_type) {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "move source and destination types differ",
                ));
            }
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::Const { dst, constant } => {
            let value = artifact.modules[module_id]
                .constants
                .get(*constant as usize)
                .ok_or_else(|| {
                    code_failure(
                        limits,
                        module_id,
                        function_id,
                        "constant id is out of range",
                    )
                })?;
            let destination = register_type(function, *dst, module_id, function_id, limits)?;
            if !constant_assignable(value, destination) {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "constant is not assignable to destination",
                ));
            }
            if matches!(value, Constant::String(_)) {
                require_string(artifact, function, *dst, module_id, function_id, limits)?;
            }
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::Null { dst } => {
            let destination = register_type(function, *dst, module_id, function_id, limits)?;
            if destination.kind != 7 || destination.flags & 1 == 0 {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "null destination is not a nullable reference",
                ));
            }
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::Convert { dst, src } => {
            read(function, state, *src, module_id, function_id, limits)?;
            let source = register_type(function, *src, module_id, function_id, limits)?;
            let destination = register_type(function, *dst, module_id, function_id, limits)?;
            if !convertible(source.kind, destination.kind) {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "convert register types are incompatible",
                ));
            }
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::Add {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::Sub {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::Mul {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::Div {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::Rem {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::BitAnd {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::BitOr {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::BitXor {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::ShiftLeft {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::ShiftRight {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::ShiftUnsigned {
            form,
            dst,
            lhs,
            rhs,
        } => {
            read(function, state, *lhs, module_id, function_id, limits)?;
            read(function, state, *rhs, module_id, function_id, limits)?;
            require_kind(function, *dst, *form, module_id, function_id, limits)?;
            require_kind(function, *lhs, *form, module_id, function_id, limits)?;
            let rhs_kind = if matches!(
                instruction,
                Instruction::ShiftLeft { .. }
                    | Instruction::ShiftRight { .. }
                    | Instruction::ShiftUnsigned { .. }
            ) {
                1
            } else {
                *form
            };
            require_kind(function, *rhs, rhs_kind, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::Neg { form, dst, src } => {
            read(function, state, *src, module_id, function_id, limits)?;
            require_kind(function, *src, *form, module_id, function_id, limits)?;
            require_kind(function, *dst, *form, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::Equal {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::NotEqual {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::Less {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::LessEqual {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::Greater {
            form,
            dst,
            lhs,
            rhs,
        }
        | Instruction::GreaterEqual {
            form,
            dst,
            lhs,
            rhs,
        } => {
            read(function, state, *lhs, module_id, function_id, limits)?;
            read(function, state, *rhs, module_id, function_id, limits)?;
            require_kind(function, *lhs, *form, module_id, function_id, limits)?;
            require_kind(function, *rhs, *form, module_id, function_id, limits)?;
            require_kind(function, *dst, 5, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::RefEqual { dst, lhs, rhs } | Instruction::RefNotEqual { dst, lhs, rhs } => {
            read(function, state, *lhs, module_id, function_id, limits)?;
            read(function, state, *rhs, module_id, function_id, limits)?;
            let left = require_kind(function, *lhs, 7, module_id, function_id, limits)?;
            let right = require_kind(function, *rhs, 7, module_id, function_id, limits)?;
            if modules::resolved_type(artifact, module_id, left.nominal_type)
                != modules::resolved_type(artifact, module_id, right.nominal_type)
            {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "reference comparison types are incompatible",
                ));
            }
            require_kind(function, *dst, 5, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::NewObject { dst, type_ref } => {
            let identity =
                resolve_type_operand(artifact, module_id, *type_ref, limits, function_id)?;
            if !matches!(artifact.modules[identity.0].types[identity.1], NominalType::Class { flags, .. } if flags & 1 == 0)
            {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "new_object target is not a concrete class",
                ));
            }
            require_reference_destination(
                artifact,
                module_id,
                function,
                *dst,
                identity,
                function_id,
                limits,
            )?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::NewArray {
            dst,
            type_ref,
            length,
        } => {
            read(function, state, *length, module_id, function_id, limits)?;
            require_kind(function, *length, 1, module_id, function_id, limits)?;
            let identity =
                resolve_type_operand(artifact, module_id, *type_ref, limits, function_id)?;
            if !matches!(
                artifact.modules[identity.0].types[identity.1],
                NominalType::Array { .. }
            ) {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "new_array target is not an array type",
                ));
            }
            require_reference_destination(
                artifact,
                module_id,
                function,
                *dst,
                identity,
                function_id,
                limits,
            )?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::ArrayLength { dst, array } => {
            array_element(
                artifact,
                module_id,
                function_id,
                function,
                state,
                *array,
                limits,
            )?;
            require_kind(function, *dst, 1, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::ArrayLoad { dst, array, index } => {
            let (type_module, element) = array_element(
                artifact,
                module_id,
                function_id,
                function,
                state,
                *array,
                limits,
            )?;
            read(function, state, *index, module_id, function_id, limits)?;
            require_kind(function, *index, 1, module_id, function_id, limits)?;
            let destination = register_type(function, *dst, module_id, function_id, limits)?;
            if !value_assignable(artifact, type_module, element, module_id, destination) {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "array element is not assignable to destination",
                ));
            }
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::ArrayStore {
            array,
            index,
            value,
        } => {
            let (type_module, element) = array_element(
                artifact,
                module_id,
                function_id,
                function,
                state,
                *array,
                limits,
            )?;
            read(function, state, *index, module_id, function_id, limits)?;
            read(function, state, *value, module_id, function_id, limits)?;
            require_kind(function, *index, 1, module_id, function_id, limits)?;
            let source = register_type(function, *value, module_id, function_id, limits)?;
            if !value_assignable(artifact, module_id, source, type_module, element) {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "array store value has the wrong type",
                ));
            }
        }
        Instruction::FieldGet {
            dst,
            receiver,
            field_ref,
        } => {
            let (field_module, field) =
                resolve_field(artifact, module_id, *field_ref, function_id, limits)?;
            if field.flags & 2 != 0 {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "field_get names a static field",
                ));
            }
            verify_receiver(
                artifact,
                module_id,
                function_id,
                function,
                state,
                *receiver,
                field_module,
                field.owner,
                limits,
            )?;
            let destination = register_type(function, *dst, module_id, function_id, limits)?;
            if !value_assignable(
                artifact,
                field_module,
                field.value_type,
                module_id,
                destination,
            ) {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "field value is not assignable to destination",
                ));
            }
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::FieldSet {
            receiver,
            field_ref,
            value,
        } => {
            let (field_module, field) =
                resolve_field(artifact, module_id, *field_ref, function_id, limits)?;
            if field.flags & 1 == 0 || field.flags & 2 != 0 {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "field_set requires a mutable instance field",
                ));
            }
            verify_receiver(
                artifact,
                module_id,
                function_id,
                function,
                state,
                *receiver,
                field_module,
                field.owner,
                limits,
            )?;
            read(function, state, *value, module_id, function_id, limits)?;
            let source = register_type(function, *value, module_id, function_id, limits)?;
            if !value_assignable(artifact, module_id, source, field_module, field.value_type) {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "field store value has the wrong type",
                ));
            }
        }
        Instruction::StaticGet { dst, field_ref } => {
            let (field_module, field) =
                resolve_field(artifact, module_id, *field_ref, function_id, limits)?;
            if field.flags & 2 == 0 {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "static_get names an instance field",
                ));
            }
            let destination = register_type(function, *dst, module_id, function_id, limits)?;
            if !value_assignable(
                artifact,
                field_module,
                field.value_type,
                module_id,
                destination,
            ) {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "static field value has the wrong type",
                ));
            }
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::StaticSet { field_ref, value } => {
            let (field_module, field) =
                resolve_field(artifact, module_id, *field_ref, function_id, limits)?;
            if field.flags & 3 != 3 {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "static_set requires a mutable static field",
                ));
            }
            read(function, state, *value, module_id, function_id, limits)?;
            let source = register_type(function, *value, module_id, function_id, limits)?;
            if !value_assignable(artifact, module_id, source, field_module, field.value_type) {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "static field store has the wrong type",
                ));
            }
        }
        Instruction::IsType {
            dst,
            value,
            type_ref,
        } => {
            read(function, state, *value, module_id, function_id, limits)?;
            require_kind(function, *value, 7, module_id, function_id, limits)?;
            resolve_type_operand(artifact, module_id, *type_ref, limits, function_id)?;
            require_kind(function, *dst, 5, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::CheckedCast {
            dst,
            value,
            type_ref,
        } => {
            read(function, state, *value, module_id, function_id, limits)?;
            require_kind(function, *value, 7, module_id, function_id, limits)?;
            let identity =
                resolve_type_operand(artifact, module_id, *type_ref, limits, function_id)?;
            require_reference_destination(
                artifact,
                module_id,
                function,
                *dst,
                identity,
                function_id,
                limits,
            )?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::CallDirect {
            dst,
            function_ref,
            args,
        } => {
            verify_call(
                artifact,
                module_id,
                function_id,
                function,
                state,
                *dst,
                *function_ref,
                args,
                CallKind::Direct,
                limits,
            )?;
        }
        Instruction::CallVirtual {
            dst,
            function_ref,
            args,
        } => {
            verify_call(
                artifact,
                module_id,
                function_id,
                function,
                state,
                *dst,
                *function_ref,
                args,
                CallKind::Virtual,
                limits,
            )?;
        }
        Instruction::CallInterface {
            dst,
            function_ref,
            args,
        } => {
            verify_call(
                artifact,
                module_id,
                function_id,
                function,
                state,
                *dst,
                *function_ref,
                args,
                CallKind::Interface,
                limits,
            )?;
        }
        Instruction::CoroutineSpawn {
            dst,
            function_ref,
            args,
        } => {
            let (target_module, target) =
                resolve_function(artifact, module_id, *function_ref, function_id, limits)?;
            if target.flags & 1 == 0 {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "coroutine_spawn target is not suspending",
                ));
            }
            verify_call_arguments(
                artifact,
                module_id,
                function_id,
                function,
                state,
                target_module,
                target,
                args,
                limits,
            )?;
            require_kind(function, *dst, 7, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::CapabilityCallSync {
            dst,
            capability,
            operation,
            args,
        } => {
            verify_capability(
                artifact,
                module_id,
                function_id,
                *capability,
                *operation,
                limits,
            )?;
            for argument in args.iter() {
                read(function, state, *argument, module_id, function_id, limits)?;
            }
            if *dst != u16::MAX {
                write(state, *dst, function, module_id, function_id, limits)?;
            }
        }
        Instruction::Branch { condition, .. } => {
            read(function, state, *condition, module_id, function_id, limits)?;
            require_kind(function, *condition, 5, module_id, function_id, limits)?;
        }
        Instruction::SwitchI32 { key, .. } => {
            read(function, state, *key, module_id, function_id, limits)?;
            require_kind(function, *key, 1, module_id, function_id, limits)?;
        }
        Instruction::Return { value } => {
            if signature.1.kind == 0 {
                if *value != u16::MAX {
                    return Err(type_failure(
                        limits,
                        module_id,
                        function_id,
                        "unit return contains a value",
                    ));
                }
            } else {
                read(function, state, *value, module_id, function_id, limits)?;
                let value_type = register_type(function, *value, module_id, function_id, limits)?;
                if !modules::value_types_match(
                    artifact,
                    module_id,
                    value_type,
                    signature.0,
                    *signature.1,
                ) {
                    return Err(type_failure(
                        limits,
                        module_id,
                        function_id,
                        "return value type disagrees with signature",
                    ));
                }
            }
        }
        Instruction::Throw { exception } => {
            read(function, state, *exception, module_id, function_id, limits)?;
            let value = require_kind(function, *exception, 7, module_id, function_id, limits)?;
            if value.flags & 1 != 0 {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "throw operand is nullable",
                ));
            }
        }
        Instruction::CallSuspend {
            dst,
            function_ref,
            args,
            ..
        } => {
            verify_call(
                artifact,
                module_id,
                function_id,
                function,
                state,
                *dst,
                *function_ref,
                args,
                CallKind::Suspend,
                limits,
            )?;
        }
        Instruction::Yield { .. } => {}
        Instruction::Sleep { duration, .. } => {
            read(function, state, *duration, module_id, function_id, limits)?;
            require_kind(function, *duration, 2, module_id, function_id, limits)?;
        }
        Instruction::CoroutineJoin { dst, coroutine, .. } => {
            read(function, state, *coroutine, module_id, function_id, limits)?;
            require_kind(function, *coroutine, 7, module_id, function_id, limits)?;
            if *dst != u16::MAX {
                write(state, *dst, function, module_id, function_id, limits)?;
            }
        }
        Instruction::CapabilityCallAsync {
            dst,
            capability,
            operation,
            args,
            ..
        } => {
            verify_capability(
                artifact,
                module_id,
                function_id,
                *capability,
                *operation,
                limits,
            )?;
            for argument in args.iter() {
                read(function, state, *argument, module_id, function_id, limits)?;
            }
            if *dst != u16::MAX {
                write(state, *dst, function, module_id, function_id, limits)?;
            }
        }
        Instruction::StringLength { dst, string } => {
            read(function, state, *string, module_id, function_id, limits)?;
            require_string(artifact, function, *string, module_id, function_id, limits)?;
            require_kind(function, *dst, 1, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::StringGet { dst, string, index } => {
            read(function, state, *string, module_id, function_id, limits)?;
            read(function, state, *index, module_id, function_id, limits)?;
            require_string(artifact, function, *string, module_id, function_id, limits)?;
            require_kind(function, *index, 1, module_id, function_id, limits)?;
            require_kind(function, *dst, 6, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::StringEquals { dst, lhs, rhs } => {
            read(function, state, *lhs, module_id, function_id, limits)?;
            read(function, state, *rhs, module_id, function_id, limits)?;
            require_string(artifact, function, *lhs, module_id, function_id, limits)?;
            require_string(artifact, function, *rhs, module_id, function_id, limits)?;
            require_kind(function, *dst, 5, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::StringCompare { dst, lhs, rhs } => {
            read(function, state, *lhs, module_id, function_id, limits)?;
            read(function, state, *rhs, module_id, function_id, limits)?;
            require_string(artifact, function, *lhs, module_id, function_id, limits)?;
            require_string(artifact, function, *rhs, module_id, function_id, limits)?;
            require_kind(function, *dst, 1, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::StringHash { dst, string } => {
            read(function, state, *string, module_id, function_id, limits)?;
            require_string(artifact, function, *string, module_id, function_id, limits)?;
            require_kind(function, *dst, 1, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::StringConcat { dst, lhs, rhs } => {
            read(function, state, *lhs, module_id, function_id, limits)?;
            read(function, state, *rhs, module_id, function_id, limits)?;
            require_string(artifact, function, *lhs, module_id, function_id, limits)?;
            require_string(artifact, function, *rhs, module_id, function_id, limits)?;
            require_string(artifact, function, *dst, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::StringSubstring {
            dst,
            string,
            start,
            end,
        } => {
            read(function, state, *string, module_id, function_id, limits)?;
            read(function, state, *start, module_id, function_id, limits)?;
            read(function, state, *end, module_id, function_id, limits)?;
            require_string(artifact, function, *string, module_id, function_id, limits)?;
            require_kind(function, *start, 1, module_id, function_id, limits)?;
            require_kind(function, *end, 1, module_id, function_id, limits)?;
            require_string(artifact, function, *dst, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
        Instruction::StringFromCharArray {
            dst,
            array,
            start,
            end,
        } => {
            let (_, element) = array_element(
                artifact,
                module_id,
                function_id,
                function,
                state,
                *array,
                limits,
            )?;
            if element.kind != 6 {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "string source is not a CharArray",
                ));
            }
            read(function, state, *start, module_id, function_id, limits)?;
            read(function, state, *end, module_id, function_id, limits)?;
            require_kind(function, *start, 1, module_id, function_id, limits)?;
            require_kind(function, *end, 1, module_id, function_id, limits)?;
            require_string(artifact, function, *dst, module_id, function_id, limits)?;
            write(state, *dst, function, module_id, function_id, limits)?;
        }
    }
    Ok(())
}

fn require_string(
    artifact: &DecodedArtifact,
    function: &Function,
    register: u16,
    module_id: usize,
    function_id: usize,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let value = register_type(function, register, module_id, function_id, limits)?;
    let actual = modules::resolved_type(artifact, module_id, value.nominal_type);
    let expected = resolve_string_type(artifact, module_id, function_id, limits)?;
    if value.kind != 7 || value.flags & 1 != 0 || actual != Some(expected) {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "register is not the non-null standard-library String type",
        ));
    }
    Ok(())
}

fn resolve_string_type(
    artifact: &DecodedArtifact,
    module_id: usize,
    function_id: usize,
    limits: &ArtifactLimits,
) -> Result<(usize, usize), DiagnosticSet> {
    let mut found = None;
    for (candidate_module_id, module) in artifact.modules.iter().enumerate() {
        if module.flags != 2 {
            continue;
        }
        for export in &module.exports {
            let is_string = export.kind == 0
                && export.visibility == 1
                && module
                    .strings
                    .get(export.name as usize)
                    .is_some_and(|name| name.slice(&artifact.bytes) == b"kotlin.String");
            if !is_string {
                continue;
            }
            let candidate = (candidate_module_id, export.local_symbol as usize);
            if found.replace(candidate).is_some() {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "artifact has more than one public kotlin.String type",
                ));
            }
        }
    }
    let string_type = found.ok_or_else(|| {
        type_failure(
            limits,
            module_id,
            function_id,
            "artifact has no public kotlin.String type",
        )
    })?;
    let Some(NominalType::Class {
        flags, field_count, ..
    }) = artifact.modules[string_type.0].types.get(string_type.1)
    else {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "kotlin.String export does not resolve to a class",
        ));
    };
    if flags & 1 != 0 || flags & 2 == 0 || *field_count != 0 {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "kotlin.String must be final, concrete, and fieldless",
        ));
    }
    Ok(string_type)
}

#[derive(Clone, Copy)]
enum CallKind {
    Direct,
    Virtual,
    Interface,
    Suspend,
}

#[allow(clippy::too_many_arguments)]
fn verify_call(
    artifact: &DecodedArtifact,
    module_id: usize,
    function_id: usize,
    caller: &Function,
    state: &mut [u64],
    dst: u16,
    reference: u32,
    args: &[u16],
    kind: CallKind,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let (target_module, target) =
        resolve_function(artifact, module_id, reference, function_id, limits)?;
    if target.flags & (1 << 3) != 0 && matches!(kind, CallKind::Direct) {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "direct call targets an abstract function",
        ));
    }
    match kind {
        CallKind::Direct if target.flags & 1 != 0 => {
            return Err(type_failure(
                limits,
                module_id,
                function_id,
                "call_direct targets a suspending function",
            ));
        }
        CallKind::Virtual if target.flags & (1 << 2) == 0 => {
            return Err(type_failure(
                limits,
                module_id,
                function_id,
                "call_virtual target is not virtual",
            ));
        }
        CallKind::Interface => {
            let owner =
                modules::resolved_type(artifact, target_module, target.owner).ok_or_else(|| {
                    type_failure(
                        limits,
                        module_id,
                        function_id,
                        "interface call owner does not resolve",
                    )
                })?;
            if !matches!(
                artifact.modules[owner.0].types[owner.1],
                NominalType::Interface { .. }
            ) {
                return Err(type_failure(
                    limits,
                    module_id,
                    function_id,
                    "call_interface target owner is not an interface",
                ));
            }
        }
        CallKind::Suspend if target.flags & 1 == 0 => {
            return Err(type_failure(
                limits,
                module_id,
                function_id,
                "call_suspend target is not suspending",
            ));
        }
        _ => {}
    }
    let result = verify_call_arguments(
        artifact,
        module_id,
        function_id,
        caller,
        state,
        target_module,
        target,
        args,
        limits,
    )?;
    if result.1.kind == 0 {
        if dst != u16::MAX {
            return Err(type_failure(
                limits,
                module_id,
                function_id,
                "unit call has a destination register",
            ));
        }
    } else {
        if dst == u16::MAX {
            return Err(type_failure(
                limits,
                module_id,
                function_id,
                "non-unit call omits its destination",
            ));
        }
        let destination = register_type(caller, dst, module_id, function_id, limits)?;
        if !value_assignable(artifact, result.0, *result.1, module_id, destination) {
            return Err(type_failure(
                limits,
                module_id,
                function_id,
                "call result is not assignable to destination",
            ));
        }
        write(state, dst, caller, module_id, function_id, limits)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_call_arguments<'a>(
    artifact: &'a DecodedArtifact,
    module_id: usize,
    function_id: usize,
    caller: &Function,
    state: &[u64],
    target_module: usize,
    target: &Function,
    args: &[u16],
    limits: &ArtifactLimits,
) -> Result<(usize, &'a ValueType), DiagnosticSet> {
    let identity =
        modules::resolved_type(artifact, target_module, target.signature).ok_or_else(|| {
            type_failure(
                limits,
                module_id,
                function_id,
                "call target signature does not resolve",
            )
        })?;
    let NominalType::Function {
        result, parameters, ..
    } = &artifact.modules[identity.0].types[identity.1]
    else {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "call target signature is not a function type",
        ));
    };
    if args.len() != parameters.len() {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "call argument count disagrees with signature",
        ));
    }
    for (argument, parameter) in args.iter().zip(parameters) {
        read(caller, state, *argument, module_id, function_id, limits)?;
        let argument_type = register_type(caller, *argument, module_id, function_id, limits)?;
        if !value_assignable(artifact, module_id, argument_type, identity.0, *parameter) {
            return Err(type_failure(
                limits,
                module_id,
                function_id,
                "call argument type disagrees with signature",
            ));
        }
    }
    Ok((identity.0, result))
}

fn resolve_function<'a>(
    artifact: &'a DecodedArtifact,
    module_id: usize,
    reference: u32,
    function_id: usize,
    limits: &ArtifactLimits,
) -> Result<(usize, &'a Function), DiagnosticSet> {
    let (target_module, symbol) =
        resolve_symbol(artifact, module_id, reference, 1, function_id, limits)?;
    artifact.modules[target_module]
        .functions
        .get(symbol)
        .map(|function| (target_module, function))
        .ok_or_else(|| {
            code_failure(
                limits,
                module_id,
                function_id,
                "function reference is out of range",
            )
        })
}

fn resolve_field<'a>(
    artifact: &'a DecodedArtifact,
    module_id: usize,
    reference: u32,
    function_id: usize,
    limits: &ArtifactLimits,
) -> Result<(usize, &'a Field), DiagnosticSet> {
    let (target_module, symbol) =
        resolve_symbol(artifact, module_id, reference, 2, function_id, limits)?;
    artifact.modules[target_module]
        .fields
        .get(symbol)
        .map(|field| (target_module, field))
        .ok_or_else(|| {
            code_failure(
                limits,
                module_id,
                function_id,
                "field reference is out of range",
            )
        })
}

fn resolve_symbol(
    artifact: &DecodedArtifact,
    module_id: usize,
    reference: u32,
    kind: u8,
    function_id: usize,
    limits: &ArtifactLimits,
) -> Result<(usize, usize), DiagnosticSet> {
    if reference == u32::MAX {
        return Err(code_failure(
            limits,
            module_id,
            function_id,
            "symbol reference uses the absent sentinel",
        ));
    }
    if reference & 0x8000_0000 == 0 {
        return Ok((module_id, reference as usize));
    }
    let import = artifact.modules[module_id]
        .imports
        .get((reference & 0x7fff_ffff) as usize)
        .ok_or_else(|| {
            code_failure(
                limits,
                module_id,
                function_id,
                "import reference is out of range",
            )
        })?;
    if import.kind != kind {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "import reference has the wrong symbol kind",
        ));
    }
    let target_module = import.target_module.0 as usize;
    let target = artifact.modules.get(target_module).ok_or_else(|| {
        code_failure(
            limits,
            module_id,
            function_id,
            "import target module is out of range",
        )
    })?;
    target
        .exports
        .iter()
        .find(|export| export.kind == kind && export.name == import.target_name)
        .map(|export| (target_module, export.local_symbol as usize))
        .ok_or_else(|| {
            code_failure(
                limits,
                module_id,
                function_id,
                "imported symbol does not resolve",
            )
        })
}

fn resolve_type_operand(
    artifact: &DecodedArtifact,
    module_id: usize,
    reference: u32,
    limits: &ArtifactLimits,
    function_id: usize,
) -> Result<(usize, usize), DiagnosticSet> {
    modules::resolved_type(artifact, module_id, crate::artifact::TypeId(reference)).ok_or_else(
        || {
            type_failure(
                limits,
                module_id,
                function_id,
                "type operand does not resolve",
            )
        },
    )
}

fn constant_assignable(constant: &Constant, destination: ValueType) -> bool {
    match constant {
        Constant::I32(_) => destination.kind == 1,
        Constant::I64(_) => destination.kind == 2,
        Constant::F32(_) => destination.kind == 3,
        Constant::F64(_) => destination.kind == 4,
        Constant::Bool(_) => destination.kind == 5,
        Constant::Char(_) => destination.kind == 6,
        Constant::String(_) => destination.kind == 7,
        Constant::Null => destination.kind == 7 && destination.flags & 1 != 0,
    }
}

fn numeric(kind: u8) -> bool {
    matches!(kind, 1..=4)
}

fn convertible(source: u8, destination: u8) -> bool {
    numeric(source) && numeric(destination) || matches!((source, destination), (1, 6) | (6, 1))
}

fn require_kind(
    function: &Function,
    register: u16,
    kind: u8,
    module_id: usize,
    function_id: usize,
    limits: &ArtifactLimits,
) -> Result<ValueType, DiagnosticSet> {
    let value = register_type(function, register, module_id, function_id, limits)?;
    if value.kind == kind {
        Ok(value)
    } else {
        Err(type_failure(
            limits,
            module_id,
            function_id,
            "register has the wrong value kind",
        ))
    }
}

pub(super) fn value_assignable(
    artifact: &DecodedArtifact,
    source_module: usize,
    source: ValueType,
    destination_module: usize,
    destination: ValueType,
) -> bool {
    source.kind == destination.kind
        && if source.kind == 7 {
            (source.flags & 1 == 0 || destination.flags & 1 != 0)
                && modules::resolved_type(artifact, source_module, source.nominal_type)
                    .zip(modules::resolved_type(
                        artifact,
                        destination_module,
                        destination.nominal_type,
                    ))
                    .is_some_and(|(source, destination)| {
                        nominal_assignable(artifact, source, destination)
                    })
        } else {
            true
        }
}

fn nominal_assignable(
    artifact: &DecodedArtifact,
    source: (usize, usize),
    destination: (usize, usize),
) -> bool {
    let mut stack = vec![source];
    let mut visited = Vec::new();
    while let Some(identity) = stack.pop() {
        if identity == destination {
            return true;
        }
        if visited.contains(&identity) {
            continue;
        }
        visited.push(identity);
        match &artifact.modules[identity.0].types[identity.1] {
            NominalType::Class {
                super_type,
                interfaces,
                ..
            }
            | NominalType::Interface {
                super_type,
                interfaces,
                ..
            } => {
                if super_type.0 != u32::MAX {
                    if let Some(parent) = modules::resolved_type(artifact, identity.0, *super_type)
                    {
                        stack.push(parent);
                    }
                }
                stack.extend(interfaces.iter().filter_map(|interface| {
                    modules::resolved_type(artifact, identity.0, *interface)
                }));
            }
            NominalType::Array { .. } | NominalType::Function { .. } => {}
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn require_reference_destination(
    artifact: &DecodedArtifact,
    module_id: usize,
    function: &Function,
    dst: u16,
    identity: (usize, usize),
    function_id: usize,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let destination = require_kind(function, dst, 7, module_id, function_id, limits)?;
    let source = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: crate::artifact::TypeId(identity.1 as u32),
    };
    if !value_assignable(artifact, identity.0, source, module_id, destination) {
        Err(type_failure(
            limits,
            module_id,
            function_id,
            "reference destination has an incompatible nominal type",
        ))
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn array_element(
    artifact: &DecodedArtifact,
    module_id: usize,
    function_id: usize,
    function: &Function,
    state: &[u64],
    array: u16,
    limits: &ArtifactLimits,
) -> Result<(usize, ValueType), DiagnosticSet> {
    read(function, state, array, module_id, function_id, limits)?;
    let array_type = require_kind(function, array, 7, module_id, function_id, limits)?;
    if array_type.flags & 1 != 0 {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "array receiver is nullable",
        ));
    }
    let identity = modules::resolved_type(artifact, module_id, array_type.nominal_type)
        .ok_or_else(|| {
            type_failure(
                limits,
                module_id,
                function_id,
                "array type does not resolve",
            )
        })?;
    let NominalType::Array { element, .. } = artifact.modules[identity.0].types[identity.1] else {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "array register is not an array type",
        ));
    };
    Ok((identity.0, element))
}

#[allow(clippy::too_many_arguments)]
fn verify_receiver(
    artifact: &DecodedArtifact,
    module_id: usize,
    function_id: usize,
    function: &Function,
    state: &[u64],
    receiver: u16,
    owner_module: usize,
    owner: crate::artifact::TypeId,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    read(function, state, receiver, module_id, function_id, limits)?;
    let receiver_type = require_kind(function, receiver, 7, module_id, function_id, limits)?;
    if receiver_type.flags & 1 != 0 {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "field receiver is nullable",
        ));
    }
    let owner_type = ValueType {
        kind: 7,
        flags: 0,
        nominal_type: owner,
    };
    if !value_assignable(artifact, module_id, receiver_type, owner_module, owner_type) {
        return Err(type_failure(
            limits,
            module_id,
            function_id,
            "field receiver has an incompatible type",
        ));
    }
    Ok(())
}

fn code_failure(
    limits: &ArtifactLimits,
    module: usize,
    function: usize,
    detail: &'static str,
) -> DiagnosticSet {
    failure(
        limits,
        Family::Code,
        Code::BadInstruction,
        module,
        function,
        detail,
    )
}

fn verify_capability(
    artifact: &DecodedArtifact,
    module: usize,
    function: usize,
    capability: u32,
    operation: u32,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let descriptor = artifact
        .capabilities
        .get(capability as usize)
        .ok_or_else(|| {
            capability_failure(limits, module, function, "capability id is out of range")
        })?;
    if operation >= descriptor.operation_count {
        Err(capability_failure(
            limits,
            module,
            function,
            "capability operation id is out of range",
        ))
    } else {
        Ok(())
    }
}

fn capability_failure(
    limits: &ArtifactLimits,
    module: usize,
    function: usize,
    detail: &'static str,
) -> DiagnosticSet {
    failure(
        limits,
        Family::Capability,
        Code::BadCapability,
        module,
        function,
        detail,
    )
}

fn register_type(
    function: &Function,
    register: u16,
    module: usize,
    function_id: usize,
    limits: &ArtifactLimits,
) -> Result<ValueType, DiagnosticSet> {
    function
        .registers
        .get(register as usize)
        .copied()
        .ok_or_else(|| {
            failure(
                limits,
                Family::Register,
                Code::BadInstruction,
                module,
                function_id,
                "register is out of range",
            )
        })
}

fn read(
    function: &Function,
    state: &[u64],
    register: u16,
    module: usize,
    function_id: usize,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    register_type(function, register, module, function_id, limits)?;
    if !get(state, register as usize) {
        Err(failure(
            limits,
            Family::Register,
            Code::UninitializedRegister,
            module,
            function_id,
            "instruction reads an uninitialized register",
        ))
    } else {
        Ok(())
    }
}

fn write(
    state: &mut [u64],
    register: u16,
    function: &Function,
    module: usize,
    function_id: usize,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    register_type(function, register, module, function_id, limits)?;
    set(state, register as usize);
    Ok(())
}

fn get(bits: &[u64], bit: usize) -> bool {
    bits.get(bit / 64)
        .is_some_and(|word| word & (1_u64 << (bit % 64)) != 0)
}

fn set(bits: &mut [u64], bit: usize) {
    bits[bit / 64] |= 1_u64 << (bit % 64);
}

fn type_failure(
    limits: &ArtifactLimits,
    module: usize,
    function: usize,
    detail: &'static str,
) -> DiagnosticSet {
    failure(
        limits,
        Family::Type,
        Code::BadType,
        module,
        function,
        detail,
    )
}

fn failure(
    limits: &ArtifactLimits,
    family: Family,
    code: Code,
    module: usize,
    function: usize,
    detail: &'static str,
) -> DiagnosticSet {
    let mut diagnostic = Diagnostic::at_offset(family, code, 0, detail);
    diagnostic.location.module = u32::try_from(module).ok();
    diagnostic.location.function = u32::try_from(function).ok();
    let mut errors = DiagnosticSet::new(limits.diagnostics);
    errors.push(diagnostic);
    errors
}

fn single(diagnostic: Diagnostic) -> DiagnosticSet {
    let mut errors = DiagnosticSet::new(1);
    errors.push(diagnostic);
    errors
}
