/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use super::*;
use crate::execution::{numeric, prepared::*};
use cranelift_codegen::{
    ir::{
        condcodes::{FloatCC, IntCC},
        types, AbiParam, Endianness, InstBuilder, MemFlagsData, Type, Value,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{
    BranchProtection, JITBuilder, JITMemoryKind, JITMemoryProvider, JITModule, SystemMemoryProvider,
};
use cranelift_module::{default_libcall_names, Linkage, Module, ModuleResult};
use std::{io, sync::Mutex};

pub(super) struct Plan {
    ops: Vec<Operation>,
    copies: Vec<Copy>,
    required_bytes: u32,
    #[cfg(test)]
    writes: Vec<Vec<(u32, u32)>>,
}

pub(super) fn numeric_type(opcode: u32) -> Option<Type> {
    let form = match opcode {
        1..=18 | 21..=52 => (opcode - 1) % 4,
        53 | 57 | 61 | 65 | 69 | 73 | 105 | 109 | 121 | 125 | 129 | 133 | 137 | 141 => 0,
        54 | 58 | 62 | 66 | 70 | 74 | 106 | 110 | 122 | 126 | 130 | 134 | 138 | 142 => 1,
        _ => return None,
    };
    Some([types::I32, types::I64, types::F32, types::F64][form as usize])
}

impl Plan {
    pub(super) fn read(code: &PreparedCode, start: usize, length: usize) -> Option<Self> {
        let mut result = Self {
            ops: Vec::new(),
            copies: Vec::new(),
            required_bytes: 0,
            #[cfg(test)]
            writes: Vec::new(),
        };
        for index in 0..length.min(MAX_OPS) {
            let mut op = *code.operation(start.checked_add(index)?)?;
            let mut access = Vec::new();
            let mut writes = Vec::new();
            if let Some(ty) = numeric_type(op.opcode) {
                access.push((op.lhs, ty.bytes()));
                if !(21..=28).contains(&op.opcode) {
                    let width = if matches!(op.opcode, 134 | 138 | 142) {
                        4
                    } else {
                        ty.bytes()
                    };
                    access.push((op.rhs, width));
                }
                let width = if (29..=74).contains(&op.opcode) {
                    4
                } else {
                    ty.bytes()
                };
                writes.push((op.dst, width));
            } else {
                match op.opcode {
                    COPY32 | COPY_BOOL | COPY_CHAR => {
                        access.push((op.lhs, 4));
                        writes.push((op.dst, 4));
                    }
                    COPY64 => {
                        access.push((op.lhs, 8));
                        writes.push((op.dst, 8));
                    }
                    IMMEDIATE32 => writes.push((op.dst, 4)),
                    IMMEDIATE64 => writes.push((op.dst, 8)),
                    COPIES => {
                        let copies = code.copies.get(op.dst as usize..op.lhs as usize)?;
                        if result.copies.len().checked_add(copies.len())? > MAX_OPS {
                            break;
                        }
                        op.dst = result.copies.len() as u32;
                        for copy in copies {
                            if !matches!(copy.width, 4 | 8) {
                                return None;
                            }
                            access.push((copy.src, copy.width));
                            writes.push((copy.dst, copy.width));
                            result.copies.push(Copy {
                                dst: copy.dst,
                                src: copy.src,
                                width: copy.width,
                            });
                        }
                        op.lhs = result.copies.len() as u32;
                    }
                    BRANCH => access.push((op.dst, 4)),
                    JUMP | NOP => {}
                    _ => break,
                }
            }
            for &(offset, width) in access.iter().chain(writes.iter()) {
                let end = offset.checked_add(width)?;
                if end > i32::MAX as u32 {
                    return None;
                }
                result.required_bytes = result.required_bytes.max(end);
            }
            #[cfg(test)]
            result.writes.push(writes);
            result.ops.push(op);
            if matches!(op.opcode, JUMP | BRANCH) {
                break;
            }
        }
        (result.ops.len() >= 4).then_some(result)
    }
}

// Owns mappings on both successful and failed compilation paths. Mutex provides
// Sync without asserting JITModule is Sync; no module access occurs after publication.
struct ModuleOwner(Mutex<Option<JITModule>>);
impl Drop for ModuleOwner {
    fn drop(&mut self) {
        if let Some(module) = self
            .0
            .get_mut()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
        {
            // SAFETY: the final CompiledRun owner is being destroyed. Code pointers
            // stay private and calls borrow that owner, so no call can still execute.
            unsafe {
                module.free_memory();
            }
        }
    }
}

struct LimitedMemory {
    inner: SystemMemoryProvider,
    bytes: Arc<AtomicUsize>,
}
impl JITMemoryProvider for LimitedMemory {
    fn allocate(&mut self, size: usize, align: u64, kind: JITMemoryKind) -> io::Result<*mut u8> {
        // Conservative page-rounded upper bound, also including readonly/data requests.
        // Query the host granularity rather than assuming Linux's 4 KiB page size.
        let page = region::page::size();
        if align == 0 || !align.is_power_of_two() || align > page as u64 {
            return Err(io::Error::other("unsupported JIT allocation alignment"));
        }
        let charged = size
            .checked_add(page - 1)
            .map(|n| n / page * page)
            .ok_or_else(|| io::Error::other("JIT allocation overflow"))?;
        let total = self
            .bytes
            .load(Ordering::Relaxed)
            .checked_add(charged)
            .filter(|n| *n <= MODULE_BYTES)
            .ok_or_else(|| io::Error::other("JIT module memory cap"))?;
        let pointer = self.inner.allocate(size, align, kind)?;
        self.bytes.store(total, Ordering::Relaxed);
        Ok(pointer)
    }
    unsafe fn free_memory(&mut self) {
        // SAFETY: delegated under ModuleOwner's final-owner contract.
        unsafe {
            self.inner.free_memory();
        }
        self.bytes.store(0, Ordering::Relaxed);
    }
    fn finalize(&mut self, protection: BranchProtection) -> ModuleResult<()> {
        self.inner.finalize(protection)
    }
}

pub(super) struct CompiledRun {
    _owner: ModuleOwner,
    _reservation: Reservation,
    entry: unsafe extern "C" fn(*mut u8) -> u64,
    pub len: usize,
    pub required_bytes: u32,
    pub code_bytes: usize,
    pub mapped_bytes: usize,
    #[cfg(test)]
    writes: Vec<Vec<(u32, u32)>>,
    #[cfg(test)]
    pub(super) mapped_counter: Arc<AtomicUsize>,
}
impl std::fmt::Debug for CompiledRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledRun")
            .field("len", &self.len)
            .field("code_bytes", &self.code_bytes)
            .finish()
    }
}

