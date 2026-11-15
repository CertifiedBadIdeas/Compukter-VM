/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

//! Experimental C transport; it is enabled explicitly at build time.
use crate::experimental_fleet::{self, FleetTransport};
use crate::FfiStatus;
use compukter_vm::experimental_fleet_scheduler::{FleetConfig, SubmitError};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static FLEETS: OnceLock<Mutex<HashMap<u64, FleetTransport>>> = OnceLock::new();
static NEXT_FLEET: AtomicU64 = AtomicU64::new(1);
fn fleets() -> &'static Mutex<HashMap<u64, FleetTransport>> {
    FLEETS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn guarded(f: impl FnOnce() -> FfiStatus) -> FfiStatus {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(FfiStatus::Internal)
}
fn submission(result: Result<(), SubmitError>) -> FfiStatus {
    match result {
        Ok(()) => FfiStatus::Ok,
        Err(SubmitError::Full) => FfiStatus::BusyHandle,
        Err(SubmitError::Closed) => FfiStatus::StaleHandle,
        Err(_) => FfiStatus::InvalidArgument,
    }
}

/// Creates an empty fleet. `config` contains eight u32 values in JNI configuration order.
///
/// # Safety
/// `config` must point to eight readable u32s and `output` to one writable u64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_create(
    config: *const u32,
    output: *mut u64,
) -> FfiStatus {
    guarded(|| {
        if config.is_null() || output.is_null() {
            return FfiStatus::InvalidArgument;
        }
        // SAFETY: required by the ABI contract.
        let c = unsafe { std::slice::from_raw_parts(config, 8) };
        unsafe { create_fleet(c, output, Vec::new()) }
    })
}

unsafe fn create_fleet(
    c: &[u32],
    output: *mut u64,
    inspections: Vec<compukter_vm::experimental_fleet_scheduler::RequestInspection>,
) -> FfiStatus {
    if !(1..=64).contains(&c[0])
        || !(1..=65536).contains(&c[1])
        || !(1..=65536).contains(&c[2])
        || c[3] == 0
        || c[5] == 0
        || c[6] == 0
        || c[7] > 1
    {
        return FfiStatus::InvalidArgument;
    }
    let config = FleetConfig {
        workers: c[0] as usize,
        mailbox_capacity: c[1] as usize,
        event_capacity: c[2] as usize,
        guest_budget: c[3],
        maintenance_budget: c[4],
        host_budget: c[5],
        maximum_advances_per_tick: c[6],
    };
    let scheduler = match experimental_fleet::create_with_coordinated_turns(
        &[],
        &[1, 0],
        0,
        config,
        inspections,
        c[7] == 1,
    ) {
        Ok(fleet) => fleet,
        Err(_) => return FfiStatus::InvalidArgument,
    };
    let id = match NEXT_FLEET.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
        (value < i64::MAX as u64).then_some(value + 1)
    }) {
        Ok(value) => value,
        Err(_) => return FfiStatus::HandleExhausted,
    };
    let mut all = fleets().lock().unwrap_or_else(|e| e.into_inner());
    if all.len() >= 64 {
        drop(all);
        drop(scheduler);
        return FfiStatus::BusyHandle;
    }
    all.insert(id, FleetTransport::new(scheduler));
    // SAFETY: required by the ABI contract.
    unsafe {
        output.write(id);
    }
    FfiStatus::Ok
}

