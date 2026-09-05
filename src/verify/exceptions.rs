use crate::{
    artifact::{DecodedArtifact, Instruction, NominalType},
    diagnostic::{Code, Diagnostic, DiagnosticSet, Family},
    limits::ArtifactLimits,
};

use super::{functions, modules};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Handler {
    pub protected_start: usize,
    pub protected_end: usize,
    pub handler_block: usize,
    pub exception_register: u16,
}

#[derive(Debug)]
pub(crate) struct ExceptionModel {
    pub functions: Vec<Vec<Vec<Handler>>>,
}

pub(crate) fn verify_exceptions(
    artifact: &DecodedArtifact,
    limits: &ArtifactLimits,
) -> Result<ExceptionModel, DiagnosticSet> {
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
                        register,
                        module_id,
                        catch_value,
                    ) && !functions::value_assignable(
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
                }
                handlers.push(Handler {
                    protected_start,
                    protected_end,
                    handler_block,
                    exception_register: entry.exception_register,
                });
            }
            reject_crossing_ranges(&handlers, limits, module_id, function_id)?;
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
                    )
                })
        {
            expected |= 1 << 1;
        }
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