impl CompiledRun {
    pub(super) fn execute(&self, frame: &mut PreparedFrame<'_>, instruction: usize) -> SimpleRun {
        assert!(frame.jit_bytes().len() >= self.required_bytes as usize);
        // SAFETY: Plan bounds every operand by required_bytes and lower emits only
        // bounded frame operations. No GC, allocation, call-out or frame resize occurs.
        // The private function pointer has exactly this signature and its owner is borrowed.
        let status = unsafe { (self.entry)(frame.jit_bytes().as_mut_ptr()) };
        let detail = status as u32 as usize;
        let reason = status >> 32;
        let (done, pc, end) = match reason {
            0 => (self.len, instruction + self.len, RunEnd::Boundary),
            1 => (self.len, instruction + self.len - 1, RunEnd::Branch(detail)),
            2 => (
                detail + 1,
                instruction + detail,
                RunEnd::Failure(InstructionFailure::Fault(VmFault::InvalidValueType)),
            ),
            3 => (
                detail + 1,
                instruction + detail,
                RunEnd::Failure(InstructionFailure::Trap(GuestTrap::DivisionByZero)),
            ),
            _ => unreachable!("private JIT return status"),
        };
        #[cfg(test)]
        for ranges in self
            .writes
            .iter()
            .take(if reason >= 2 { done - 1 } else { done })
        {
            frame.jit_mark_writes(ranges);
        }
        SimpleRun {
            instruction: pc,
            attempts: done as u64,
            end,
        }
    }
}

