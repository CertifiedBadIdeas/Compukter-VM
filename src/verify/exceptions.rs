use crate::{
    artifact::{DecodedArtifact, Instruction, NominalType},
    diagnostic::{Code, Diagnostic, DiagnosticSet, Family},
    limits::ArtifactLimits,
};

use super::{functions, modules};

pub(crate) fn verify_throwable_root(
    artifact: &DecodedArtifact,
    limits: &ArtifactLimits,
) -> Result<Option<(usize, usize)>, DiagnosticSet> {
    let mut root = None;
    for (module_id, module) in artifact.modules.iter().enumerate() {
        for (type_id, nominal) in module.types.iter().enumerate() {
            let NominalType::Class {
                flags,
                generic_arity,
                super_type,
                interfaces,
                field_start,
                field_count,
                method_count,
                initializer,
                ..
            } = nominal
            else {
                continue;
            };
            if flags & 4 == 0 {
                continue;
            }
            if root.replace((module_id, type_id)).is_some() {
                return Err(failure(limits, module_id, 0, "multiple Throwable roots"));
            }
            if artifact.header.runtime_major < 1
                || (artifact.header.runtime_major == 1 && artifact.header.runtime_minor < 8)
            {
                return Err(failure(
                    limits,
                    module_id,
                    0,
                    "Throwable root requires Runtime ABI 1.8",
                ));
            }
            let fields = module
                .fields
                .get(*field_start as usize..(*field_start as usize).saturating_add(2));
            let valid_fields = fields.is_some_and(|fields| {
                fields.iter().all(|field| {
                    field.flags & 2 == 0
                        && modules::resolved_type(artifact, module_id, field.owner)
                            == Some((module_id, type_id))
                }) && fields[0].value_type.kind == 7
                    && fields[0].value_type.flags == 1
                    && modules::resolved_type(
                        artifact,
                        module_id,
                        fields[0].value_type.nominal_type,
                    )
                    .is_some_and(|identity| {
                        let NominalType::Class { name, .. } =
                            artifact.modules[identity.0].types[identity.1]
                        else {
                            return false;
                        };
                        artifact.modules[identity.0]
                            .strings
                            .get(name as usize)
                            .is_some_and(|name| name.slice(&artifact.bytes) == b"kotlin.String")
                    })
                    && fields[1].value_type.kind == 7
                    && fields[1].value_type.flags == 1
                    && modules::resolved_type(
                        artifact,
                        module_id,
                        fields[1].value_type.nominal_type,
                    ) == Some((module_id, type_id))
            });
            let valid_parent = super_type.0 == u32::MAX || modules::resolved_type(artifact, module_id, *super_type).is_some_and(|identity| {
                matches!(&artifact.modules[identity.0].types[identity.1], NominalType::Class {
                    flags: 0, generic_arity: 0, super_type, interfaces, field_count: 0, method_count: 0, initializer: None, ..
                } if super_type.0 == u32::MAX && interfaces.is_empty())
            });
            if *flags != 4
                || *generic_arity != 0
                || !interfaces.is_empty()
                || *field_count != 2
                || *method_count != 0
                || initializer.is_some()
                || !valid_fields
                || !valid_parent
            {
                return Err(failure(
                    limits,
                    module_id,
                    0,
                    "invalid Throwable root layout or superclass",
                ));
            }
        }
    }
    Ok(root)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Handler {
    pub protected_start: usize,
    pub protected_end: usize,
    pub handler_block: usize,
    pub exception_register: u16,
}

/// Roles are resolved from validated metadata, never exception class names.
pub(crate) fn verify_runtime_exception_types(
    artifact: &DecodedArtifact,
    limits: &ArtifactLimits,
    root: Option<(usize, usize)>,
) -> Result<[Option<(usize, usize)>; 8], DiagnosticSet> {
    let mut roles = [None; 8];
    for (module_id, module) in artifact.modules.iter().enumerate() {
        for (type_id, nominal) in module.types.iter().enumerate() {
            let NominalType::Class { flags, .. } = nominal else {
                continue;
            };
            let tag = flags >> 3;
            if tag == 0 {
                continue;
            }
            if tag > 8 || (artifact.header.runtime_major, artifact.header.runtime_minor) < (1, 9) {
                return Err(failure(
                    limits,
                    module_id,
                    0,
                    "runtime exception role requires Runtime ABI 1.9",
                ));
            }
            if roles[tag as usize - 1]
                .replace((module_id, type_id))
                .is_some()
            {
                return Err(failure(
                    limits,
                    module_id,
                    0,
                    "duplicate runtime exception role",
                ));
            }
            let mut current = Some((module_id, type_id));
            let mut remaining = artifact
                .modules
                .iter()
                .map(|module| module.types.len())
                .sum::<usize>();
            while current != root && remaining != 0 {
                let Some((owner, id)) = current else { break };
                let NominalType::Class {
                    flags,
                    generic_arity: 0,
                    super_type,
                    interfaces,
                    field_count: 0,
                    method_count: 0,
                    initializer: None,
                    ..
                } = &artifact.modules[owner].types[id]
                else {
                    break;
                };
                if flags & 5 != 0 || !interfaces.is_empty() {
                    break;
                }
                current = modules::resolved_type(artifact, owner, *super_type);
                remaining -= 1;
            }
            if root.is_none() || current != root || Some((module_id, type_id)) == root {
                return Err(failure(
                    limits,
                    module_id,
                    0,
                    "runtime exception role requires a zero-state Throwable subclass",
                ));
            }
        }
    }
    Ok(roles)
}

#[derive(Debug)]
pub(crate) struct ExceptionModel {
    pub functions: Vec<Vec<Vec<Handler>>>,
}

pub(crate) fn verify_exceptions(
    artifact: &DecodedArtifact,
    limits: &ArtifactLimits,
) -> Result<ExceptionModel, DiagnosticSet> {
    let root = verify_throwable_root(artifact, limits)?;
    let roles = verify_runtime_exception_types(artifact, limits, root)?;
    let uses_arithmetic_errors = artifact.modules.iter().any(|module| {
        module.code.iter().any(|code| {
            code.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    Instruction::Div { form: 1 | 2, .. } | Instruction::Rem { form: 1 | 2, .. }
                )
            })
        })
    });
    if uses_arithmetic_errors
        && ((artifact.header.runtime_major, artifact.header.runtime_minor) < (1, 9)
            || roles[0].is_none())
    {
        return Err(failure(
            limits,
            0,
            0,
            "arithmetic error artifact: rebuild with exception roles for Runtime ABI 1.9",
        ));
    }
    let uses_exceptions = artifact.modules.iter().any(|module| {
        !module.exceptions.is_empty()
            || module.code.iter().any(|code| {
                code.instructions
                    .iter()
                    .any(|instruction| matches!(instruction, Instruction::Throw { .. }))
            })
    });
    if uses_exceptions
        && (artifact.header.runtime_major < 1
            || artifact.header.runtime_major == 1 && artifact.header.runtime_minor < 8)
    {
        return Err(failure(
            limits,
            0,
            0,
            "legacy exception artifact: rebuild for Runtime ABI 1.8",
        ));
    }
    if uses_exceptions && root.is_none() {
        return Err(failure(
            limits,
            0,
            0,
            "exception artifact requires a verified Throwable root; rebuild libraries",
        ));
    }
    let mut modules_model = Vec::new();
    modules_model
        .try_reserve_exact(artifact.modules.len())
        .map_err(|_| failure(limits, 0, 0, "cannot reserve exception model"))?;
    for (module_id, module) in artifact.modules.iter().enumerate() {
        let mut functions_model = Vec::new();
        functions_model
            .try_reserve_exact(module.functions.len())
            .map_err(|_| failure(limits, module_id, 0, "cannot reserve function handlers"))?;
        for (function_id, function) in module.functions.iter().enumerate() {
            let start = function.first_exception as usize;
            let end = start
                .checked_add(function.exception_count as usize)
                .filter(|end| *end <= module.exceptions.len())
                .ok_or_else(|| {
                    failure(
                        limits,
                        module_id,
                        function_id,
                        "function exception range is out of bounds",
                    )
                })?;
            let block_start = function.first_block.0 as usize;
            let block_end = block_start
                .checked_add(function.block_count as usize)
                .filter(|end| *end <= module.blocks.len())
                .ok_or_else(|| {
                    failure(
                        limits,
                        module_id,
                        function_id,
                        "function block range is out of bounds",
                    )
                })?;
            let mut handlers = Vec::new();
            handlers
                .try_reserve_exact(end - start)
                .map_err(|_| failure(limits, module_id, function_id, "cannot reserve handlers"))?;
            for entry in &module.exceptions[start..end] {
                let protected_start = entry.first_protected_block.0 as usize;
                let protected_end = protected_start
                    .checked_add(entry.protected_block_count as usize)
                    .filter(|end| {
                        entry.protected_block_count != 0
                            && protected_start >= block_start
                            && *end <= block_end
                    })
                    .ok_or_else(|| {
                        failure(
                            limits,
                            module_id,
                            function_id,
                            "protected block range is empty or outside function",
                        )
                    })?;
                let handler_block = entry.handler_block.0 as usize;
                if entry.owner_function.0 as usize != function_id
                    || handler_block < block_start
                    || handler_block >= block_end
                {
                    return Err(failure(
                        limits,
                        module_id,
                        function_id,
                        "exception owner or handler block is outside function",
                    ));
                }
                let register = function
                    .values
                    .get(entry.exception_register as usize)
                    .map(|value| value.semantic_type)
                    .ok_or_else(|| {
                        failure(
                            limits,
                            module_id,
                            function_id,
                            "exception register is out of range",
                        )
                    })?;
                if register.kind != 7 || register.flags & 1 != 0 {
                    return Err(failure(
                        limits,
                        module_id,
                        function_id,
                        "exception register is not a non-null reference",
                    ));
                }
                let (root_module, root_type) = root.ok_or_else(|| {
                    failure(limits, module_id, function_id, "missing Throwable root")
                })?;
                let root_value = crate::artifact::ValueType {
                    kind: 7,
                    flags: 0,
                    nominal_type: crate::artifact::TypeId(root_type as u32),
                };
                if entry.catch_type.0 != u32::MAX {
                    let catch = modules::resolved_type(artifact, module_id, entry.catch_type)
                        .ok_or_else(|| {
                            failure(
                                limits,
                                module_id,
                                function_id,
                                "catch type does not resolve",
                            )
                        })?;
                    if !matches!(
                        artifact.modules[catch.0].types[catch.1],
                        NominalType::Class { .. } | NominalType::Interface { .. }
                    ) {
                        return Err(failure(
                            limits,
                            module_id,
                            function_id,
                            "catch type is not a reference nominal type",
                        ));
                    }
                    let catch_value = crate::artifact::ValueType {
                        kind: 7,
                        flags: 0,
                        nominal_type: entry.catch_type,
                    };
                    if !functions::value_assignable(
                        artifact,
                        module_id,
                        catch_value,
                        root_module,
                        root_value,
                    ) {
                        return Err(failure(
                            limits,
                            module_id,
                            function_id,
                            "catch type is not a Throwable subclass",
                        ));
                    }
                    if !functions::value_assignable(
                        artifact,
                        module_id,
                        catch_value,
                        module_id,
                        register,
                    ) {
                        return Err(failure(
                            limits,
                            module_id,
                            function_id,
                            "exception register and catch type are incompatible",
                        ));
                    }
                } else if modules::resolved_type(artifact, module_id, register.nominal_type)
                    != Some((root_module, root_type))
                {
                    return Err(failure(
                        limits,
                        module_id,
                        function_id,
                        "catch-all register must be the Throwable root",
                    ));
                }
                handlers.push(Handler {
                    protected_start,
                    protected_end,
                    handler_block,
                    exception_register: entry.exception_register,
                });
            }
            reject_crossing_ranges(&handlers, limits, module_id, function_id)?;
            for code in &module.code[block_start..block_end] {
                for instruction in &code.instructions {
                    if let Instruction::Throw { exception } = instruction {
                        let value = function
                            .values
                            .get(*exception as usize)
                            .map(|value| value.semantic_type)
                            .ok_or_else(|| {
                                failure(
                                    limits,
                                    module_id,
                                    function_id,
                                    "throw register is out of range",
                                )
                            })?;
                        let (root_module, root_type) = root.ok_or_else(|| {
                            failure(limits, module_id, function_id, "missing Throwable root")
                        })?;
                        let root_value = crate::artifact::ValueType {
                            kind: 7,
                            flags: 0,
                            nominal_type: crate::artifact::TypeId(root_type as u32),
                        };
                        if value.kind != 7
                            || value.flags != 0
                            || !functions::value_assignable(
                                artifact,
                                module_id,
                                value,
                                root_module,
                                root_value,
                            )
                        {
                            return Err(failure(
                                limits,
                                module_id,
                                function_id,
                                "throw operand is not a non-null Throwable",
                            ));
                        }
                    }
                }
            }
            functions_model.push(handlers);
        }
        modules_model.push(functions_model);
    }
    Ok(ExceptionModel {
        functions: modules_model,
    })
}

