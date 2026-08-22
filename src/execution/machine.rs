use super::{
    error::{AdmissionError, RunError},
    image::{ExecutionImage, ResolvedValueType},
    value::{EntryArgument, RegisterValue, RuntimeValue},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lifecycle {
    Pristine,
    Runnable,
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    function: usize,
    block: usize,
    instruction: usize,
    caller_instruction: usize,
    destination: u16,
}

impl Frame {
    const EMPTY: Self = Self {
        function: usize::MAX,
        block: usize::MAX,
        instruction: 0,
        caller_instruction: 0,
        destination: u16::MAX,
    };
}

pub(super) struct Machine {
    image: ExecutionImage,
    lifecycle: Lifecycle,
    frames: Box<[Frame]>,
    registers: Box<[RegisterValue]>,
    frame_depth: usize,
}

impl Machine {
    pub(super) fn new(image: ExecutionImage) -> Result<Self, AdmissionError> {
        let frame_count = image.maximum_call_depth();
        let register_count = frame_count
            .checked_mul(image.registers_per_frame())
            .ok_or(AdmissionError::StoragePlanOverflow)?;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(frame_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        frames.resize(frame_count, Frame::EMPTY);
        let mut registers = Vec::new();
        registers
            .try_reserve_exact(register_count)
            .map_err(|_| AdmissionError::AllocationFailed)?;
        registers.resize(register_count, RegisterValue::Uninitialized);
        Ok(Self {
            image,
            lifecycle: Lifecycle::Pristine,
            frames: frames.into_boxed_slice(),
            registers: registers.into_boxed_slice(),
            frame_depth: 0,
        })
    }

    pub(super) fn start(&mut self, arguments: &[EntryArgument]) -> Result<(), RunError> {
        if self.lifecycle != Lifecycle::Pristine {
            return Err(RunError::AlreadyStarted);
        }
        let entry_index = self.image.entry_index();
        let entry = self
            .image
            .function(entry_index)
            .ok_or(RunError::NotRunnable)?;
        let supplied = u16::try_from(arguments.len()).unwrap_or(u16::MAX);
        if arguments.len() != entry.parameter_count {
            return Err(RunError::EntryArity {
                expected: entry.parameter_count as u16,
                supplied,
            });
        }
        for (parameter, (argument, expected)) in arguments
            .iter()
            .zip(&entry.registers[..entry.parameter_count])
            .enumerate()
        {
            self.validate_argument(parameter as u16, argument.0, *expected)?;
        }

        let entry = self
            .image
            .function(entry_index)
            .ok_or(RunError::NotRunnable)?;
        self.frames[0] = Frame {
            function: entry_index,
            block: entry.first_block,
            instruction: 0,
            caller_instruction: 0,
            destination: u16::MAX,
        };
        let width = self.image.registers_per_frame();
        self.registers[..width].fill(RegisterValue::Uninitialized);
        for (slot, argument) in self.registers.iter_mut().zip(arguments) {
            *slot = RegisterValue::Initialized(argument.0);
        }
        self.frame_depth = 1;
        self.lifecycle = Lifecycle::Runnable;
        Ok(())
    }

    fn validate_argument(
        &self,
        parameter: u16,
        value: RuntimeValue,
        expected: ResolvedValueType,
    ) -> Result<(), RunError> {
        let primitive_matches = matches!(
            (expected.kind, value),
            (1, RuntimeValue::I32(_))
                | (2, RuntimeValue::I64(_))
                | (3, RuntimeValue::F32(_))
                | (4, RuntimeValue::F64(_))
                | (5, RuntimeValue::Bool(_))
                | (6, RuntimeValue::Char(_))
        );
        if primitive_matches {
            return Ok(());
        }
        match value {
            RuntimeValue::Null if expected.kind == 7 && expected.nullable => Ok(()),
            RuntimeValue::Reference(value) if expected.kind == 7 => {
                if value.image != self.image.content_hash() {
                    return Err(RunError::ForeignReference { parameter });
                }
                let admitted = self
                    .image
                    .host_reference(value)
                    .ok_or(RunError::DeadReference { parameter })?;
                if !admitted.live || admitted.value.ty != value.ty {
                    return Err(RunError::DeadReference { parameter });
                }
                let expected_type = expected.nominal.ok_or(RunError::EntryType { parameter })?;
                if admitted.assignable_to.contains(&expected_type) {
                    Ok(())
                } else {
                    Err(RunError::EntryType { parameter })
                }
            }
            _ => Err(RunError::EntryType { parameter }),
        }
    }

    pub(super) fn frame_depth(&self) -> usize {
        self.frame_depth
    }

    #[cfg(test)]
    pub(super) fn test_register(&self, register: usize) -> Option<RuntimeValue> {
        match self.registers.get(register) {
            Some(RegisterValue::Initialized(value)) => Some(*value),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn test_snapshot(&self) -> (u8, usize, Box<[RegisterValue]>) {
        (
            match self.lifecycle {
                Lifecycle::Pristine => 0,
                Lifecycle::Runnable => 1,
            },
            self.frame_depth,
            self.registers.clone(),
        )
    }
}