/// Creates a fleet with one host operation inspected before it consumes request quota.
///
/// # Safety
/// `config` names eight readable u32s, `output` one writable u64, and each text
/// pointer names a readable region of its declared length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_create_inspection(
    config: *const u32,
    namespace: *const u8,
    namespace_len: usize,
    name: *const u8,
    name_len: usize,
    detail: *const u8,
    detail_len: usize,
    major: u32,
    minor: u32,
    operation: u32,
    output: *mut u64,
) -> FfiStatus {
    guarded(|| {
        if config.is_null()
            || output.is_null()
            || namespace.is_null()
            || name.is_null()
            || detail.is_null()
            || namespace_len == 0
            || namespace_len > 256
            || name_len == 0
            || name_len > 256
            || detail_len == 0
            || detail_len > compukter_vm::MAXIMUM_HOST_FAILURE_DETAIL_BYTES
            || major > u16::MAX as u32
            || minor > u16::MAX as u32
        {
            return FfiStatus::InvalidArgument;
        }
        let c = unsafe { std::slice::from_raw_parts(config, 8) };
        let namespace = unsafe { std::slice::from_raw_parts(namespace, namespace_len) };
        let name = unsafe { std::slice::from_raw_parts(name, name_len) };
        let detail = unsafe { std::slice::from_raw_parts(detail, detail_len) };
        let (Ok(namespace), Ok(name), Ok(detail)) = (
            std::str::from_utf8(namespace),
            std::str::from_utf8(name),
            std::str::from_utf8(detail),
        ) else {
            return FfiStatus::InvalidArgument;
        };
        let inspection = compukter_vm::experimental_fleet_scheduler::RequestInspection {
            namespace: namespace.into(),
            name: name.into(),
            abi_major: major as u16,
            abi_minor: minor as u16,
            operation,
            over_budget_detail: detail.into(),
        };
        unsafe { create_fleet(c, output, vec![inspection]) }
    })
}

/// Transfers a session without pending external operations to the fleet.
///
/// # Safety
/// `output` must point to one writable u64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_attach(
    handle: u64,
    session: u64,
    output: *mut u64,
) -> FfiStatus {
    guarded(|| {
        if output.is_null() {
            return FfiStatus::InvalidArgument;
        }
        let mut all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get_mut(&handle) else {
            return FfiStatus::StaleHandle;
        };
        match fleet.attach(session, 0, Vec::new()) {
            Ok(machine) => {
                unsafe { output.write(machine) };
                FfiStatus::Ok
            }
            Err(error) => error,
        }
    })
}

/// Transfers a session with saved timer metadata. On refusal the session remains owned by the caller.
///
/// # Safety
/// `output` must point to one writable u64. If `timers_len` is nonzero, `timers`
/// must point to that many readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_attach_timers(
    handle: u64,
    session: u64,
    tick: u64,
    timers: *const u8,
    timers_len: usize,
    output: *mut u64,
) -> FfiStatus {
    guarded(|| {
        if output.is_null() || timers_len > 256 * 32 || (timers_len != 0 && timers.is_null()) {
            return FfiStatus::InvalidArgument;
        }
        let data = if timers_len == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(timers, timers_len) }
        };
        let Some(timers) = experimental_fleet::decode_timers(data) else {
            return FfiStatus::InvalidArgument;
        };
        let mut all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get_mut(&handle) else {
            return FfiStatus::StaleHandle;
        };
        match fleet.attach(session, tick, timers) {
            Ok(machine) => {
                unsafe { output.write(machine) };
                FfiStatus::Ok
            }
            Err(error) => error,
        }
    })
}

/// Grants one tick. A negative `deadline_nanos` means no deadline.
#[unsafe(no_mangle)]
pub extern "C" fn compukter_experimental_fleet_grant(
    handle: u64,
    machine: u64,
    tick: u64,
    retired: u32,
    deadline_nanos: i64,
) -> FfiStatus {
    guarded(|| {
        if deadline_nanos < -1 {
            return FfiStatus::InvalidArgument;
        }
        let deadline = if deadline_nanos < 0 {
            None
        } else {
            let Some(deadline) =
                Instant::now().checked_add(Duration::from_nanos(deadline_nanos as u64))
            else {
                return FfiStatus::InvalidArgument;
            };
            Some(deadline)
        };
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        all.get(&handle).map_or(FfiStatus::StaleHandle, |fleet| {
            submission(
                fleet
                    .scheduler
                    .grant_until(machine, tick, retired, deadline),
            )
        })
    })
}

