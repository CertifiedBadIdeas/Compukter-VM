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

use compukter_ffi::{
    compukter_abi_version, compukter_advance, compukter_close, compukter_compilation_complete,
    compukter_compilation_request_copy, compukter_compilation_request_size, compukter_create,
    compukter_create_boot_in_store, compukter_create_in_store, compukter_deploy,
    compukter_deployment_candidate_close, compukter_executable_revision,
    compukter_filesystem_generation, compukter_filesystem_list, compukter_filesystem_read,
    compukter_filesystem_stat, compukter_max_create_bytes, compukter_max_outcome_bytes,
    compukter_redstone_confirm_output, compukter_redstone_submit_input,
    compukter_resource_snapshot, compukter_resume_bool, compukter_resume_f32_bits,
    compukter_resume_failure, compukter_resume_i32, compukter_resume_string, compukter_resume_unit,
    compukter_store_close, compukter_store_durable_generation, compukter_store_flush,
    compukter_store_health, compukter_store_open, compukter_store_recover,
    compukter_store_tombstone, compukter_submit_canonical_line, compukter_terminal_changes_since,
    compukter_terminal_commit, compukter_terminal_full_state, compukter_terminal_key,
    compukter_terminal_text, compukter_verify_artifact, compukter_verify_for_deploy, FfiStatus,
};
use jni::{
    errors::{Result, ThrowRuntimeExAndDefault},
    objects::{JByteArray, JCharArray, JClass, JIntArray, JLongArray},
    sys::{jboolean, jint, jlong},
    Env, EnvUnowned,
};

const INVALID_ARGUMENT: jint = FfiStatus::InvalidArgument as jint;

fn bytes(env: &Env<'_>, value: &JByteArray<'_>) -> Result<Vec<u8>> {
    env.convert_byte_array(value)
}

fn chars(env: &Env<'_>, value: &JCharArray<'_>) -> Result<Vec<u16>> {
    let mut result = vec![0; value.len(env)?];
    value.get_region(env, 0, &mut result)?;
    Ok(result)
}

fn ints(env: &Env<'_>, value: &JIntArray<'_>) -> Result<Vec<u32>> {
    let mut signed = vec![0; value.len(env)?];
    value.get_region(env, 0, &mut signed)?;
    Ok(signed.into_iter().map(|item| item as u32).collect())
}

fn write_long(env: &Env<'_>, output: &JLongArray<'_>, value: usize) -> Result<()> {
    output.set_region(env, 0, &[value as jlong])
}

fn write_handle(env: &Env<'_>, output: &JLongArray<'_>, value: u64) -> Result<()> {
    output.set_region(env, 0, &[value as jlong])
}

fn output_call(
    env: &Env<'_>,
    output: &JByteArray<'_>,
    written_out: &JLongArray<'_>,
    call: impl FnOnce(*mut u8, usize, *mut usize) -> FfiStatus,
) -> Result<jint> {
    if written_out.len(env)? != 1 {
        return Ok(INVALID_ARGUMENT);
    }
    let mut buffer = vec![0; output.len(env)?];
    let mut written = 0usize;
    let status = call(buffer.as_mut_ptr(), buffer.len(), &mut written);
    write_long(env, written_out, written)?;
    if status == FfiStatus::Ok && written <= buffer.len() {
        let signed = unsafe { core::slice::from_raw_parts(buffer.as_ptr().cast::<i8>(), written) };
        output.set_region(env, 0, signed)?;
    }
    Ok(status as jint)
}

fn status<'caller>(
    env: &mut EnvUnowned<'caller>,
    call: impl FnOnce(&mut Env<'caller>) -> Result<jint>,
) -> jint {
    env.with_env(call).resolve::<ThrowRuntimeExAndDefault>()
}

