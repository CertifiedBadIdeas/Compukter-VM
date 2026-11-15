/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

//! Private experimental transport. No worker calls into the JVM.
use crate::{bytes, chars, ints, status, write_handle, write_long};
use compukter_ffi::{experimental_fleet, FfiStatus};
use compukter_vm::experimental_fleet_scheduler::{
    FleetConfig, ReplyValue, RequestInspection, SubmitError,
};
use jni::{
    objects::{JByteArray, JCharArray, JClass, JIntArray, JLongArray},
    sys::{jint, jlong},
    EnvUnowned,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

type Fleet = experimental_fleet::FleetTransport;
static FLEETS: OnceLock<Mutex<HashMap<u64, Fleet>>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);
fn fleets() -> &'static Mutex<HashMap<u64, Fleet>> {
    FLEETS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn code(status: FfiStatus) -> jint {
    status as jint
}
fn submit(result: Result<(), SubmitError>) -> jint {
    code(match result {
        Ok(()) => FfiStatus::Ok,
        Err(SubmitError::Full) => FfiStatus::BusyHandle,
        Err(SubmitError::Closed) => FfiStatus::StaleHandle,
        Err(_) => FfiStatus::InvalidArgument,
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_requestLifecycle<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    kind: jint,
    tick: jlong,
) -> jint {
    status(&mut env, |_| {
        if tick < 0 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get(&(handle as u64)) else {
            return Ok(code(FfiStatus::StaleHandle));
        };
        Ok(submit(match kind {
            0 => fleet.scheduler.stop_with_lifecycle_poll(machine as u64),
            1 if fleet.owns_session(machine as u64) => fleet
                .scheduler
                .detach_with_lifecycle_poll(machine as u64, None),
            2 if fleet.owns_session(machine as u64) => fleet
                .scheduler
                .detach_with_lifecycle_poll(machine as u64, Some(tick as u64)),
            3 if fleet.owns_session(machine as u64) => fleet
                .scheduler
                .detach_for_checkpoint(machine as u64, tick as u64),
            _ => return Ok(code(FfiStatus::InvalidArgument)),
        }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_deployment<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    request: jlong,
    kind: jint,
    payload: JByteArray<'a>,
    candidate: jlong,
    revision: jlong,
) -> jint {
    status(&mut env, |env| {
        if machine < 0 || request < 0 || candidate < 0 || revision < 0 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let maximum = if kind == 0 {
            compukter_vm::ArtifactLimits::default().artifact_bytes
        } else {
            4096
        };
        if payload.len(env)? > maximum {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let payload = bytes(env, &payload)?;
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = map.get(&(handle as u64)) else {
            return Ok(code(FfiStatus::StaleHandle));
        };
        let result = match kind {
            0 if candidate == 0 && revision == 0 => {
                fleet
                    .scheduler
                    .prepare_deployment(machine as u64, request as u64, payload.into())
            }
            1 if candidate == 0 && revision == 0 => match String::from_utf8(payload) {
                Ok(path) => fleet.scheduler.deployment_revision(
                    machine as u64,
                    request as u64,
                    path.into_boxed_str(),
                ),
                Err(_) => return Ok(code(FfiStatus::InvalidArgument)),
            },
            2 if candidate > 0 => match String::from_utf8(payload) {
                Ok(path) => fleet.scheduler.install_deployment(
                    machine as u64,
                    request as u64,
                    path.into_boxed_str(),
                    if revision == 0 {
                        compukter_vm::ExecutableRevision::Absent
                    } else {
                        compukter_vm::ExecutableRevision::Present(revision as u64)
                    },
                    candidate as u64,
                ),
                Err(_) => return Ok(code(FfiStatus::InvalidArgument)),
            },
            3 if candidate > 0 && revision == 0 && payload.is_empty() => fleet
                .scheduler
                .discard_deployment(machine as u64, request as u64, candidate as u64),
            _ => return Ok(code(FfiStatus::InvalidArgument)),
        };
        Ok(submit(result))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_filesystemQuery<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    request: jlong,
    kind: jint,
    path: JByteArray<'a>,
    cursor: JByteArray<'a>,
    offset: jlong,
    maximum: jint,
    generation: jlong,
) -> jint {
    status(&mut env, |env| {
        if machine < 0
            || request < 0
            || offset < 0
            || maximum < 0
            || generation < 0
            || path.len(env)? > 4096
            || cursor.len(env)? > 4096
        {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let (Ok(path), Ok(cursor)) = (
            String::from_utf8(bytes(env, &path)?),
            String::from_utf8(bytes(env, &cursor)?),
        ) else {
            return Ok(code(FfiStatus::InvalidArgument));
        };
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(match kind {
                    0 if cursor.is_empty() && offset == 0 && maximum == 0 && generation == 0 => f
                        .scheduler
                        .file_stat(machine as u64, request as u64, path.into_boxed_str()),
                    1 if offset == 0 && generation == 0 => f.scheduler.file_list(
                        machine as u64,
                        request as u64,
                        path.into_boxed_str(),
                        (!cursor.is_empty()).then(|| cursor.into_boxed_str()),
                        maximum as u32,
                    ),
                    2 if cursor.is_empty() => f.scheduler.file_read(
                        machine as u64,
                        request as u64,
                        path.into_boxed_str(),
                        offset as u64,
                        maximum as u32,
                        generation as u64,
                    ),
                    _ => return code(FfiStatus::InvalidArgument),
                })
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_resourceSnapshot<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    request: jlong,
) -> jint {
    status(&mut env, |_| {
        if machine < 0 || request < 0 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(
                    f.scheduler
                        .resource_snapshot(machine as u64, request as u64),
                )
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_filesystemGeneration<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    request: jlong,
) -> jint {
    status(&mut env, |_| {
        if machine < 0 || request < 0 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(
                    f.scheduler
                        .filesystem_generation(machine as u64, request as u64),
                )
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_redstoneInput<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    packet: jint,
) -> jint {
    status(&mut env, |_| {
        if machine < 0 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(f.scheduler.redstone_input(machine as u64, packet as u32))
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_confirmRedstoneOutput<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    packed: jint,
) -> jint {
    status(&mut env, |_| {
        if machine < 0 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(
                    f.scheduler
                        .confirm_redstone_output(machine as u64, packed as u32),
                )
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_terminalUpdate<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    request: jlong,
    revision: jlong,
) -> jint {
    status(&mut env, |_| {
        if machine < 0 || request < 0 || revision < 0 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(
                    f.scheduler
                        .terminal_update(machine as u64, request as u64, revision as u64),
                )
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_create<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    artifact: JByteArray<'a>,
    schemas: JByteArray<'a>,
    count: jint,
    config: JIntArray<'a>,
    namespace: JByteArray<'a>,
    name: JByteArray<'a>,
    detail: JByteArray<'a>,
    major: jint,
    minor: jint,
    operation: jint,
    output: JLongArray<'a>,
) -> jint {
    status(&mut env, |env| {
        if output.len(env)? != 1
            || !(7..=8).contains(&config.len(env)?)
            || !(0..=4096).contains(&count)
            || artifact.len(env)? > compukter_vm::ArtifactLimits::default().artifact_bytes
            || schemas.len(env)? > 65536
            || namespace.len(env)? > 256
            || name.len(env)? > 256
            || detail.len(env)? > compukter_vm::MAXIMUM_HOST_FAILURE_DETAIL_BYTES
        {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let mut c = [0; 7];
        config.get_region(env, 0, &mut c)?;
        let mut coordinated_turns = [0];
        if config.len(env)? == 8 {
            config.get_region(env, 7, &mut coordinated_turns)?;
        }
        if c[0] <= 0
            || c[0] > 64
            || c[1] <= 0
            || c[1] > 65536
            || c[2] <= 0
            || c[2] > 65536
            || c[3] <= 0
            || c[4] < 0
            || c[5] <= 0
            || c[6] <= 0
            || !(0..=1).contains(&coordinated_turns[0])
        {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let cfg = FleetConfig {
            workers: c[0] as usize,
            mailbox_capacity: c[1] as usize,
            event_capacity: c[2] as usize,
            guest_budget: c[3] as u32,
            maintenance_budget: c[4] as u32,
            host_budget: c[5] as u32,
            maximum_advances_per_tick: c[6] as u32,
        };
        let ns = bytes(env, &namespace)?;
        let nm = bytes(env, &name)?;
        let dt = bytes(env, &detail)?;
        let inspections = if ns.is_empty()
            && nm.is_empty()
            && dt.is_empty()
            && major == 0
            && minor == 0
            && operation == 0
        {
            vec![]
        } else {
            if ns.is_empty()
                || nm.is_empty()
                || dt.is_empty()
                || !(0..=65535).contains(&major)
                || !(0..=65535).contains(&minor)
                || operation < 0
            {
                return Ok(code(FfiStatus::InvalidArgument));
            }
            let (Ok(ns), Ok(nm), Ok(dt)) = (
                String::from_utf8(ns),
                String::from_utf8(nm),
                String::from_utf8(dt),
            ) else {
                return Ok(code(FfiStatus::InvalidArgument));
            };
            vec![RequestInspection {
                namespace: ns.into_boxed_str(),
                name: nm.into_boxed_str(),
                abi_major: major as u16,
                abi_minor: minor as u16,
                operation: operation as u32,
                over_budget_detail: dt.into_boxed_str(),
            }]
        };
        let scheduler = match experimental_fleet::create_with_coordinated_turns(
            &bytes(env, &artifact)?,
            &bytes(env, &schemas)?,
            count as usize,
            cfg,
            inspections,
            coordinated_turns[0] == 1,
        ) {
            Ok(f) => f,
            Err(_) => return Ok(code(FfiStatus::InvalidArgument)),
        };
        let id =
            match NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1)) {
                Ok(id) => id,
                Err(_) => return Ok(code(FfiStatus::BusyHandle)),
            };
        let mut map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= 64 {
            drop(map);
            drop(scheduler);
            return Ok(code(FfiStatus::BusyHandle));
        }
        write_handle(env, &output, id)?;
        map.insert(id, Fleet::new(scheduler));
        Ok(code(FfiStatus::Ok))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_grant<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    tick: jlong,
    retirements: jint,
    nanos: jlong,
) -> jint {
    status(&mut env, |_| {
        if machine < 0 || tick < 0 || retirements <= 0 || nanos < -1 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let deadline = if nanos == -1 {
            None
        } else {
            let Some(deadline) = Instant::now().checked_add(Duration::from_nanos(nanos as u64))
            else {
                return Ok(code(FfiStatus::InvalidArgument));
            };
            Some(deadline)
        };
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(f.scheduler.grant_until(
                    machine as u64,
                    tick as u64,
                    retirements as u32,
                    deadline,
                ))
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_stop<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
) -> jint {
    status(&mut env, |_| {
        if machine < 0 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(f.scheduler.stop(machine as u64))
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_completeCompilation<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    token: jlong,
    kind: jint,
    payload: JByteArray<'a>,
) -> jint {
    status(&mut env, |env| {
        if machine < 0
            || token <= 0
            || !(0..=1).contains(&kind)
            || payload.len(env)? > compukter_vm::ArtifactLimits::default().artifact_bytes
        {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let payload = bytes(env, &payload)?;
        let result = if kind == 0 {
            compukter_vm::experimental_fleet_scheduler::CompilationResult::Artifact(
                payload.into_boxed_slice(),
            )
        } else {
            let Ok(detail) = String::from_utf8(payload) else {
                return Ok(code(FfiStatus::InvalidArgument));
            };
            compukter_vm::experimental_fleet_scheduler::CompilationResult::Failure(
                detail.into_boxed_str(),
            )
        };
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |fleet| {
                submit(
                    fleet
                        .scheduler
                        .complete_compilation(machine as u64, token as u64, result),
                )
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_resolveCost<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    task: jint,
    request: jlong,
    units: jint,
) -> jint {
    status(&mut env, |_| {
        if machine < 0 || task < 0 || request < 0 || units <= 0 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(f.scheduler.resolve_cost(
                    machine as u64,
                    task as u32,
                    request as u64,
                    units as u32,
                ))
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_reply<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    task: jint,
    request: jlong,
    tag: jint,
    bits: jlong,
    payload: JByteArray<'a>,
) -> jint {
    status(&mut env, |env| {
        if machine < 0 || task < 0 || request < 0 || payload.len(env)? > 65536 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let value = match tag {
            0 => ReplyValue::Unit,
            1 => ReplyValue::I32(bits as i32),
            2 => ReplyValue::I64(bits),
            3 => ReplyValue::F32(bits as u32),
            4 => ReplyValue::F64(bits as u64),
            5 if bits == 0 || bits == 1 => ReplyValue::Bool(bits != 0),
            6 if (0..=65535).contains(&bits) => ReplyValue::Char(bits as u16),
            7 => {
                let b = bytes(env, &payload)?;
                if b.len() % 2 != 0 {
                    return Ok(code(FfiStatus::InvalidArgument));
                }
                ReplyValue::String(
                    b.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|v| u16::from_le_bytes([v[0], v[1]]))
                        .collect(),
                )
            }
            8 => ReplyValue::Bytes(bytes(env, &payload)?.into_boxed_slice()),
            _ => return Ok(code(FfiStatus::InvalidArgument)),
        };
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(
                    f.scheduler
                        .reply(machine as u64, task as u32, request as u64, value),
                )
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_eventGeneration<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    output: JLongArray<'a>,
) -> jint {
    status(&mut env, |env| {
        if output.len(env)? != 1 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let signal = {
            let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
            let Some(fleet) = map.get(&(handle as u64)) else {
                return Ok(code(FfiStatus::StaleHandle));
            };
            fleet.scheduler.event_signal()
        };
        output.set_region(env, 0, &[signal.generation() as jlong])?;
        Ok(code(FfiStatus::Ok))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_waitEvents<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    generation: jlong,
    timeout_ms: jint,
    output: JLongArray<'a>,
) -> jint {
    status(&mut env, |env| {
        if output.len(env)? != 1 || !(0..=60_000).contains(&timeout_ms) {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let signal = {
            let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
            let Some(fleet) = map.get(&(handle as u64)) else {
                return Ok(code(FfiStatus::StaleHandle));
            };
            fleet.scheduler.event_signal()
        };
        let changed = signal.wait(generation as u64, Duration::from_millis(timeout_ms as u64));
        output.set_region(env, 0, &[i64::from(changed)])?;
        Ok(code(FfiStatus::Ok))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_poll<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    output: JByteArray<'a>,
    written: JLongArray<'a>,
) -> jint {
    status(&mut env, |env| {
        copy_poll(env, handle, None, output, written)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_pollLifecycle<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    output: JByteArray<'a>,
    written: JLongArray<'a>,
) -> jint {
    status(&mut env, |env| {
        copy_poll(env, handle, Some(machine as u64), output, written)
    })
}

fn copy_poll(
    env: &jni::Env<'_>,
    handle: jlong,
    machine: Option<u64>,
    output: JByteArray<'_>,
    written: JLongArray<'_>,
) -> jni::errors::Result<jint> {
    if written.len(env)? != 1 {
        return Ok(code(FfiStatus::InvalidArgument));
    }
    let capacity = output.len(env)?;
    let mut map = fleets().lock().unwrap_or_else(|e| e.into_inner());
    let Some(fleet) = map.get_mut(&(handle as u64)) else {
        return Ok(code(FfiStatus::StaleHandle));
    };
    let event = match machine {
        Some(machine) => match fleet.poll_lifecycle(machine) {
            Ok(event) => event,
            Err(status) => return Ok(code(status)),
        },
        None => fleet.poll(),
    };
    let Some(bytes) = event else {
        write_long(env, &written, 0)?;
        return Ok(code(FfiStatus::Ok));
    };
    write_long(env, &written, bytes.len())?;
    if capacity < bytes.len() {
        return Ok(code(FfiStatus::BufferTooSmall));
    }
    let signed = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<i8>(), bytes.len()) };
    output.set_region(env, 0, signed)?;
    match machine {
        Some(machine) => fleet.consume_lifecycle_event(machine),
        None => fleet.consume_event(),
    }
    Ok(code(FfiStatus::Ok))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_close<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
) -> jint {
    status(&mut env, |_| {
        let removed = {
            let mut map = fleets().lock().unwrap_or_else(|e| e.into_inner());
            if map
                .get(&(handle as u64))
                .is_some_and(|fleet| !fleet.can_close())
            {
                return Ok(code(FfiStatus::BusyHandle));
            }
            map.remove(&(handle as u64))
        };
        match removed {
            Some(f) => {
                drop(f);
                Ok(code(FfiStatus::Ok))
            }
            None => Ok(code(FfiStatus::StaleHandle)),
        }
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_replyWire<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    task: jint,
    request: jlong,
    payload: JByteArray<'a>,
) -> jint {
    status(&mut env, |env| {
        if machine < 0 || task < 0 || request < 0 || payload.len(env)? > 65536 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let Some(value) = experimental_fleet::decode_reply(&bytes(env, &payload)?) else {
            return Ok(code(FfiStatus::InvalidArgument));
        };
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(
                    f.scheduler
                        .reply(machine as u64, task as u32, request as u64, value),
                )
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_replyFailure<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    task: jint,
    request: jlong,
    kind: jint,
    detail: JByteArray<'a>,
) -> jint {
    status(&mut env, |env| {
        if machine < 0
            || task < 0
            || request < 0
            || detail.len(env)? == 0
            || detail.len(env)? > compukter_vm::MAXIMUM_HOST_FAILURE_DETAIL_BYTES
        {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let kind = match kind {
            0 => compukter_vm::HostFailureKind::EndOfFile,
            1 => compukter_vm::HostFailureKind::Unavailable,
            2 => compukter_vm::HostFailureKind::InputOutput,
            3 => compukter_vm::HostFailureKind::Cancelled,
            4 => compukter_vm::HostFailureKind::Other,
            _ => return Ok(code(FfiStatus::InvalidArgument)),
        };
        let Ok(detail) = String::from_utf8(bytes(env, &detail)?) else {
            return Ok(code(FfiStatus::InvalidArgument));
        };
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(f.scheduler.reply(
                    machine as u64,
                    task as u32,
                    request as u64,
                    ReplyValue::Failure(kind, detail.into_boxed_str()),
                ))
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_terminalKey<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    key: jint,
    action: jint,
    modifiers: jint,
    request: jlong,
) -> jint {
    status(&mut env, |_| {
        if machine < 0
            || request < -1
            || !(0..=u16::MAX as jint).contains(&key)
            || !(0..=u8::MAX as jint).contains(&modifiers)
        {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let Ok(key) = compukter_vm::TerminalKey::try_from(key as u16) else {
            return Ok(code(FfiStatus::InvalidArgument));
        };
        let action = match action {
            0 => compukter_vm::TerminalKeyAction::Press,
            1 => compukter_vm::TerminalKeyAction::Repeat,
            _ => return Ok(code(FfiStatus::InvalidArgument)),
        };
        let Ok(modifiers) = compukter_vm::TerminalModifiers::new(modifiers as u8) else {
            return Ok(code(FfiStatus::InvalidArgument));
        };
        let event = compukter_vm::TerminalKeyEvent::new(key, action, modifiers);
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(if request == -1 {
                    f.scheduler.terminal_key(machine as u64, event)
                } else {
                    f.scheduler
                        .terminal_key_checked(machine as u64, request as u64, event)
                })
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_terminalText<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    points: JIntArray<'a>,
    request: jlong,
) -> jint {
    status(&mut env, |env| {
        if machine < 0 || request < -1 || points.len(env)? > 4096 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let Some(text) = ints(env, &points)?
            .into_iter()
            .map(char::from_u32)
            .collect::<Option<String>>()
        else {
            return Ok(code(FfiStatus::InvalidArgument));
        };
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                let text = text.into_boxed_str();
                submit(if request == -1 {
                    f.scheduler.terminal_text(machine as u64, text)
                } else {
                    f.scheduler
                        .terminal_text_checked(machine as u64, request as u64, text)
                })
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_canonicalLine<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    line: JCharArray<'a>,
    request: jlong,
) -> jint {
    status(&mut env, |env| {
        if machine < 0 || request < -1 || line.len(env)? > 4096 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let line = chars(env, &line)?.into_boxed_slice();
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                submit(if request == -1 {
                    f.scheduler.canonical_line(machine as u64, line)
                } else {
                    f.scheduler
                        .canonical_line_checked(machine as u64, request as u64, line)
                })
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_attachSession<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    session: jlong,
    output: JLongArray<'a>,
) -> jint {
    status(&mut env, |env| {
        if output.len(env)? != 1 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let mut map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = map.get_mut(&(handle as u64)) else {
            return Ok(code(FfiStatus::StaleHandle));
        };
        let machine = match fleet.attach(session as u64, 0, Vec::new()) {
            Ok(machine) => machine,
            Err(status) => return Ok(code(status)),
        };
        write_handle(env, &output, machine)?;
        Ok(code(FfiStatus::Ok))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_attachSessionWithTimers<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    session: jlong,
    tick: jlong,
    packed: JByteArray<'a>,
    output: JLongArray<'a>,
) -> jint {
    status(&mut env, |env| {
        if tick < 0 || output.len(env)? != 1 || packed.len(env)? > 256 * 32 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let data = bytes(env, &packed)?;
        let Some(timers) = experimental_fleet::decode_timers(&data) else {
            return Ok(code(FfiStatus::InvalidArgument));
        };
        let mut map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = map.get_mut(&(handle as u64)) else {
            return Ok(code(FfiStatus::StaleHandle));
        };
        let machine = match fleet.attach(session as u64, tick as u64, timers) {
            Ok(machine) => machine,
            Err(status) => return Ok(code(status)),
        };
        write_handle(env, &output, machine)?;
        Ok(code(FfiStatus::Ok))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_detachWithTimers<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    tick: jlong,
) -> jint {
    status(&mut env, |_| {
        if machine < 0 || tick < 0 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |fleet| {
                if !fleet.owns_session(machine as u64) {
                    return code(FfiStatus::InvalidArgument);
                }
                submit(
                    fleet
                        .scheduler
                        .detach_with_timers(machine as u64, tick as u64),
                )
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_detach<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
) -> jint {
    status(&mut env, |_| {
        if machine < 0 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(handle as u64))
            .map_or(code(FfiStatus::StaleHandle), |f| {
                if !f.owns_session(machine as u64) {
                    return code(FfiStatus::InvalidArgument);
                }
                submit(f.scheduler.detach(machine as u64))
            }))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_restoreSession<
    'a,
>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    machine: jlong,
    output: JLongArray<'a>,
) -> jint {
    status(&mut env, |env| {
        if machine < 0 || output.len(env)? != 1 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let mut map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = map.get_mut(&(handle as u64)) else {
            return Ok(code(FfiStatus::StaleHandle));
        };
        match fleet.recover(machine as u64) {
            Ok(session) => {
                write_handle(env, &output, session)?;
                Ok(code(FfiStatus::Ok))
            }
            Err(status) => Ok(code(status)),
        }
    })
}
