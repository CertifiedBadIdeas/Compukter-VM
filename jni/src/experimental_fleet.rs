/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

//! Private experimental transport. No worker calls into the JVM.
use crate::{bytes, status, write_handle, write_long};
use compukter_ffi::{experimental_fleet, FfiStatus};
use compukter_vm::experimental_fleet_scheduler::{
    FleetConfig, FleetScheduler, ReplyValue, RequestInspection, SubmitError,
};
use jni::{
    objects::{JByteArray, JClass, JIntArray, JLongArray},
    sys::{jint, jlong},
    EnvUnowned,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Fleet {
    scheduler: FleetScheduler,
    pending: Option<Vec<u8>>,
    detached: HashMap<u64, Box<compukter_vm::ComputerMachine>>,
    sessions: HashMap<u64, experimental_fleet::SessionMetadata>,
}
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
            || config.len(env)? != 7
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
        let scheduler = match experimental_fleet::create_with_request_inspections(
            &bytes(env, &artifact)?,
            &bytes(env, &schemas)?,
            count as usize,
            cfg,
            inspections,
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
        map.insert(
            id,
            Fleet {
                scheduler,
                pending: None,
                detached: HashMap::new(),
                sessions: HashMap::new(),
            },
        );
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
            Instant::now().checked_add(Duration::from_nanos(nanos as u64))
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
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniExperimentalFleet_poll<'a>(
    mut env: EnvUnowned<'a>,
    _: JClass<'a>,
    handle: jlong,
    output: JByteArray<'a>,
    written: JLongArray<'a>,
) -> jint {
    status(&mut env, |env| {
        if written.len(env)? != 1 {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let capacity = output.len(env)?;
        let mut map = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(f) = map.get_mut(&(handle as u64)) else {
            return Ok(code(FfiStatus::StaleHandle));
        };
        if f.pending.is_none() {
            f.pending = match experimental_fleet::poll_event(&f.scheduler) {
                Ok(None) => None,
                Ok(Some(experimental_fleet::WireEvent::Outcome(b))) => Some([vec![0], b].concat()),
                Ok(Some(experimental_fleet::WireEvent::Inspection(b))) => {
                    Some([vec![1], b].concat())
                }
                Ok(Some(experimental_fleet::WireEvent::ScopeStopped {
                    machine,
                    scope,
                    pending,
                })) => {
                    let mut b = vec![4];
                    b.extend_from_slice(&machine.to_le_bytes());
                    b.extend_from_slice(&scope.to_le_bytes());
                    b.extend_from_slice(&(pending.len() as u32).to_le_bytes());
                    for (task, request) in pending {
                        b.extend_from_slice(&task.to_le_bytes());
                        b.extend_from_slice(&request.to_le_bytes());
                    }
                    Some(b)
                }
                Ok(Some(experimental_fleet::WireEvent::Stopped { machine, pending })) => {
                    f.sessions.remove(&machine);
                    let mut b = vec![2];
                    b.extend_from_slice(&machine.to_le_bytes());
                    b.extend_from_slice(&(pending.len() as u32).to_le_bytes());
                    for (task, request) in pending {
                        b.extend_from_slice(&task.to_le_bytes());
                        b.extend_from_slice(&request.to_le_bytes());
                    }
                    Some(b)
                }
                Ok(Some(experimental_fleet::WireEvent::Detached { machine, computer })) => {
                    f.detached.insert(machine, computer);
                    let mut bytes = vec![6];
                    bytes.extend_from_slice(&machine.to_le_bytes());
                    Some(bytes)
                }
                Ok(Some(experimental_fleet::WireEvent::DetachRejected { machine })) => {
                    let mut bytes = vec![7];
                    bytes.extend_from_slice(&machine.to_le_bytes());
                    Some(bytes)
                }
                Ok(Some(experimental_fleet::WireEvent::Error { machine, message })) => {
                    let mut bytes = vec![3];
                    bytes.extend_from_slice(&machine.to_le_bytes());
                    bytes.extend_from_slice(message.as_bytes());
                    Some(bytes)
                }
                Err(e) => Some([vec![5], e.into_bytes()].concat()),
            };
        }
        let Some(b) = f.pending.as_ref() else {
            write_long(env, &written, 0)?;
            return Ok(code(FfiStatus::Ok));
        };
        write_long(env, &written, b.len())?;
        if capacity < b.len() {
            return Ok(code(FfiStatus::BufferTooSmall));
        }
        let signed = unsafe { std::slice::from_raw_parts(b.as_ptr().cast::<i8>(), b.len()) };
        output.set_region(env, 0, signed)?;
        f.pending = None;
        Ok(code(FfiStatus::Ok))
    })
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
                .is_some_and(|fleet| !fleet.sessions.is_empty())
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
) -> jint {
    status(&mut env, |_| {
        if machine < 0
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
                submit(f.scheduler.terminal_key(machine as u64, event))
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
        if fleet.sessions.try_reserve(1).is_err() || fleet.detached.try_reserve(1).is_err() {
            return Ok(code(FfiStatus::Internal));
        }
        let (machine, metadata) =
            match experimental_fleet::attach_session(&fleet.scheduler, session as u64) {
                Ok(value) => value,
                Err(status) => return Ok(code(status)),
            };
        fleet.sessions.insert(machine, metadata);
        write_handle(env, &output, machine)?;
        Ok(code(FfiStatus::Ok))
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
                if !f.sessions.contains_key(&(machine as u64)) {
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
        let key = machine as u64;
        if !fleet.detached.contains_key(&key) || !fleet.sessions.contains_key(&key) {
            return Ok(code(FfiStatus::InvalidArgument));
        }
        let computer = fleet.detached.remove(&key).expect("checked computer");
        let metadata = fleet.sessions.remove(&key).expect("checked metadata");
        match experimental_fleet::restore_session(computer, metadata) {
            Ok(session) => {
                write_handle(env, &output, session)?;
                Ok(code(FfiStatus::Ok))
            }
            Err((error, computer, metadata)) => {
                fleet.detached.insert(key, computer);
                fleet.sessions.insert(key, metadata);
                Ok(code(error))
            }
        }
    })
}