fn flags() -> MemFlagsData {
    MemFlagsData::new().with_endianness(Endianness::Little)
}
fn load(b: &mut FunctionBuilder<'_>, base: Value, ty: Type, offset: u32) -> Value {
    b.ins().load(ty, flags(), base, offset as i32)
}
fn store(b: &mut FunctionBuilder<'_>, base: Value, offset: u32, value: Value) {
    b.ins().store(flags(), value, base, offset as i32);
}
fn guard(b: &mut FunctionBuilder<'_>, bad: Value, reason: u64, pc: usize) {
    let fail = b.create_block();
    let next = b.create_block();
    b.ins().brif(bad, fail, &[], next, &[]);
    b.switch_to_block(fail);
    let status = b
        .ins()
        .iconst(types::I64, ((reason << 32) | pc as u64) as i64);
    b.ins().return_(&[status]);
    b.switch_to_block(next);
}
fn canonical(b: &mut FunctionBuilder<'_>, value: Value, ty: Type) -> Value {
    let nan = b.ins().fcmp(FloatCC::Unordered, value, value);
    let bits = if ty == types::F32 {
        b.ins()
            .iconst(types::I32, numeric::CANONICAL_F32_NAN as i64)
    } else {
        b.ins()
            .iconst(types::I64, numeric::CANONICAL_F64_NAN as i64)
    };
    let replacement = b.ins().bitcast(ty, MemFlagsData::new(), bits);
    b.ins().select(nan, replacement, value)
}

fn number(b: &mut FunctionBuilder<'_>, base: Value, op: Operation, pc: usize, ty: Type) -> Value {
    let lhs = load(b, base, ty, op.lhs);
    if (21..=28).contains(&op.opcode) {
        if op.opcode >= 25 {
            return lhs;
        }
        return if ty.is_int() {
            b.ins().ineg(lhs)
        } else {
            let result = b.ins().fneg(lhs);
            canonical(b, result, ty)
        };
    }
    let rhs_ty = if matches!(op.opcode, 134 | 138 | 142) {
        types::I32
    } else {
        ty
    };
    let rhs = load(b, base, rhs_ty, op.rhs);
    if (29..=74).contains(&op.opcode) {
        let group = if op.opcode <= 52 {
            (op.opcode - 29) / 4
        } else {
            (op.opcode - 53) / 4
        };
        let value = if ty.is_float() {
            let condition = [
                FloatCC::Equal,
                FloatCC::NotEqual,
                FloatCC::LessThan,
                FloatCC::LessThanOrEqual,
                FloatCC::GreaterThan,
                FloatCC::GreaterThanOrEqual,
            ][group as usize];
            b.ins().fcmp(condition, lhs, rhs)
        } else {
            let conditions = if op.opcode >= 53 {
                [
                    IntCC::Equal,
                    IntCC::NotEqual,
                    IntCC::UnsignedLessThan,
                    IntCC::UnsignedLessThanOrEqual,
                    IntCC::UnsignedGreaterThan,
                    IntCC::UnsignedGreaterThanOrEqual,
                ]
            } else {
                [
                    IntCC::Equal,
                    IntCC::NotEqual,
                    IntCC::SignedLessThan,
                    IntCC::SignedLessThanOrEqual,
                    IntCC::SignedGreaterThan,
                    IntCC::SignedGreaterThanOrEqual,
                ]
            };
            b.ins().icmp(conditions[group as usize], lhs, rhs)
        };
        return b.ins().uextend(types::I32, value);
    }
    if ty.is_float() {
        let result = match op.opcode {
            3 | 4 => b.ins().fadd(lhs, rhs),
            7 | 8 => b.ins().fsub(lhs, rhs),
            11 | 12 => b.ins().fmul(lhs, rhs),
            15 | 16 => b.ins().fdiv(lhs, rhs),
            _ => unreachable!(),
        };
        return canonical(b, result, ty);
    }
    match op.opcode {
        1 | 2 => b.ins().iadd(lhs, rhs),
        5 | 6 => b.ins().isub(lhs, rhs),
        9 | 10 => b.ins().imul(lhs, rhs),
        13 | 14 | 17 | 18 | 105 | 106 | 109 | 110 => {
            let zero = b.ins().icmp_imm_s(IntCC::Equal, rhs, 0);
            guard(b, zero, 3, pc);
            if op.opcode >= 105 {
                return if op.opcode <= 106 {
                    b.ins().udiv(lhs, rhs)
                } else {
                    b.ins().urem(lhs, rhs)
                };
            }
            // Kotlin MIN / -1 wraps; Cranelift sdiv would trap. Substitute a safe
            // divisor before the instruction, then select the defined result.
            let minimum = if ty == types::I32 {
                i32::MIN as i64
            } else {
                i64::MIN
            };
            let min = b.ins().icmp_imm_s(IntCC::Equal, lhs, minimum);
            let minus_one = b.ins().icmp_imm_s(IntCC::Equal, rhs, -1);
            let overflow = b.ins().band(min, minus_one);
            let one = b.ins().iconst(ty, 1);
            let divisor = b.ins().select(overflow, one, rhs);
            if op.opcode <= 14 {
                b.ins().sdiv(lhs, divisor)
            } else {
                b.ins().srem(lhs, divisor)
            }
        }
        121 | 122 => b.ins().band(lhs, rhs),
        125 | 126 => b.ins().bor(lhs, rhs),
        129 | 130 => b.ins().bxor(lhs, rhs),
        133 | 134 | 137 | 138 | 141 | 142 => {
            let count = b
                .ins()
                .band_imm_s(rhs, if ty == types::I32 { 31 } else { 63 });
            match op.opcode {
                133 | 134 => b.ins().ishl(lhs, count),
                137 | 138 => b.ins().sshr(lhs, count),
                _ => b.ins().ushr(lhs, count),
            }
        }
        _ => unreachable!("validated numeric opcode"),
    }
}