/// Requests detachment; completion must be consumed before recovery.
#[unsafe(no_mangle)]
pub extern "C" fn compukter_experimental_fleet_detach(handle: u64, machine: u64) -> FfiStatus {
    guarded(|| {
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        all.get(&handle).map_or(FfiStatus::StaleHandle, |fleet| {
            if !fleet.owns_session(machine) {
                return FfiStatus::InvalidArgument;
            }
            submission(fleet.scheduler.detach(machine))
        })
    })
}

/// Requests detachment while atomically preserving pending timers.
#[unsafe(no_mangle)]
pub extern "C" fn compukter_experimental_fleet_detach_timers(
    handle: u64,
    machine: u64,
    tick: u64,
) -> FfiStatus {
    guarded(|| {
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        all.get(&handle).map_or(FfiStatus::StaleHandle, |fleet| {
            if !fleet.owns_session(machine) {
                return FfiStatus::InvalidArgument;
            }
            submission(fleet.scheduler.detach_with_timers(machine, tick))
        })
    })
}

/// Stops a machine; its pending request identities arrive in the stop event.
#[unsafe(no_mangle)]
pub extern "C" fn compukter_experimental_fleet_stop(handle: u64, machine: u64) -> FfiStatus {
    guarded(|| {
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        all.get(&handle).map_or(FfiStatus::StaleHandle, |fleet| {
            submission(fleet.scheduler.stop(machine))
        })
    })
}

/// Recovers a detached session; refusal retains it in the fleet.
///
/// # Safety
/// `output` must point to one writable u64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_recover(
    handle: u64,
    machine: u64,
    output: *mut u64,
) -> FfiStatus {
    guarded(|| {
        if output.is_null() {
            return FfiStatus::InvalidArgument;
        }
        let mut all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get_mut(&handle) else {
            return FfiStatus::StaleHandle;
        };
        match fleet.recover(machine) {
            Ok(session) => {
                unsafe { output.write(session) };
                FfiStatus::Ok
            }
            Err(error) => error,
        }
    })
}

/// Reads one event. A small output buffer does not consume the event.
///
/// # Safety
/// `written` must point to one writable usize. A nonempty output must point to
/// a writable region of `capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_poll(
    handle: u64,
    output: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> FfiStatus {
    guarded(|| {
        if written.is_null() || (capacity > 0 && output.is_null()) {
            return FfiStatus::InvalidArgument;
        }
        let mut all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get_mut(&handle) else {
            return FfiStatus::StaleHandle;
        };
        let Some(event) = fleet.poll() else {
            unsafe { written.write(0) };
            return FfiStatus::Ok;
        };
        unsafe { written.write(event.len()) };
        if event.len() > capacity {
            return FfiStatus::BufferTooSmall;
        }
        unsafe { std::ptr::copy_nonoverlapping(event.as_ptr(), output, event.len()) };
        fleet.consume_event();
        FfiStatus::Ok
    })
}

/// Closes an empty fleet; attached or detached sessions must first be recovered or stopped.
#[unsafe(no_mangle)]
pub extern "C" fn compukter_experimental_fleet_close(handle: u64) -> FfiStatus {
    guarded(|| {
        let removed = {
            let mut all = fleets().lock().unwrap_or_else(|e| e.into_inner());
            if all.get(&handle).is_some_and(|fleet| !fleet.can_close()) {
                return FfiStatus::BusyHandle;
            }
            all.remove(&handle)
        };
        match removed {
            Some(fleet) => {
                drop(fleet);
                FfiStatus::Ok
            }
            None => FfiStatus::StaleHandle,
        }
    })
}

