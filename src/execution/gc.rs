use super::{
    error::VmFault,
    external_roots::ExternalRootTable,
    heap::Heap,
    heap_ops::load_value,
    image::ExecutionImage,
    layout::{RuntimeTypeLayout, ValueWidth},
    machine::Frame,
    value::{Ref32, RegisterValue, RuntimeValue},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CollectorPhase {
    Idle,
    Roots,
    Mark,
    Sweep,
}

pub(super) struct RootSet<'a> {
    pub static_slots: &'a [RuntimeValue],
    pub frames: &'a [Frame],
    pub registers: &'a [RegisterValue],
    pub frame_depth: usize,
    pub external: &'a ExternalRootTable,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CollectorAction {
    Root,
    Dequeue(u32),
    Edge,
    Leaf(u32),
    Sweep(u32),
    Transition,
}

#[derive(Clone, Copy, Debug)]
enum Scan {
    Object {
        reference: Ref32,
        next: usize,
    },
    ReferenceArray {
        reference: Ref32,
        next: u32,
        length: u32,
    },
}

pub(super) struct Collector {
    phase: CollectorPhase,
    epoch: u32,
    external_root: usize,
    static_field: usize,
    frame: usize,
    register: usize,
    gray_head: Option<u32>,
    gray_tail: Option<u32>,
    scan: Option<Scan>,
    sweep_offset: u32,
    #[cfg(test)]
    last_action: Option<CollectorAction>,
}

impl Collector {
    pub(super) const fn new() -> Self {
        Self {
            phase: CollectorPhase::Idle,
            epoch: 0,
            external_root: 0,
            static_field: 0,
            frame: 0,
            register: 0,
            gray_head: None,
            gray_tail: None,
            scan: None,
            sweep_offset: 0,
            #[cfg(test)]
            last_action: None,
        }
    }

    pub(super) fn start(&mut self) {
        debug_assert_eq!(self.phase, CollectorPhase::Idle);
        self.epoch = if self.epoch == 1 { 2 } else { 1 };
        self.phase = CollectorPhase::Roots;
        self.external_root = 0;
        self.static_field = 0;
        self.frame = 0;
        self.register = 0;
        self.gray_head = None;
        self.gray_tail = None;
        self.scan = None;
        self.sweep_offset = 0;
        #[cfg(test)]
        {
            self.last_action = None;
        }
    }

    pub(super) const fn phase(&self) -> CollectorPhase {
        self.phase
    }

    pub(super) const fn is_active(&self) -> bool {
        !matches!(self.phase, CollectorPhase::Idle)
    }

    pub(super) fn step(
        &mut self,
        heap: &mut Heap,
        image: &ExecutionImage,
        roots: RootSet<'_>,
    ) -> Result<u32, VmFault> {
        #[cfg(test)]
        {
            self.last_action = None;
        }
        match self.phase {
            CollectorPhase::Idle => Ok(0),
            CollectorPhase::Roots => {
                if let Some(root) = self.next_root(
                    image,
                    roots.static_slots,
                    roots.frames,
                    roots.registers,
                    roots.frame_depth,
                    roots.external,
                )? {
                    self.enqueue_value(heap, root)?;
                    #[cfg(test)]
                    {
                        self.last_action = Some(CollectorAction::Root);
                    }
                } else {
                    self.phase = CollectorPhase::Mark;
                    #[cfg(test)]
                    {
                        self.last_action = Some(CollectorAction::Transition);
                    }
                }
                Ok(1)
            }
            CollectorPhase::Mark => {
                self.mark_step(heap, image)?;
                Ok(1)
            }
            CollectorPhase::Sweep => {
                if self.sweep_offset == heap.arena_bytes() {
                    self.phase = CollectorPhase::Idle;
                    #[cfg(test)]
                    {
                        self.last_action = Some(CollectorAction::Transition);
                    }
                } else {
                    #[cfg(test)]
                    {
                        self.last_action = Some(CollectorAction::Sweep(self.sweep_offset));
                    }
                    self.sweep_offset = heap.sweep_block(self.sweep_offset, self.epoch)?;
                }
                Ok(1)
            }
        }
    }

