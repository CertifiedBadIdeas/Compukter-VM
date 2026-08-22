use sha2::{Digest, Sha256};

use crate::{
    artifact::{format, DecodedArtifact, ModuleId, NominalType, TypeId},
    decode::container::decode_container,
    diagnostic::{Code, Diagnostic, DiagnosticSet, Family},
    limits::ArtifactLimits,
};

const MODULE_DOMAIN: &[u8] = b"Compukter module v1\0";
const SEMANTIC_MODULE_SECTIONS: [u16; 11] = [
    format::STRINGS,
    format::TYPES,
    format::CONSTANTS,
    format::IMPORTS,
    format::EXPORTS,
    format::FIELDS,
    format::FUNCTIONS,
    format::BLOCKS,
    format::CODE,
    format::EXCEPTIONS,
    format::UTF16_LITERALS,
];

pub(crate) fn verify_modules(
    artifact: &DecodedArtifact,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let container = decode_container(&artifact.bytes, limits)?;
    for (module_id, module) in artifact.modules.iter().enumerate() {
        let scope = u32::try_from(module_id)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| failure(limits, module_id, "module scope overflows u32"))?;
        let mut hasher = Sha256::new();
        hasher.update(MODULE_DOMAIN);
        for kind in SEMANTIC_MODULE_SECTIONS {
            let entry = container
                .directory
                .iter()
                .find(|entry| entry.scope == scope && entry.kind == kind)
                .ok_or_else(|| failure(limits, module_id, "semantic module section is missing"))?;
            let start = usize::try_from(entry.offset)
                .map_err(|_| failure(limits, module_id, "section offset does not fit usize"))?;
            let length = usize::try_from(entry.length)
                .map_err(|_| failure(limits, module_id, "section length does not fit usize"))?;
            let end = start
                .checked_add(length)
                .ok_or_else(|| failure(limits, module_id, "section range overflows usize"))?;
            let payload = artifact
                .bytes
                .get(start..end)
                .ok_or_else(|| failure(limits, module_id, "section range is outside artifact"))?;
            hasher.update(kind.to_le_bytes());
            hasher.update(entry.length.to_le_bytes());
            hasher.update(payload);
        }
        let actual: [u8; 32] = hasher.finalize().into();
        if actual != module.semantic_hash {
            return Err(failure(
                limits,
                module_id,
                "module semantic SHA-256 does not match",
            ));
        }
    }
    let order = topological_module_order(artifact, limits)?;
    resolve_imports_and_exports(artifact, &order, limits)?;
    verify_nominal_types(artifact, limits)?;
    Ok(())
}