/// Completes an asynchronous host request with the ordinary host-response wire.
///
/// # Safety
/// A nonempty payload must name `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_reply(
    handle: u64,
    machine: u64,
    task: u32,
    request: u64,
    payload: *const u8,
    length: usize,
) -> FfiStatus {
    guarded(|| {
        if task == 0 || request == 0 || length > 65536 || (length > 0 && payload.is_null()) {
            return FfiStatus::InvalidArgument;
        }
        let bytes = if length == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(payload, length) }
        };
        let Some(value) = experimental_fleet::decode_reply(bytes) else {
            return FfiStatus::InvalidArgument;
        };
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        all.get(&handle).map_or(FfiStatus::StaleHandle, |fleet| {
            submission(fleet.scheduler.reply(machine, task, request, value))
        })
    })
}

/// Completes an asynchronous host request with a bounded failure.
///
/// # Safety
/// `detail` must name `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_reply_failure(
    handle: u64,
    machine: u64,
    task: u32,
    request: u64,
    kind: u32,
    detail: *const u8,
    length: usize,
) -> FfiStatus {
    guarded(|| {
        use compukter_vm::{experimental_fleet_scheduler::ReplyValue, HostFailureKind as K};
        if task == 0
            || request == 0
            || length == 0
            || length > compukter_vm::MAXIMUM_HOST_FAILURE_DETAIL_BYTES
            || detail.is_null()
        {
            return FfiStatus::InvalidArgument;
        }
        let kind = match kind {
            0 => K::EndOfFile,
            1 => K::Unavailable,
            2 => K::InputOutput,
            3 => K::Cancelled,
            4 => K::Other,
            _ => return FfiStatus::InvalidArgument,
        };
        let bytes = unsafe { std::slice::from_raw_parts(detail, length) };
        let Ok(detail) = std::str::from_utf8(bytes) else {
            return FfiStatus::InvalidArgument;
        };
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        all.get(&handle).map_or(FfiStatus::StaleHandle, |fleet| {
            submission(fleet.scheduler.reply(
                machine,
                task,
                request,
                ReplyValue::Failure(kind, detail.into()),
            ))
        })
    })
}

/// Completes host-side cost inspection for an outstanding request.
#[unsafe(no_mangle)]
pub extern "C" fn compukter_experimental_fleet_resolve_cost(
    handle: u64,
    machine: u64,
    task: u32,
    request: u64,
    units: u32,
) -> FfiStatus {
    guarded(|| {
        if task == 0 || request == 0 || units == 0 {
            return FfiStatus::InvalidArgument;
        }
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        all.get(&handle).map_or(FfiStatus::StaleHandle, |fleet| {
            submission(fleet.scheduler.resolve_cost(machine, task, request, units))
        })
    })
}

/// Completes a pending compiler transaction with an artifact (kind 0) or UTF-8 diagnostic (kind 1).
///
/// # Safety
/// A nonempty payload must name `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_compilation(
    handle: u64,
    machine: u64,
    token: u64,
    kind: u32,
    payload: *const u8,
    length: usize,
) -> FfiStatus {
    guarded(|| {
        use compukter_vm::experimental_fleet_scheduler::CompilationResult;
        if token == 0
            || length > compukter_vm::ArtifactLimits::default().artifact_bytes
            || (length > 0 && payload.is_null())
        {
            return FfiStatus::InvalidArgument;
        }
        let bytes = if length == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(payload, length) }
        };
        let result = match kind {
            0 => CompilationResult::Artifact(bytes.into()),
            1 => match std::str::from_utf8(bytes) {
                Ok(detail) => CompilationResult::Failure(detail.into()),
                Err(_) => return FfiStatus::InvalidArgument,
            },
            _ => return FfiStatus::InvalidArgument,
        };
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        all.get(&handle).map_or(FfiStatus::StaleHandle, |fleet| {
            submission(fleet.scheduler.complete_compilation(machine, token, result))
        })
    })
}