macro_rules! simple_status {
    ($name:ident ( $($argument:ident : $type:ty),* $(,)? ) => $call:expr) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn $name<'caller>(
            mut env: EnvUnowned<'caller>,
            _class: JClass<'caller>,
            $($argument: $type),*
        ) -> jint {
            status(&mut env, |_| Ok($call as jint))
        }
    };
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_abiVersion<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
) -> jint {
    status(&mut env, |_| Ok(compukter_abi_version() as jint))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_maximumOutcomeBytes<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
) -> jlong {
    env.with_env(|_| -> Result<jlong> { Ok(compukter_max_outcome_bytes() as jlong) })
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_maximumCreateBytes<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
) -> jlong {
    env.with_env(|_| -> Result<jlong> { Ok(compukter_max_create_bytes() as jlong) })
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_verifyArtifact<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    artifact: JByteArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let artifact = bytes(env, &artifact)?;
        Ok(unsafe { compukter_verify_artifact(artifact.as_ptr(), artifact.len()) } as jint)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_storeOpen<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    root: JByteArray<'caller>,
    limits: JByteArray<'caller>,
    output: JByteArray<'caller>,
    written: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let root = bytes(env, &root)?;
        let limits = bytes(env, &limits)?;
        output_call(env, &output, &written, |out, capacity, count| unsafe {
            compukter_store_open(
                root.as_ptr(),
                root.len(),
                limits.as_ptr(),
                limits.len(),
                out,
                capacity,
                count,
            )
        })
    })
}

macro_rules! handle_output {
    ($name:ident, $call:path) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn $name<'caller>(
            mut env: EnvUnowned<'caller>,
            _class: JClass<'caller>,
            handle: jlong,
            output: JByteArray<'caller>,
            written: JLongArray<'caller>,
        ) -> jint {
            status(&mut env, |env| {
                output_call(env, &output, &written, |out, capacity, count| unsafe {
                    $call(handle as u64, out, capacity, count)
                })
            })
        }
    };
}

handle_output!(
    Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_storeHealth,
    compukter_store_health
);
handle_output!(
    Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_filesystemGeneration,
    compukter_filesystem_generation
);
handle_output!(
    Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_resourceSnapshot,
    compukter_resource_snapshot
);
handle_output!(
    Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_terminalFullState,
    compukter_terminal_full_state
);

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_storeDurableGeneration<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    id: JByteArray<'caller>,
    output: JByteArray<'caller>,
    written: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let id = bytes(env, &id)?;
        if id.len() != 16 {
            return Ok(INVALID_ARGUMENT);
        }
        output_call(env, &output, &written, |out, capacity, count| unsafe {
            compukter_store_durable_generation(handle as u64, id.as_ptr(), out, capacity, count)
        })
    })
}

macro_rules! handle_id_status {
    ($name:ident, $call:path) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn $name<'caller>(
            mut env: EnvUnowned<'caller>,
            _class: JClass<'caller>,
            handle: jlong,
            id: JByteArray<'caller>,
        ) -> jint {
            status(&mut env, |env| {
                let id = bytes(env, &id)?;
                if id.len() != 16 {
                    return Ok(INVALID_ARGUMENT);
                }
                Ok(unsafe { $call(handle as u64, id.as_ptr()) } as jint)
            })
        }
    };
}

handle_id_status!(
    Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_storeTombstone,
    compukter_store_tombstone
);
handle_id_status!(
    Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_storeRecover,
    compukter_store_recover
);

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_storeFlush<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    id: JByteArray<'caller>,
    generation: jlong,
) -> jint {
    status(&mut env, |env| {
        let id = bytes(env, &id)?;
        if id.len() != 16 {
            return Ok(INVALID_ARGUMENT);
        }
        Ok(unsafe { compukter_store_flush(handle as u64, id.as_ptr(), generation as u64) } as jint)
    })
}

simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_storeClose(handle: jlong) => compukter_store_close(handle as u64));
simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_close(handle: jlong) => compukter_close(handle as u64));
simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_deploymentCandidateClose(handle: jlong) => compukter_deployment_candidate_close(handle as u64));
simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_submitRedstoneInput(handle: jlong, packet: jint) => compukter_redstone_submit_input(handle as u64, packet as u32));
simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_confirmRedstoneOutput(handle: jlong, packet: jint) => compukter_redstone_confirm_output(handle as u64, packet as u32));
simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_resumeUnit(handle: jlong, task_id: jint, request_id: jlong) => compukter_resume_unit(handle as u64, task_id as u32, request_id as u64));
simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_resumeInt(handle: jlong, task_id: jint, request_id: jlong, value: jint) => compukter_resume_i32(handle as u64, task_id as u32, request_id as u64, value));
simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_resumeFloatBits(handle: jlong, task_id: jint, request_id: jlong, bits: jint) => compukter_resume_f32_bits(handle as u64, task_id as u32, request_id as u64, bits as u32));
simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_resumeBool(handle: jlong, task_id: jint, request_id: jlong, value: jboolean) => compukter_resume_bool(handle as u64, task_id as u32, request_id as u64, u32::from(value)));
simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_resumeFailure(handle: jlong, task_id: jint, request_id: jlong, kind: jint, code: jint) => compukter_resume_failure(handle as u64, task_id as u32, request_id as u64, kind as u32, code as u32));
simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_terminalCommit(handle: jlong) => compukter_terminal_commit(handle as u64));
simple_status!(Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_terminalKey(handle: jlong, key: jint, action: jint, modifiers: jint) => compukter_terminal_key(handle as u64, key as u16, action as u32, modifiers as u32));

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_create<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    artifact: JByteArray<'caller>,
    output: JByteArray<'caller>,
    written: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let artifact = bytes(env, &artifact)?;
        output_call(env, &output, &written, |out, capacity, count| unsafe {
            compukter_create(artifact.as_ptr(), artifact.len(), out, capacity, count)
        })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_createInStore<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    store_handle: jlong,
    id: JByteArray<'caller>,
    rom: JByteArray<'caller>,
    artifact: JByteArray<'caller>,
    output: JByteArray<'caller>,
    written: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let id = bytes(env, &id)?;
        let rom = bytes(env, &rom)?;
        let artifact = bytes(env, &artifact)?;
        if id.len() != 16 {
            return Ok(INVALID_ARGUMENT);
        }
        output_call(env, &output, &written, |out, capacity, count| unsafe {
            compukter_create_in_store(
                store_handle as u64,
                id.as_ptr(),
                rom.as_ptr(),
                rom.len(),
                artifact.as_ptr(),
                artifact.len(),
                out,
                capacity,
                count,
            )
        })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_createBootInStore<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    store_handle: jlong,
    id: JByteArray<'caller>,
    rom: JByteArray<'caller>,
    output: JByteArray<'caller>,
    written: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let id = bytes(env, &id)?;
        let rom = bytes(env, &rom)?;
        if id.len() != 16 {
            return Ok(INVALID_ARGUMENT);
        }
        output_call(env, &output, &written, |out, capacity, count| unsafe {
            compukter_create_boot_in_store(
                store_handle as u64,
                id.as_ptr(),
                rom.as_ptr(),
                rom.len(),
                out,
                capacity,
                count,
            )
        })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_verifyForDeploy<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    artifact: JByteArray<'caller>,
    candidate_out: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        if candidate_out.len(env)? != 1 {
            return Ok(INVALID_ARGUMENT);
        }
        let artifact = bytes(env, &artifact)?;
        let mut candidate = 0u64;
        let result = unsafe {
            compukter_verify_for_deploy(
                handle as u64,
                artifact.as_ptr(),
                artifact.len(),
                &mut candidate,
            )
        };
        write_handle(env, &candidate_out, candidate)?;
        Ok(result as jint)
    })
}

macro_rules! path_output {
    ($name:ident, $call:path) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn $name<'caller>(
            mut env: EnvUnowned<'caller>,
            _class: JClass<'caller>,
            handle: jlong,
            path: JByteArray<'caller>,
            output: JByteArray<'caller>,
            written: JLongArray<'caller>,
        ) -> jint {
            status(&mut env, |env| {
                let path = bytes(env, &path)?;
                output_call(env, &output, &written, |out, capacity, count| unsafe {
                    $call(
                        handle as u64,
                        path.as_ptr(),
                        path.len(),
                        out,
                        capacity,
                        count,
                    )
                })
            })
        }
    };
}