fn verify_nominal_types(
    artifact: &DecodedArtifact,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let mut prefixes = Vec::new();
    prefixes
        .try_reserve_exact(artifact.modules.len() + 1)
        .map_err(|_| type_failure(limits, 0, "cannot reserve type prefixes"))?;
    prefixes.push(0_usize);
    for module in &artifact.modules {
        let next = prefixes
            .last()
            .and_then(|value| value.checked_add(module.types.len()))
            .ok_or_else(|| type_failure(limits, 0, "total type count overflows usize"))?;
        prefixes.push(next);
    }
    let total = *prefixes.last().expect("type prefix zero");
    let mut edges = Vec::new();
    edges
        .try_reserve_exact(total)
        .map_err(|_| type_failure(limits, 0, "cannot reserve type graph"))?;

    for (module_id, module) in artifact.modules.iter().enumerate() {
        for (type_id, nominal) in module.types.iter().enumerate() {
            let mut neighbors = Vec::new();
            match nominal {
                NominalType::Class {
                    flags,
                    super_type,
                    interfaces,
                    field_start,
                    field_count,
                    method_start,
                    method_count,
                    ..
                } => {
                    if flags & 0b11 == 0b11 {
                        return Err(type_failure(
                            limits,
                            module_id,
                            "class cannot be both abstract and final",
                        ));
                    }
                    if super_type.0 != u32::MAX {
                        let target =
                            resolved_type(artifact, module_id, *super_type).ok_or_else(|| {
                                type_failure(
                                    limits,
                                    module_id,
                                    "superclass reference does not resolve",
                                )
                            })?;
                        if !matches!(
                            artifact.modules[target.0].types[target.1],
                            NominalType::Class { .. }
                        ) {
                            return Err(type_failure(
                                limits,
                                module_id,
                                "superclass is not a class",
                            ));
                        }
                        neighbors.push(global_type(&prefixes, target));
                    }
                    add_interfaces(
                        artifact,
                        &prefixes,
                        module_id,
                        interfaces,
                        &mut neighbors,
                        limits,
                    )?;
                    verify_owned_range(
                        artifact,
                        module_id,
                        type_id,
                        *field_start,
                        *field_count,
                        true,
                        limits,
                    )?;
                    verify_owned_range(
                        artifact,
                        module_id,
                        type_id,
                        *method_start,
                        *method_count,
                        false,
                        limits,
                    )?;
                }
                NominalType::Interface {
                    super_type,
                    interfaces,
                    method_start,
                    method_count,
                    ..
                } => {
                    if super_type.0 != u32::MAX {
                        let target =
                            resolved_type(artifact, module_id, *super_type).ok_or_else(|| {
                                type_failure(limits, module_id, "interface parent does not resolve")
                            })?;
                        if !matches!(
                            artifact.modules[target.0].types[target.1],
                            NominalType::Interface { .. }
                        ) {
                            return Err(type_failure(
                                limits,
                                module_id,
                                "interface parent is not an interface",
                            ));
                        }
                        neighbors.push(global_type(&prefixes, target));
                    }
                    add_interfaces(
                        artifact,
                        &prefixes,
                        module_id,
                        interfaces,
                        &mut neighbors,
                        limits,
                    )?;
                    verify_owned_range(
                        artifact,
                        module_id,
                        type_id,
                        *method_start,
                        *method_count,
                        false,
                        limits,
                    )?;
                }
                NominalType::Array { .. } | NominalType::Function { .. } => {}
            }
            edges.push(neighbors);
        }
        for field in &module.fields {
            let owner = resolved_type(artifact, module_id, field.owner)
                .ok_or_else(|| type_failure(limits, module_id, "field owner does not resolve"))?;
            if !matches!(
                artifact.modules[owner.0].types[owner.1],
                NominalType::Class { .. }
            ) {
                return Err(type_failure(
                    limits,
                    module_id,
                    "field owner is not a class",
                ));
            }
        }
    }
    reject_type_cycles(&edges, artifact, &prefixes, limits)
}