/// Enqueues an observation: 0 terminal update, 1 resources, 2 filesystem generation.
#[unsafe(no_mangle)]
pub extern "C" fn compukter_experimental_fleet_observe(
    handle: u64,
    machine: u64,
    request: u64,
    kind: u32,
    revision: u64,
) -> FfiStatus {
    guarded(|| {
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get(&handle) else {
            return FfiStatus::StaleHandle;
        };
        submission(match kind {
            0 => fleet.scheduler.terminal_update(machine, request, revision),
            1 if revision == 0 => fleet.scheduler.resource_snapshot(machine, request),
            2 if revision == 0 => fleet.scheduler.filesystem_generation(machine, request),
            _ => return FfiStatus::InvalidArgument,
        })
    })
}

/// Enqueues redstone input (kind 0) or confirms applied output (kind 1).
#[unsafe(no_mangle)]
pub extern "C" fn compukter_experimental_fleet_redstone(
    handle: u64,
    machine: u64,
    kind: u32,
    packet: u32,
) -> FfiStatus {
    guarded(|| {
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get(&handle) else {
            return FfiStatus::StaleHandle;
        };
        submission(match kind {
            0 => fleet.scheduler.redstone_input(machine, packet),
            1 => fleet.scheduler.confirm_redstone_output(machine, packet),
            _ => return FfiStatus::InvalidArgument,
        })
    })
}

/// Enqueues a terminal key with an optional nonnegative completion request; -1 omits acknowledgement.
#[unsafe(no_mangle)]
pub extern "C" fn compukter_experimental_fleet_terminal_key(
    handle: u64,
    machine: u64,
    key: u32,
    action: u32,
    modifiers: u32,
    request: i64,
) -> FfiStatus {
    guarded(|| {
        if key > u16::MAX as u32 || modifiers > u8::MAX as u32 || request < -1 {
            return FfiStatus::InvalidArgument;
        }
        let Ok(key) = compukter_vm::TerminalKey::try_from(key as u16) else {
            return FfiStatus::InvalidArgument;
        };
        let action = match action {
            0 => compukter_vm::TerminalKeyAction::Press,
            1 => compukter_vm::TerminalKeyAction::Repeat,
            _ => return FfiStatus::InvalidArgument,
        };
        let Ok(modifiers) = compukter_vm::TerminalModifiers::new(modifiers as u8) else {
            return FfiStatus::InvalidArgument;
        };
        let event = compukter_vm::TerminalKeyEvent::new(key, action, modifiers);
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get(&handle) else {
            return FfiStatus::StaleHandle;
        };
        submission(if request == -1 {
            fleet.scheduler.terminal_key(machine, event)
        } else {
            fleet
                .scheduler
                .terminal_key_checked(machine, request as u64, event)
        })
    })
}

/// Enqueues UTF-8 terminal text with an optional completion acknowledgement.
///
/// # Safety
/// A nonempty payload must name `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_terminal_text(
    handle: u64,
    machine: u64,
    payload: *const u8,
    length: usize,
    request: i64,
) -> FfiStatus {
    guarded(|| {
        if length > 16384 || request < -1 || (length > 0 && payload.is_null()) {
            return FfiStatus::InvalidArgument;
        }
        let bytes = if length == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(payload, length) }
        };
        let Ok(text) = std::str::from_utf8(bytes) else {
            return FfiStatus::InvalidArgument;
        };
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get(&handle) else {
            return FfiStatus::StaleHandle;
        };
        submission(if request == -1 {
            fleet.scheduler.terminal_text(machine, text.into())
        } else {
            fleet
                .scheduler
                .terminal_text_checked(machine, request as u64, text.into())
        })
    })
}

/// Enqueues UTF-16 canonical input with an optional completion acknowledgement.
///
/// # Safety
/// A nonempty payload must name `length` readable, aligned u16 values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_canonical_line(
    handle: u64,
    machine: u64,
    payload: *const u16,
    length: usize,
    request: i64,
) -> FfiStatus {
    guarded(|| {
        if length > 4096 || request < -1 || (length > 0 && payload.is_null()) {
            return FfiStatus::InvalidArgument;
        }
        let line = if length == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(payload, length) }
        };
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get(&handle) else {
            return FfiStatus::StaleHandle;
        };
        submission(if request == -1 {
            fleet.scheduler.canonical_line(machine, line.into())
        } else {
            fleet
                .scheduler
                .canonical_line_checked(machine, request as u64, line.into())
        })
    })
}