path_output!(
    Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_executableRevision,
    compukter_executable_revision
);
path_output!(
    Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_filesystemStat,
    compukter_filesystem_stat
);

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_deploy<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    candidate_handle: jlong,
    path: JByteArray<'caller>,
    expected_kind: jint,
    expected_generation: jlong,
    output: JByteArray<'caller>,
    written: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let path = bytes(env, &path)?;
        output_call(env, &output, &written, |out, capacity, count| unsafe {
            compukter_deploy(
                handle as u64,
                candidate_handle as u64,
                path.as_ptr(),
                path.len(),
                expected_kind as u32,
                expected_generation as u64,
                out,
                capacity,
                count,
            )
        })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_submitCanonicalLine<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    line: JCharArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let line = chars(env, &line)?;
        Ok(
            unsafe { compukter_submit_canonical_line(handle as u64, line.as_ptr(), line.len()) }
                as jint,
        )
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_filesystemList<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    path: JByteArray<'caller>,
    start_after: JByteArray<'caller>,
    maximum_entries: jint,
    output: JByteArray<'caller>,
    written: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let path = bytes(env, &path)?;
        let start_after = bytes(env, &start_after)?;
        output_call(env, &output, &written, |out, capacity, count| unsafe {
            compukter_filesystem_list(
                handle as u64,
                path.as_ptr(),
                path.len(),
                start_after.as_ptr(),
                start_after.len(),
                maximum_entries as u32,
                out,
                capacity,
                count,
            )
        })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_filesystemRead<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    path: JByteArray<'caller>,
    offset: jlong,
    maximum_bytes: jint,
    expected_generation: jlong,
    output: JByteArray<'caller>,
    written: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let path = bytes(env, &path)?;
        output_call(env, &output, &written, |out, capacity, count| unsafe {
            compukter_filesystem_read(
                handle as u64,
                path.as_ptr(),
                path.len(),
                offset as u64,
                maximum_bytes as u32,
                expected_generation as u64,
                out,
                capacity,
                count,
            )
        })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_advance<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    guest_budget: jint,
    maintenance_budget: jint,
    host_request_budget: jint,
    output: JByteArray<'caller>,
    written: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        output_call(env, &output, &written, |out, capacity, count| unsafe {
            compukter_advance(
                handle as u64,
                guest_budget as u32,
                maintenance_budget as u32,
                host_request_budget as u32,
                out,
                capacity,
                count,
            )
        })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_compilationRequestSize<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    token: jlong,
    required_out: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        if required_out.len(env)? != 1 {
            return Ok(INVALID_ARGUMENT);
        }
        let mut required = 0usize;
        let result = unsafe {
            compukter_compilation_request_size(handle as u64, token as u64, &mut required)
        };
        write_long(env, &required_out, required)?;
        Ok(result as jint)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_compilationRequestCopy<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    token: jlong,
    output: JByteArray<'caller>,
    written: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        output_call(env, &output, &written, |out, capacity, count| unsafe {
            compukter_compilation_request_copy(handle as u64, token as u64, out, capacity, count)
        })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_compilationComplete<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    token: jlong,
    kind: jint,
    payload: JByteArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let payload = bytes(env, &payload)?;
        Ok(unsafe {
            compukter_compilation_complete(
                handle as u64,
                token as u64,
                kind as u32,
                payload.as_ptr(),
                payload.len(),
            )
        } as jint)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_resumeString<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    task_id: jint,
    request_id: jlong,
    value: JCharArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let value = chars(env, &value)?;
        Ok(unsafe {
            compukter_resume_string(
                handle as u64,
                task_id as u32,
                request_id as u64,
                value.as_ptr(),
                value.len(),
            )
        } as jint)
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_terminalChangesSince<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    revision: jlong,
    output: JByteArray<'caller>,
    written: JLongArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        output_call(env, &output, &written, |out, capacity, count| unsafe {
            compukter_terminal_changes_since(handle as u64, revision as u64, out, capacity, count)
        })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ru_lazyhat_compukters_lang_runtime_vm_JniNative_terminalText<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
    code_points: JIntArray<'caller>,
) -> jint {
    status(&mut env, |env| {
        let code_points = ints(env, &code_points)?;
        Ok(unsafe {
            compukter_terminal_text(handle as u64, code_points.as_ptr(), code_points.len())
        } as jint)
    })
}