fn add_interfaces(
    artifact: &DecodedArtifact,
    prefixes: &[usize],
    module_id: usize,
    interfaces: &[TypeId],
    neighbors: &mut Vec<usize>,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    neighbors
        .try_reserve_exact(interfaces.len())
        .map_err(|_| type_failure(limits, module_id, "cannot reserve inheritance edges"))?;
    for interface in interfaces {
        let target = resolved_type(artifact, module_id, *interface).ok_or_else(|| {
            type_failure(limits, module_id, "interface reference does not resolve")
        })?;
        if !matches!(
            artifact.modules[target.0].types[target.1],
            NominalType::Interface { .. }
        ) {
            return Err(type_failure(
                limits,
                module_id,
                "implemented type is not an interface",
            ));
        }
        neighbors.push(global_type(prefixes, target));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_owned_range(
    artifact: &DecodedArtifact,
    module_id: usize,
    type_id: usize,
    start: u32,
    count: u32,
    fields: bool,
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let module = &artifact.modules[module_id];
    let bound = if fields {
        module.fields.len()
    } else {
        module.functions.len()
    };
    let start = start as usize;
    let end = start
        .checked_add(count as usize)
        .filter(|end| *end <= bound)
        .ok_or_else(|| type_failure(limits, module_id, "owned member range is out of bounds"))?;
    for id in start..end {
        let owner = if fields {
            module.fields[id].owner
        } else {
            module.functions[id].owner
        };
        if owner != TypeId(type_id as u32) {
            return Err(type_failure(
                limits,
                module_id,
                "owned member range contains a different owner",
            ));
        }
    }
    Ok(())
}

pub(super) fn resolved_type(
    artifact: &DecodedArtifact,
    module_id: usize,
    reference: TypeId,
) -> Option<(usize, usize)> {
    if reference.0 == u32::MAX {
        return None;
    }
    if reference.0 & 0x8000_0000 == 0 {
        let type_id = reference.0 as usize;
        return (type_id < artifact.modules[module_id].types.len()).then_some((module_id, type_id));
    }
    let import = artifact.modules[module_id]
        .imports
        .get((reference.0 & 0x7fff_ffff) as usize)?;
    if import.kind != 0 {
        return None;
    }
    let target_module = import.target_module.0 as usize;
    let target = artifact.modules.get(target_module)?;
    target
        .exports
        .iter()
        .find(|export| export.kind == 0 && export.name == import.target_name)
        .and_then(|export| {
            let type_id = export.local_symbol as usize;
            (type_id < target.types.len()).then_some((target_module, type_id))
        })
}

fn global_type(prefixes: &[usize], value: (usize, usize)) -> usize {
    prefixes[value.0] + value.1
}

fn reject_type_cycles(
    edges: &[Vec<usize>],
    artifact: &DecodedArtifact,
    prefixes: &[usize],
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    let mut states = vec![0_u8; edges.len()];
    for root in 0..edges.len() {
        if states[root] != 0 {
            continue;
        }
        states[root] = 1;
        let mut stack = vec![(root, 0_usize)];
        while let Some((node, edge)) = stack.last_mut() {
            if *edge == edges[*node].len() {
                states[*node] = 2;
                stack.pop();
                continue;
            }
            let target = edges[*node][*edge];
            *edge += 1;
            match states[target] {
                0 => {
                    states[target] = 1;
                    stack.push((target, 0));
                }
                1 => {
                    let module_id = prefixes
                        .windows(2)
                        .position(|range| *node >= range[0] && *node < range[1])
                        .unwrap_or(0);
                    return Err(type_failure(
                        limits,
                        module_id.min(artifact.modules.len().saturating_sub(1)),
                        "nominal type graph contains a cycle",
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn topological_module_order(
    artifact: &DecodedArtifact,
    limits: &ArtifactLimits,
) -> Result<Vec<ModuleId>, DiagnosticSet> {
    let mut states = vec![0_u8; artifact.modules.len()];
    let mut order = Vec::new();
    order
        .try_reserve_exact(artifact.modules.len())
        .map_err(|_| failure(limits, 0, "cannot reserve module order"))?;
    for root in 0..artifact.modules.len() {
        if states[root] != 0 {
            continue;
        }
        states[root] = 1;
        let mut stack = vec![(root, 0_usize)];
        while let Some((module_id, edge)) = stack.last_mut() {
            if *edge == artifact.modules[*module_id].imports.len() {
                states[*module_id] = 2;
                order.push(ModuleId(*module_id as u32));
                stack.pop();
                continue;
            }
            let import = &artifact.modules[*module_id].imports[*edge];
            *edge += 1;
            let target = import.target_module.0 as usize;
            if target >= artifact.modules.len() {
                return Err(symbol_failure(
                    limits,
                    *module_id,
                    "import target module is out of range",
                ));
            }
            match states[target] {
                0 => {
                    states[target] = 1;
                    stack.push((target, 0));
                }
                1 => {
                    return Err(failure(
                        limits,
                        *module_id,
                        "module import graph contains a cycle",
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(order)
}

fn resolve_imports_and_exports(
    artifact: &DecodedArtifact,
    order: &[ModuleId],
    limits: &ArtifactLimits,
) -> Result<(), DiagnosticSet> {
    for module_id in order {
        let source = &artifact.modules[module_id.0 as usize];
        for import in &source.imports {
            let target_id = import.target_module.0 as usize;
            let target = &artifact.modules[target_id];
            if import.target_hash != target.semantic_hash {
                return Err(symbol_failure(
                    limits,
                    module_id.0 as usize,
                    "import target semantic hash does not match",
                ));
            }
            let target_name = target.strings[import.target_name as usize].slice(&artifact.bytes);
            let mut matches = target.exports.iter().filter(|export| {
                export.kind == import.kind
                    && target.strings[export.name as usize].slice(&artifact.bytes) == target_name
                    && signatures_match(
                        artifact,
                        module_id.0 as usize,
                        import.expected_signature.0,
                        target_id,
                        export.signature.0,
                    )
            });
            if matches.next().is_none() || matches.next().is_some() {
                return Err(symbol_failure(
                    limits,
                    module_id.0 as usize,
                    "import does not resolve to exactly one export",
                ));
            }
        }
    }
    Ok(())
}

fn signatures_match(
    artifact: &DecodedArtifact,
    left_module: usize,
    left: u32,
    right_module: usize,
    right: u32,
) -> bool {
    let Some(left_identity) = resolved_type(artifact, left_module, TypeId(left)) else {
        return false;
    };
    let Some(right_identity) = resolved_type(artifact, right_module, TypeId(right)) else {
        return false;
    };
    let left = &artifact.modules[left_identity.0].types[left_identity.1];
    let right = &artifact.modules[right_identity.0].types[right_identity.1];
    match (left, right) {
        (
            crate::artifact::NominalType::Function {
                flags: left_flags,
                result: left_result,
                parameters: left_parameters,
                ..
            },
            crate::artifact::NominalType::Function {
                flags: right_flags,
                result: right_result,
                parameters: right_parameters,
                ..
            },
        ) => {
            left_flags == right_flags
                && value_types_match(
                    artifact,
                    left_identity.0,
                    *left_result,
                    right_identity.0,
                    *right_result,
                )
                && left_parameters.len() == right_parameters.len()
                && left_parameters
                    .iter()
                    .zip(right_parameters)
                    .all(|(left, right)| {
                        value_types_match(
                            artifact,
                            left_identity.0,
                            *left,
                            right_identity.0,
                            *right,
                        )
                    })
        }
        _ => false,
    }
}

pub(super) fn value_types_match(
    artifact: &DecodedArtifact,
    left_module: usize,
    left: crate::artifact::ValueType,
    right_module: usize,
    right: crate::artifact::ValueType,
) -> bool {
    left.kind == right.kind
        && left.flags == right.flags
        && if left.kind == 7 {
            resolved_type(artifact, left_module, left.nominal_type)
                == resolved_type(artifact, right_module, right.nominal_type)
        } else {
            true
        }
}

fn failure(limits: &ArtifactLimits, module: usize, detail: &'static str) -> DiagnosticSet {
    let mut diagnostic = Diagnostic::at_offset(Family::Module, Code::BadModule, 0, detail);
    diagnostic.location.module = u32::try_from(module).ok();
    let mut errors = DiagnosticSet::new(limits.diagnostics);
    errors.push(diagnostic);
    errors
}

fn symbol_failure(limits: &ArtifactLimits, module: usize, detail: &'static str) -> DiagnosticSet {
    let mut diagnostic = Diagnostic::at_offset(Family::Symbol, Code::BadSymbol, 0, detail);
    diagnostic.location.module = u32::try_from(module).ok();
    let mut errors = DiagnosticSet::new(limits.diagnostics);
    errors.push(diagnostic);
    errors
}

fn type_failure(limits: &ArtifactLimits, module: usize, detail: &'static str) -> DiagnosticSet {
    let mut diagnostic = Diagnostic::at_offset(Family::Type, Code::BadType, 0, detail);
    diagnostic.location.module = u32::try_from(module).ok();
    let mut errors = DiagnosticSet::new(limits.diagnostics);
    errors.push(diagnostic);
    errors
}