/// Enqueues stat (0), list (1), or generation-checked read (2).
///
/// # Safety
/// Nonempty `path` and `cursor` must name readable regions of their declared lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_filesystem(
    handle: u64,
    machine: u64,
    request: u64,
    kind: u32,
    path: *const u8,
    path_len: usize,
    cursor: *const u8,
    cursor_len: usize,
    offset: u64,
    maximum: u32,
    generation: u64,
) -> FfiStatus {
    guarded(|| {
        if path_len > 4096
            || cursor_len > 4096
            || (path_len > 0 && path.is_null())
            || (cursor_len > 0 && cursor.is_null())
        {
            return FfiStatus::InvalidArgument;
        }
        let path = if path_len == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(path, path_len) }
        };
        let cursor = if cursor_len == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(cursor, cursor_len) }
        };
        let (Ok(path), Ok(cursor)) = (std::str::from_utf8(path), std::str::from_utf8(cursor))
        else {
            return FfiStatus::InvalidArgument;
        };
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get(&handle) else {
            return FfiStatus::StaleHandle;
        };
        submission(match kind {
            0 if cursor.is_empty() && offset == 0 && maximum == 0 && generation == 0 => {
                fleet.scheduler.file_stat(machine, request, path.into())
            }
            1 if offset == 0 && generation == 0 => fleet.scheduler.file_list(
                machine,
                request,
                path.into(),
                (!cursor.is_empty()).then(|| cursor.into()),
                maximum,
            ),
            2 if cursor.is_empty() => fleet.scheduler.file_read(
                machine,
                request,
                path.into(),
                offset,
                maximum,
                generation,
            ),
            _ => return FfiStatus::InvalidArgument,
        })
    })
}

/// Enqueues preparation (0), revision query (1), installation (2), or discard (3).
///
/// # Safety
/// A nonempty payload must name `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compukter_experimental_fleet_deployment(
    handle: u64,
    machine: u64,
    request: u64,
    kind: u32,
    payload: *const u8,
    length: usize,
    candidate: u64,
    revision: u64,
) -> FfiStatus {
    guarded(|| {
        let limit = if kind == 0 {
            compukter_vm::ArtifactLimits::default().artifact_bytes
        } else {
            4096
        };
        if length > limit || (length > 0 && payload.is_null()) {
            return FfiStatus::InvalidArgument;
        }
        let bytes = if length == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(payload, length) }
        };
        let all = fleets().lock().unwrap_or_else(|e| e.into_inner());
        let Some(fleet) = all.get(&handle) else {
            return FfiStatus::StaleHandle;
        };
        submission(match kind {
            0 if candidate == 0 && revision == 0 => {
                fleet
                    .scheduler
                    .prepare_deployment(machine, request, bytes.into())
            }
            1 if candidate == 0 && revision == 0 => match std::str::from_utf8(bytes) {
                Ok(path) => fleet
                    .scheduler
                    .deployment_revision(machine, request, path.into()),
                Err(_) => return FfiStatus::InvalidArgument,
            },
            2 if candidate > 0 => match std::str::from_utf8(bytes) {
                Ok(path) => fleet.scheduler.install_deployment(
                    machine,
                    request,
                    path.into(),
                    if revision == 0 {
                        compukter_vm::ExecutableRevision::Absent
                    } else {
                        compukter_vm::ExecutableRevision::Present(revision)
                    },
                    candidate,
                ),
                Err(_) => return FfiStatus::InvalidArgument,
            },
            3 if candidate > 0 && revision == 0 && bytes.is_empty() => fleet
                .scheduler
                .discard_deployment(machine, request, candidate),
            _ => return FfiStatus::InvalidArgument,
        })
    })
}