pub(super) fn compile(plan: Plan, reservation: Reservation) -> Result<CompiledRun, String> {
    let mut settings = settings::builder();
    settings
        .set("opt_level", "speed")
        .map_err(|e| e.to_string())?;
    settings
        .set("use_colocated_libcalls", "false")
        .map_err(|e| e.to_string())?;
    settings.set("is_pic", "false").map_err(|e| e.to_string())?;
    let isa = cranelift_native::builder()
        .map_err(str::to_owned)?
        .finish(settings::Flags::new(settings))
        .map_err(|e| e.to_string())?;
    let bytes = Arc::new(AtomicUsize::new(0));
    let mut builder = JITBuilder::with_isa(isa, default_libcall_names());
    builder.memory_provider(Box::new(LimitedMemory {
        inner: SystemMemoryProvider::new(),
        bytes: bytes.clone(),
    }));
    let owner = ModuleOwner(Mutex::new(Some(JITModule::new(builder))));
    let mut locked = owner.0.lock().unwrap();
    let module = locked.as_mut().unwrap();
    let mut ctx = module.make_context();
    ctx.func
        .signature
        .params
        .push(AbiParam::new(module.target_config().pointer_type()));
    ctx.func.signature.returns.push(AbiParam::new(types::I64));
    let id = module
        .declare_function("run", Linkage::Local, &ctx.func.signature)
        .map_err(|e| e.to_string())?;
    let mut frontend = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut frontend);
        let block = b.create_block();
        b.switch_to_block(block);
        b.append_block_params_for_function_params(block);
        let base = b.block_params(block)[0];
        let mut terminated = false;
        for (pc, &op) in plan.ops.iter().enumerate() {
            if let Some(ty) = numeric_type(op.opcode) {
                let value = number(&mut b, base, op, pc, ty);
                store(&mut b, base, op.dst, value);
                continue;
            }
            match op.opcode {
                NOP => {}
                COPY32 | COPY64 | COPY_BOOL | COPY_CHAR => {
                    let ty = if op.opcode == COPY64 {
                        types::I64
                    } else {
                        types::I32
                    };
                    let value = load(&mut b, base, ty, op.lhs);
                    if matches!(op.opcode, COPY_BOOL | COPY_CHAR) {
                        let bad = b.ins().icmp_imm_s(
                            IntCC::UnsignedGreaterThan,
                            value,
                            if op.opcode == COPY_BOOL { 1 } else { 65535 },
                        );
                        guard(&mut b, bad, 2, pc);
                    }
                    store(&mut b, base, op.dst, value);
                }
                IMMEDIATE32 | IMMEDIATE64 => {
                    let (ty, bits) = if op.opcode == IMMEDIATE32 {
                        (types::I32, op.lhs as u64)
                    } else {
                        (types::I64, op.lhs as u64 | (op.rhs as u64) << 32)
                    };
                    let value = b.ins().iconst(ty, bits as i64);
                    store(&mut b, base, op.dst, value);
                }
                COPIES => {
                    for copy in &plan.copies[op.dst as usize..op.lhs as usize] {
                        let value = load(
                            &mut b,
                            base,
                            if copy.width == 4 {
                                types::I32
                            } else {
                                types::I64
                            },
                            copy.src,
                        );
                        store(&mut b, base, copy.dst, value);
                    }
                }
                JUMP | BRANCH => {
                    let target = if op.opcode == JUMP {
                        b.ins().iconst(types::I64, op.dst as i64)
                    } else {
                        let condition = load(&mut b, base, types::I32, op.dst);
                        let bad = b.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, condition, 1);
                        guard(&mut b, bad, 2, pc);
                        let yes = b.ins().iconst(types::I64, op.lhs as i64);
                        let no = b.ins().iconst(types::I64, op.rhs as i64);
                        b.ins().select(condition, yes, no)
                    };
                    let status = b.ins().bor_imm_s(target, 1_i64 << 32);
                    b.ins().return_(&[status]);
                    terminated = true;
                }
                _ => unreachable!("validated frame opcode"),
            }
        }
        if !terminated {
            let status = b.ins().iconst(types::I64, 0);
            b.ins().return_(&[status]);
        }
        b.seal_all_blocks();
        b.finalize(module.target_config());
    }
    module
        .define_function(id, &mut ctx)
        .map_err(|e| e.to_string())?;
    let code_bytes = ctx
        .compiled_code()
        .ok_or("missing compiled code")?
        .code_buffer()
        .len();
    module.finalize_definitions().map_err(|e| e.to_string())?;
    let pointer = module.get_finalized_function(id);
    // SAFETY: lowering declared exactly fn(frame_pointer) -> u64 with native calling convention.
    let entry =
        unsafe { std::mem::transmute::<*const u8, unsafe extern "C" fn(*mut u8) -> u64>(pointer) };
    drop(locked);
    Ok(CompiledRun {
        _owner: owner,
        _reservation: reservation,
        entry,
        len: plan.ops.len(),
        required_bytes: plan.required_bytes,
        code_bytes,
        mapped_bytes: bytes.load(Ordering::Relaxed),
        #[cfg(test)]
        writes: plan.writes,
        #[cfg(test)]
        mapped_counter: bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_provider_rejects_over_cap_and_frees_all_mappings() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut memory = LimitedMemory {
            inner: SystemMemoryProvider::new(),
            bytes: counter.clone(),
        };
        assert!(memory
            .allocate(MODULE_BYTES + 1, 16, JITMemoryKind::Executable)
            .is_err());
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        memory
            .allocate(MODULE_BYTES, 16, JITMemoryKind::Executable)
            .unwrap();
        assert!(memory.allocate(1, 16, JITMemoryKind::Executable).is_err());
        // SAFETY: no function pointers were published from this test provider.
        unsafe {
            memory.free_memory();
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }
}
