use std::collections::VecDeque;

use crate::{
    artifact::{DecodedModule, Instruction},
    diagnostic::{Code, Diagnostic, DiagnosticSet, Family},
    limits::ArtifactLimits,
};

pub(super) struct ControlFlow {
    pub successors: Vec<Vec<usize>>,
}

pub(super) fn verify_control_flow(
    module: &DecodedModule,
    module_id: usize,
    function_id: usize,
    limits: &ArtifactLimits,
) -> Result<ControlFlow, DiagnosticSet> {
    let function = &module.functions[function_id];
    let start = function.first_block.0 as usize;
    let end = start
        .checked_add(function.block_count as usize)
        .filter(|end| *end <= module.blocks.len() && *end <= module.code.len())
        .ok_or_else(|| {
            failure(
                limits,
                module_id,
                function_id,
                "function block range is invalid",
            )
        })?;
    if start == end {
        return Err(failure(
            limits,
            module_id,
            function_id,
            "non-abstract function has no blocks",
        ));
    }

    let mut successors = Vec::new();
    successors
        .try_reserve_exact(end - start)
        .map_err(|_| failure(limits, module_id, function_id, "cannot reserve CFG"))?;
    for block_id in start..end {
        let block = &module.blocks[block_id];
        if block.owner_function.0 as usize != function_id
            || block.code_record.0 as usize != block_id
        {
            return Err(failure(
                limits,
                module_id,
                function_id,
                "function blocks are not contiguous or have the wrong owner",
            ));
        }
        let instructions = &module.code[block_id].instructions;
        if instructions.is_empty()
            || !instructions.last().is_some_and(Instruction::is_terminator)
            || instructions[..instructions.len() - 1]
                .iter()
                .any(Instruction::is_terminator)
        {
            return Err(failure(
                limits,
                module_id,
                function_id,
                "block does not contain exactly one final terminator",
            ));
        }
        let targets = terminator_targets(instructions.last().expect("checked final instruction"));
        let mut local = Vec::new();
        local
            .try_reserve_exact(targets.len())
            .map_err(|_| failure(limits, module_id, function_id, "cannot reserve CFG edges"))?;
        for target in targets {
            let target = target as usize;
            if target < start || target >= end {
                return Err(failure(
                    limits,
                    module_id,
                    function_id,
                    "control-flow target is outside the function",
                ));
            }
            if target <= block_id && module.blocks[target].flags & 1 == 0 {
                return Err(failure(
                    limits,
                    module_id,
                    function_id,
                    "backedge target is not a loop-header safepoint",
                ));
            }
            local.push(target - start);
        }
        successors.push(local);
    }

    let mut reachable = vec![false; successors.len()];
    let mut queue = VecDeque::new();
    reachable[0] = true;
    queue.push_back(0);
    while let Some(block) = queue.pop_front() {
        for target in &successors[block] {
            if !reachable[*target] {
                reachable[*target] = true;
                queue.push_back(*target);
            }
        }
    }
    if reachable.iter().any(|value| !value) {
        return Err(failure(
            limits,
            module_id,
            function_id,
            "function contains an unreachable block",
        ));
    }
    Ok(ControlFlow { successors })
}

fn terminator_targets(instruction: &Instruction) -> Vec<u32> {
    match instruction {
        Instruction::Jump { target } => vec![*target],
        Instruction::Branch {
            true_block,
            false_block,
            ..
        } => vec![*true_block, *false_block],
        Instruction::SwitchI32 {
            default_block,
            cases,
            ..
        } => {
            let mut targets = Vec::with_capacity(cases.len() + 1);
            targets.push(*default_block);
            targets.extend(cases.iter().map(|case| case.target));
            targets
        }
        Instruction::CallSuspend { resume_block, .. }
        | Instruction::Yield { resume_block }
        | Instruction::Sleep { resume_block, .. }
        | Instruction::CoroutineJoin { resume_block, .. }
        | Instruction::CapabilityCallAsync { resume_block, .. } => vec![*resume_block],
        Instruction::Return { .. } | Instruction::Throw { .. } | Instruction::Unreachable => {
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn failure(
    limits: &ArtifactLimits,
    module: usize,
    function: usize,
    detail: &'static str,
) -> DiagnosticSet {
    let mut diagnostic = Diagnostic::at_offset(Family::Cfg, Code::BadControlFlow, 0, detail);
    diagnostic.location.module = u32::try_from(module).ok();
    diagnostic.location.function = u32::try_from(function).ok();
    let mut errors = DiagnosticSet::new(limits.diagnostics);
    errors.push(diagnostic);
    errors
}