fn reject_crossing_ranges(
    handlers: &[Handler],
    limits: &ArtifactLimits,
    module_id: usize,
    function_id: usize,
) -> Result<(), DiagnosticSet> {
    let mut order: Vec<usize> = (0..handlers.len()).collect();
    order.sort_unstable_by_key(|id| {
        (
            handlers[*id].protected_start,
            std::cmp::Reverse(handlers[*id].protected_end),
        )
    });
    let mut stack: Vec<usize> = Vec::new();
    for id in order {
        let current = handlers[id];
        while stack
            .last()
            .is_some_and(|parent| handlers[*parent].protected_end <= current.protected_start)
        {
            stack.pop();
        }
        if stack
            .last()
            .is_some_and(|parent| current.protected_end > handlers[*parent].protected_end)
        {
            return Err(failure(
                limits,
                module_id,
                function_id,
                "exception protected ranges cross without nesting",
            ));
        }
        stack.push(id);
    }
    Ok(())
}

pub(crate) fn verify_semantic_features(
    artifact: &DecodedArtifact,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let mut expected = 0_u32;
    let mut uses_tasks = false;
    let mut uses_channels = false;
    let mut uses_i64_string_conversion = false;
    let mut uses_f32_string_conversion = false;
    let mut uses_array_copy = false;
    for module in &artifact.modules {
        if !module.exceptions.is_empty()
            || module
                .code
                .iter()
                .flat_map(|code| code.instructions.iter())
                .any(|instruction| matches!(instruction, Instruction::Throw { .. }))
        {
            expected |= 1 << 0;
        }
        if module
            .functions
            .iter()
            .any(|function| function.flags & 1 != 0)
            || module
                .code
                .iter()
                .flat_map(|code| code.instructions.iter())
                .any(|instruction| {
                    matches!(
                        instruction,
                        Instruction::CoroutineSpawn { .. }
                            | Instruction::CallSuspend { .. }
                            | Instruction::Yield { .. }
                            | Instruction::Sleep { .. }
                            | Instruction::CoroutineJoin { .. }
                            | Instruction::ChannelSend { .. }
                            | Instruction::ChannelReceive { .. }
                    )
                })
        {
            expected |= 1 << 1;
        }
        uses_tasks |= module
            .code
            .iter()
            .flat_map(|code| code.instructions.iter())
            .any(|instruction| {
                matches!(
                    instruction,
                    Instruction::CoroutineSpawn { .. } | Instruction::CoroutineJoin { .. }
                )
            });
        uses_channels |= module
            .code
            .iter()
            .flat_map(|code| code.instructions.iter())
            .any(|instruction| {
                matches!(
                    instruction,
                    Instruction::ChannelCreate { .. }
                        | Instruction::ChannelSend { .. }
                        | Instruction::ChannelReceive { .. }
                )
            });
        uses_array_copy |= module
            .code
            .iter()
            .flat_map(|code| code.instructions.iter())
            .any(|instruction| matches!(instruction, Instruction::ArrayCopy { .. }));
        if uses_array_copy {
            expected |= 1 << 5;
        }
        uses_i64_string_conversion |= module
            .code
            .iter()
            .flat_map(|code| code.instructions.iter())
            .any(|instruction| matches!(instruction, Instruction::StringValueOf { form: 2, .. }));
        uses_f32_string_conversion |= module
            .code
            .iter()
            .flat_map(|code| code.instructions.iter())
            .any(|instruction| matches!(instruction, Instruction::StringValueOf { form: 3, .. }));
        if !artifact.capabilities.is_empty()
            || module
                .code
                .iter()
                .flat_map(|code| code.instructions.iter())
                .any(|instruction| {
                    matches!(
                        instruction,
                        Instruction::CapabilityCallSync { .. }
                            | Instruction::CapabilityCallAsync { .. }
                    )
                })
        {
            expected |= 1 << 2;
        }
        if !module.imports.is_empty() {
            expected |= 1 << 3;
        }
        if uses_channels {
            expected |= 1 << 4;
        }
    }
    if uses_array_copy && (artifact.header.runtime_major, artifact.header.runtime_minor) < (1, 5) {
        let mut diagnostic = Diagnostic::at_offset(
            Family::Module,
            Code::BadModule,
            6,
            "array copying requires minimum runtime ABI 1.5",
        );
        diagnostic.location.section = None;
        let mut errors = DiagnosticSet::new(limits.diagnostics);
        errors.push(diagnostic);
        return Err(errors);
    }
    if uses_tasks && (artifact.header.runtime_major, artifact.header.runtime_minor) < (1, 1) {
        let mut diagnostic = Diagnostic::at_offset(
            Family::Module,
            Code::BadModule,
            6,
            "task instructions require minimum runtime ABI 1.1",
        );
        diagnostic.location.section = None;
        let mut errors = DiagnosticSet::new(limits.diagnostics);
        errors.push(diagnostic);
        return Err(errors);
    }
    if uses_channels && (artifact.header.runtime_major, artifact.header.runtime_minor) < (1, 2) {
        let mut diagnostic = Diagnostic::at_offset(
            Family::Module,
            Code::BadModule,
            6,
            "channel instructions require minimum runtime ABI 1.2",
        );
        diagnostic.location.section = None;
        let mut errors = DiagnosticSet::new(limits.diagnostics);
        errors.push(diagnostic);
        return Err(errors);
    }
    if uses_i64_string_conversion
        && (artifact.header.runtime_major, artifact.header.runtime_minor) < (1, 3)
    {
        let mut diagnostic = Diagnostic::at_offset(
            Family::Module,
            Code::BadModule,
            6,
            "I64 string conversion requires minimum runtime ABI 1.3",
        );
        diagnostic.location.section = None;
        let mut errors = DiagnosticSet::new(limits.diagnostics);
        errors.push(diagnostic);
        return Err(errors);
    }
    if uses_f32_string_conversion
        && (artifact.header.runtime_major, artifact.header.runtime_minor) < (1, 4)
    {
        let mut diagnostic = Diagnostic::at_offset(
            Family::Module,
            Code::BadModule,
            6,
            "F32 string conversion requires minimum runtime ABI 1.4",
        );
        diagnostic.location.section = None;
        let mut errors = DiagnosticSet::new(limits.diagnostics);
        errors.push(diagnostic);
        return Err(errors);
    }
    if uses_channels
        && (artifact.manifest.maximum_channels == 0
            || artifact.manifest.maximum_channel_values == 0)
    {
        return Err(module_failure(
            limits,
            "channel instructions require non-zero channel manifest limits",
        ));
    }
    if !uses_channels
        && (artifact.manifest.maximum_channels != 0
            || artifact.manifest.maximum_channel_values != 0)
    {
        return Err(module_failure(
            limits,
            "channel manifest limits require channel instructions",
        ));
    }
    if artifact.header.semantic_features != expected {
        let mut diagnostic = Diagnostic::at_offset(
            Family::Module,
            Code::BadModule,
            20,
            "semantic feature bits do not exactly match artifact use",
        );
        diagnostic.location.section = None;
        let mut errors = DiagnosticSet::new(limits.diagnostics);
        errors.push(diagnostic);
        Err(errors)
    } else {
        Ok(())
    }
}

fn module_failure(limits: &ArtifactLimits, detail: &'static str) -> DiagnosticSet {
    let mut diagnostic = Diagnostic::at_offset(Family::Module, Code::BadModule, 0, detail);
    diagnostic.location.section = None;
    let mut errors = DiagnosticSet::new(limits.diagnostics);
    errors.push(diagnostic);
    errors
}

fn failure(
    limits: &ArtifactLimits,
    module: usize,
    function: usize,
    detail: &'static str,
) -> DiagnosticSet {
    let mut diagnostic = Diagnostic::at_offset(Family::Exception, Code::BadException, 0, detail);
    diagnostic.location.module = u32::try_from(module).ok();
    diagnostic.location.function = u32::try_from(function).ok();
    let mut errors = DiagnosticSet::new(limits.diagnostics);
    errors.push(diagnostic);
    errors
}