    fn next_root(
        &mut self,
        image: &ExecutionImage,
        static_slots: &[RuntimeValue],
        frames: &[Frame],
        registers: &[RegisterValue],
        frame_depth: usize,
        external_roots: &ExternalRootTable,
    ) -> Result<Option<RuntimeValue>, VmFault> {
        while self.external_root < external_roots.len() {
            let index = self.external_root;
            self.external_root += 1;
            if let Some(reference) = external_roots.root(index) {
                return Ok(Some(RuntimeValue::Reference(reference)));
            }
        }
        while let Some(field) = image.fields().get(self.static_field) {
            self.static_field += 1;
            if field.value_type.kind == 7 && field.static_slot.is_some() {
                let slot = field.static_slot.ok_or(VmFault::InvalidStoragePlan)?;
                return static_slots
                    .get(slot as usize)
                    .copied()
                    .map(Some)
                    .ok_or(VmFault::InvalidStoragePlan);
            }
        }
        while self.frame < frame_depth {
            let frame = frames.get(self.frame).ok_or(VmFault::CorruptLifecycle)?;
            let function = image
                .function(frame.function)
                .ok_or(VmFault::CorruptLifecycle)?;
            while self.register < function.register_count {
                let register = self.register;
                self.register += 1;
                if function
                    .registers
                    .get(register)
                    .is_some_and(|ty| ty.kind == 7)
                {
                    let index = self
                        .frame
                        .checked_mul(image.registers_per_frame())
                        .and_then(|base| base.checked_add(register))
                        .ok_or(VmFault::InvalidStoragePlan)?;
                    return Ok(Some(match registers.get(index) {
                        Some(RegisterValue::Initialized(value)) => *value,
                        Some(RegisterValue::Uninitialized) => RuntimeValue::Null,
                        None => return Err(VmFault::InvalidStoragePlan),
                    }));
                }
            }
            self.frame += 1;
            self.register = 0;
        }
        Ok(None)
    }

    fn enqueue_value(&mut self, heap: &mut Heap, value: RuntimeValue) -> Result<(), VmFault> {
        if let RuntimeValue::Reference(reference) = value {
            heap.enqueue_gray(
                reference,
                self.epoch,
                &mut self.gray_head,
                &mut self.gray_tail,
            )?;
        }
        Ok(())
    }

    fn mark_step(&mut self, heap: &mut Heap, image: &ExecutionImage) -> Result<(), VmFault> {
        if let Some(scan) = self.scan {
            match scan {
                Scan::Object { reference, next } => {
                    let ty = image
                        .type_key(heap.managed_type(reference)? as usize)
                        .ok_or(VmFault::InvalidResolvedId)?;
                    let RuntimeTypeLayout::Object(layout) =
                        image.type_layout(ty).ok_or(VmFault::InvalidResolvedId)?
                    else {
                        return Err(VmFault::CorruptHeap);
                    };
                    let offset = *layout
                        .reference_offsets
                        .get(next)
                        .ok_or(VmFault::CorruptLifecycle)?;
                    let value = load_value(heap, reference, offset, ValueWidth::Ref)?;
                    self.enqueue_value(heap, value)?;
                    #[cfg(test)]
                    {
                        self.last_action = Some(CollectorAction::Edge);
                    }
                    self.scan =
                        (next + 1 < layout.reference_offsets.len()).then_some(Scan::Object {
                            reference,
                            next: next + 1,
                        });
                }
                Scan::ReferenceArray {
                    reference,
                    next,
                    length,
                } => {
                    let offset = 8_u32
                        .checked_add(next.checked_mul(4).ok_or(VmFault::CorruptHeap)?)
                        .ok_or(VmFault::CorruptHeap)?;
                    let value = load_value(heap, reference, offset, ValueWidth::Ref)?;
                    self.enqueue_value(heap, value)?;
                    #[cfg(test)]
                    {
                        self.last_action = Some(CollectorAction::Edge);
                    }
                    self.scan = (next + 1 < length).then_some(Scan::ReferenceArray {
                        reference,
                        next: next + 1,
                        length,
                    });
                }
            }
            return Ok(());
        }

        let Some((reference, type_id)) =
            heap.dequeue_gray(&mut self.gray_head, &mut self.gray_tail)?
        else {
            self.phase = CollectorPhase::Sweep;
            self.sweep_offset = 0;
            #[cfg(test)]
            {
                self.last_action = Some(CollectorAction::Transition);
            }
            return Ok(());
        };
        #[cfg(test)]
        {
            self.last_action = Some(CollectorAction::Dequeue(reference.payload()));
        }
        let ty = image
            .type_key(type_id as usize)
            .ok_or(VmFault::InvalidResolvedId)?;
        match image.type_layout(ty).ok_or(VmFault::InvalidResolvedId)? {
            RuntimeTypeLayout::Object(layout) if !layout.reference_offsets.is_empty() => {
                self.scan = Some(Scan::Object { reference, next: 0 });
            }
            RuntimeTypeLayout::Array {
                element: ValueWidth::Ref,
            } => {
                let bytes = heap.read_payload(reference, 0, 4)?;
                let length = u32::from_le_bytes(bytes[..4].try_into().unwrap());
                if length != 0 {
                    self.scan = Some(Scan::ReferenceArray {
                        reference,
                        next: 0,
                        length,
                    });
                }
            }
            RuntimeTypeLayout::Object(_)
            | RuntimeTypeLayout::Array { .. }
            | RuntimeTypeLayout::NonHeap => {
                #[cfg(test)]
                {
                    self.last_action = Some(CollectorAction::Leaf(reference.payload()));
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) const fn test_epoch(&self) -> u32 {
        self.epoch
    }

    #[cfg(test)]
    pub(super) const fn test_last_action(&self) -> Option<CollectorAction> {
        self.last_action
    }
}
