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

mod access;
#[cfg(feature = "jit-diagnostics")]
mod dump;
mod regions;
pub(super) mod slots;
pub(in crate::execution) use regions::{RegionRun, RunState};

pub(super) struct Plan {
    region: Vec<regions::RegionBlock>,
    ops: Vec<Operation>,
    copies: Vec<Copy>,
    required_bytes: u32,
    has_heap: bool,
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

fn heap_kind(opcode: u32) -> Option<u32> {
    // Only array loads use the NON_NULL flag; reject other unknown flag bits.
    match opcode {
        201..=207 => Some(opcode - FIELD_GET),
        209..=215 => Some(opcode - FIELD_SET),
        217..=223 => Some(opcode - ARRAY_LOAD),
        225..=231 => Some(opcode - ARRAY_STORE),
        ARRAY_LENGTH => Some(1),
        flagged if (217..=223).contains(&(flagged ^ NON_NULL)) => {
            Some((flagged ^ NON_NULL) - ARRAY_LOAD)
        }
        _ => None,
    }
}

impl Plan {
    pub(super) fn read(code: &PreparedCode, start: usize, length: usize) -> Option<Self> {
        Self::prefix(code, start, length).filter(|p| p.ops.len() >= 4)
    }

    fn prefix(code: &PreparedCode, start: usize, length: usize) -> Option<Self> {
        let mut result = Self {
            region: Vec::new(),
            ops: Vec::new(),
            copies: Vec::new(),
            required_bytes: 0,
            has_heap: false,
            #[cfg(test)]
            writes: Vec::new(),
        };
        for index in 0..length.min(MAX_OPS) {
            let mut op = *code.operation(start.checked_add(index)?)?;
            if op.opcode == COPIES {
                let copies = code.copies.get(op.dst as usize..op.lhs as usize)?;
                if result.copies.len().checked_add(copies.len())? > MAX_OPS {
                    break;
                }
                op.dst = result.copies.len() as u32;
                for copy in copies {
                    if !matches!(copy.width, 4 | 8) {
                        return None;
                    }
                    result.copies.push(Copy {
                        dst: copy.dst,
                        src: copy.src,
                        width: copy.width,
                    });
                }
                op.lhs = result.copies.len() as u32;
            }
            let Some(accesses) = access::footprint(op, &result.copies) else {
                break;
            };
            result.has_heap |= heap_kind(op.opcode).is_some();
            for access in &accesses {
                let end = access.offset.checked_add(access.ty.bytes())?;
                if end > i32::MAX as u32 {
                    return None;
                }
                result.required_bytes = result.required_bytes.max(end);
            }
            #[cfg(test)]
            result.writes.push(
                accesses
                    .iter()
                    .filter(|a| a.write)
                    .map(|a| (a.offset, a.ty.bytes()))
                    .collect(),
            );
            result.ops.push(op);
            if matches!(op.opcode, JUMP | BRANCH) {
                break;
            }
        }
        (!result.ops.is_empty()).then_some(result)
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
    entry: unsafe extern "C" fn(*mut u8, *mut u8, *mut u8) -> u64,
    pub(super) has_heap: bool,
    region: bool,
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
    pub(super) fn is_region(&self) -> bool {
        self.region
    }

    fn invoke(
        &self,
        frame: &mut PreparedFrame<'_>,
        heap: Option<HeapAccess<'_>>,
        state: *mut u8,
    ) -> (u64, Option<InstructionFailure>) {
        if self.has_heap {
            let access = heap.expect("heap plan requires a scoped heap borrow");
            let mut context = HeapContext {
                frame: frame.jit_view(),
                image: access.image,
                heap: access.heap,
                failure: None,
            };
            let base = context.frame.as_mut_ptr();
            let opaque = (&mut context as *mut HeapContext<'_, '_>).cast::<u8>();
            // SAFETY: Plan bounds all frame operands. Heap callouts borrow the
            // invocation context synchronously, without allocation/GC/frame resize.
            // The module owner and all borrowed storage remain alive until return.
            let status = unsafe { (self.entry)(base, opaque, state) };
            (status, context.failure)
        } else {
            // SAFETY: this plan emits bounded frame operations and never reads the
            // unused null context. The private entry has exactly this signature.
            (
                unsafe {
                    (self.entry)(frame.jit_bytes().as_mut_ptr(), core::ptr::null_mut(), state)
                },
                None,
            )
        }
    }

    pub(super) fn execute(
        &self,
        frame: &mut PreparedFrame<'_>,
        instruction: usize,
        heap: Option<HeapAccess<'_>>,
    ) -> SimpleRun {
        assert!(frame.jit_bytes().len() >= self.required_bytes as usize);
        assert!(!self.region);
        let (status, failure) = self.invoke(frame, heap, core::ptr::null_mut());
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
            4 => (
                detail + 1,
                instruction + detail,
                RunEnd::Failure(failure.expect("checked callout failure")),
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

// Rust owns rich errors and all admission-specific heap state. Native code sees
// only an opaque invocation pointer and a success/failure bit, never a heap base.
struct HeapContext<'frame, 'heap> {
    frame: crate::execution::frame::JitFrame<'frame>,
    image: &'heap crate::execution::image::ExecutionImage,
    heap: &'heap mut crate::execution::heap::Heap,
    failure: Option<InstructionFailure>,
}

unsafe extern "C" fn checked_heap<const OPCODE: u32>(
    context: *mut u8,
    dst: u32,
    lhs: u32,
    rhs: u32,
) -> u32 {
    // SAFETY: only private generated calls use this symbol; execute supplies a
    // live, exclusive context for precisely the synchronous native invocation.
    let context = unsafe { &mut *context.cast::<HeapContext<'_, '_>>() };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::execution::machine::prepared_execution::execute_heap_opcode::<OPCODE>(
            Operation {
                opcode: OPCODE,
                dst,
                lhs,
                rhs,
            },
            &mut context.frame.reborrow(),
            context.image,
            context.heap,
        )
    }))
    .unwrap_or(Err(InstructionFailure::Fault(VmFault::CorruptHeap)));
    match result {
        Ok(()) => 0,
        Err(failure) => {
            context.failure = Some(failure);
            1
        }
    }
}

fn heap_helper(opcode: u32) -> *const u8 {
    match opcode {
        201 => checked_heap::<201> as *const u8,
        202 => checked_heap::<202> as *const u8,
        203 => checked_heap::<203> as *const u8,
        204 => checked_heap::<204> as *const u8,
        205 => checked_heap::<205> as *const u8,
        206 => checked_heap::<206> as *const u8,
        207 => checked_heap::<207> as *const u8,
        209 => checked_heap::<209> as *const u8,
        210 => checked_heap::<210> as *const u8,
        211 => checked_heap::<211> as *const u8,
        212 => checked_heap::<212> as *const u8,
        213 => checked_heap::<213> as *const u8,
        214 => checked_heap::<214> as *const u8,
        215 => checked_heap::<215> as *const u8,
        217 => checked_heap::<217> as *const u8,
        218 => checked_heap::<218> as *const u8,
        219 => checked_heap::<219> as *const u8,
        220 => checked_heap::<220> as *const u8,
        221 => checked_heap::<221> as *const u8,
        222 => checked_heap::<222> as *const u8,
        223 => checked_heap::<223> as *const u8,
        225 => checked_heap::<225> as *const u8,
        226 => checked_heap::<226> as *const u8,
        227 => checked_heap::<227> as *const u8,
        228 => checked_heap::<228> as *const u8,
        229 => checked_heap::<229> as *const u8,
        230 => checked_heap::<230> as *const u8,
        231 => checked_heap::<231> as *const u8,
        232 => checked_heap::<232> as *const u8,
        65753 => checked_heap::<65753> as *const u8,
        65754 => checked_heap::<65754> as *const u8,
        65755 => checked_heap::<65755> as *const u8,
        65756 => checked_heap::<65756> as *const u8,
        65757 => checked_heap::<65757> as *const u8,
        65758 => checked_heap::<65758> as *const u8,
        65759 => checked_heap::<65759> as *const u8,
        _ => unreachable!("validated heap opcode"),
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
fn guard(
    b: &mut FunctionBuilder<'_>,
    bad: Value,
    reason: u64,
    pc: usize,
    exit: Option<regions::Exit>,
) {
    let fail = b.create_block();
    let next = b.create_block();
    b.ins().brif(bad, fail, &[], next, &[]);
    b.switch_to_block(fail);
    let status = b
        .ins()
        .iconst(types::I64, ((reason << 32) | pc as u64) as i64);
    if let Some(exit) = exit {
        exit.return_status(b, status);
    } else {
        b.ins().return_(&[status]);
    }
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

fn number(
    b: &mut FunctionBuilder<'_>,
    base: Value,
    op: Operation,
    pc: usize,
    ty: Type,
    exit: Option<regions::Exit>,
    values: Option<&slots::Cache>,
) -> Value {
    let lhs = slots::read(b, base, values, ty, op.lhs);
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
    let rhs = slots::read(b, base, values, rhs_ty, op.rhs);
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
            guard(b, zero, 3, pc, exit);
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
    let helpers: std::collections::BTreeMap<_, _> = plan
        .ops
        .iter()
        .filter(|op| heap_kind(op.opcode).is_some())
        .map(|op| (op.opcode, format!("checked_heap_{}", op.opcode)))
        .collect();
    for (&opcode, name) in &helpers {
        builder.symbol(name, heap_helper(opcode));
    }
    builder.memory_provider(Box::new(LimitedMemory {
        inner: SystemMemoryProvider::new(),
        bytes: bytes.clone(),
    }));
    let owner = ModuleOwner(Mutex::new(Some(JITModule::new(builder))));
    let mut locked = owner.0.lock().unwrap();
    let module = locked.as_mut().unwrap();
    let pointer_type = module.target_config().pointer_type();
    let mut helper_signature = module.make_signature();
    helper_signature.params.push(AbiParam::new(pointer_type));
    helper_signature
        .params
        .extend([AbiParam::new(types::I32); 3]);
    helper_signature.returns.push(AbiParam::new(types::I32));
    let helpers = helpers
        .into_iter()
        .map(|(opcode, name)| {
            module
                .declare_function(&name, Linkage::Import, &helper_signature)
                .map(|id| (opcode, id))
                .map_err(|e| e.to_string())
        })
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
    let mut ctx = module.make_context();
    ctx.func
        .signature
        .params
        .push(AbiParam::new(module.target_config().pointer_type()));
    ctx.func.signature.params.push(AbiParam::new(pointer_type));
    ctx.func.signature.params.push(AbiParam::new(pointer_type));
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
        let opaque = b.block_params(block)[1];
        let state = b.block_params(block)[2];
        let values = slots::Cache::new(&plan, &mut b, base);
        let mut emitter = Emitter {
            base,
            opaque,
            plan: &plan,
            module,
            helpers: &helpers,
            values,
        };
        if plan.region.is_empty() {
            let mut terminated = false;
            for (pc, &op) in plan.ops.iter().enumerate() {
                terminated = emitter.operation(&mut b, op, pc, None);
                if terminated {
                    break;
                }
            }
            if !terminated {
                let status = b.ins().iconst(types::I64, 0);
                b.ins().return_(&[status]);
            }
        } else {
            regions::emit(&mut b, &mut emitter, state);
        }
        if let Some(values) = &emitter.values {
            values.finish(&mut b, base);
        }
        b.seal_all_blocks();
        b.finalize(module.target_config());
    }
    #[cfg(feature = "jit-diagnostics")]
    let capture = dump::Capture::begin(&plan, &mut ctx);
    let defined = module.define_function(id, &mut ctx);
    #[cfg(feature = "jit-diagnostics")]
    if let Some(capture) = capture {
        if let Err(error) = &defined {
            capture.error(&error.to_string());
        }
        capture.finish(&ctx);
    }
    defined.map_err(|e| e.to_string())?;
    let code_bytes = ctx
        .compiled_code()
        .ok_or("missing compiled code")?
        .code_buffer()
        .len();
    module.finalize_definitions().map_err(|e| e.to_string())?;
    let pointer = module.get_finalized_function(id);
    // SAFETY: lowering declared exactly fn(frame_pointer, opaque_context) -> u64 with native calling convention.
    let entry = unsafe {
        std::mem::transmute::<*const u8, unsafe extern "C" fn(*mut u8, *mut u8, *mut u8) -> u64>(
            pointer,
        )
    };
    drop(locked);
    Ok(CompiledRun {
        _owner: owner,
        _reservation: reservation,
        entry,
        has_heap: plan.has_heap,
        region: !plan.region.is_empty(),
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

struct Emitter<'a> {
    base: Value,
    opaque: Value,
    plan: &'a Plan,
    module: &'a mut JITModule,
    helpers: &'a std::collections::BTreeMap<u32, cranelift_module::FuncId>,
    values: Option<slots::Cache>,
}
impl Emitter<'_> {
    fn operation(
        &mut self,
        b: &mut FunctionBuilder<'_>,
        op: Operation,
        pc: usize,
        exit: Option<regions::Exit>,
    ) -> bool {
        let Self {
            base,
            opaque,
            plan,
            module,
            helpers,
            values,
        } = self;
        let (base, opaque) = (*base, *opaque);
        if let Some(ty) = numeric_type(op.opcode) {
            let value = number(b, base, op, pc, ty, exit, values.as_ref());
            slots::write(b, base, values.as_ref(), op.dst, value);
            return false;
        }
        if heap_kind(op.opcode).is_some() {
            let helper = module.declare_func_in_func(helpers[&op.opcode], b.func);
            let operands =
                [op.dst, op.lhs, op.rhs].map(|value| b.ins().iconst(types::I32, value as i64));
            let call = b
                .ins()
                .call(helper, &[opaque, operands[0], operands[1], operands[2]]);
            let failed = b.inst_results(call)[0];
            guard(b, failed, 4, pc, exit);
            return false;
        }
        match op.opcode {
            NOP => {}
            COPY32 | COPY64 | COPY_BOOL | COPY_CHAR => {
                let ty = if op.opcode == COPY64 {
                    types::I64
                } else {
                    types::I32
                };
                let value = slots::read(b, base, values.as_ref(), ty, op.lhs);
                if matches!(op.opcode, COPY_BOOL | COPY_CHAR) {
                    let bad = b.ins().icmp_imm_s(
                        IntCC::UnsignedGreaterThan,
                        value,
                        if op.opcode == COPY_BOOL { 1 } else { 65535 },
                    );
                    guard(b, bad, 2, pc, exit);
                }
                slots::write(b, base, values.as_ref(), op.dst, value);
            }
            IMMEDIATE32 | IMMEDIATE64 => {
                let (ty, bits) = if op.opcode == IMMEDIATE32 {
                    (types::I32, op.lhs as u64)
                } else {
                    (types::I64, op.lhs as u64 | (op.rhs as u64) << 32)
                };
                let value = b.ins().iconst(ty, bits as i64);
                slots::write(b, base, values.as_ref(), op.dst, value);
            }
            COPIES => {
                for copy in &plan.copies[op.dst as usize..op.lhs as usize] {
                    let value = slots::read(
                        b,
                        base,
                        values.as_ref(),
                        if copy.width == 4 {
                            types::I32
                        } else {
                            types::I64
                        },
                        copy.src,
                    );
                    slots::write(b, base, values.as_ref(), copy.dst, value);
                }
            }
            JUMP | BRANCH => {
                let target = if op.opcode == JUMP {
                    b.ins().iconst(types::I64, op.dst as i64)
                } else {
                    let condition = slots::read(b, base, values.as_ref(), types::I32, op.dst);
                    let bad = b.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, condition, 1);
                    guard(b, bad, 2, pc, exit);
                    let yes = b.ins().iconst(types::I64, op.lhs as i64);
                    let no = b.ins().iconst(types::I64, op.rhs as i64);
                    b.ins().select(condition, yes, no)
                };
                let status = b.ins().bor_imm_s(target, 1_i64 << 32);
                b.ins().return_(&[status]);
                return true;
            }
            _ => unreachable!("validated frame opcode"),
        }

        false
    }
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
