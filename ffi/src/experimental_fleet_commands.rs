/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use super::FleetTransport;
use crate::FfiStatus;
use compukter_vm::experimental_fleet_scheduler::{ScalarCommand, SubmitError};
use std::time::{Duration, Instant};

pub const COMMAND_BATCH_VERSION: u32 = 1;
pub const COMMAND_BATCH_CAPACITY: usize = 16;
pub const COMMAND_BATCH_BYTES: usize = 8 + COMMAND_BATCH_CAPACITY * 40;

fn decode(bytes: &[u8]) -> Option<([ScalarCommand; COMMAND_BATCH_CAPACITY], usize)> {
    if bytes.len() < 8 || bytes.len() > COMMAND_BATCH_BYTES {
        return None;
    }
    let u32_at =
        |at| -> Option<u32> { Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?)) };
    let version: Option<u32> = u32_at(0);
    let count = u32_at(4)? as usize;
    if version != Some(COMMAND_BATCH_VERSION)
        || count == 0
        || count > COMMAND_BATCH_CAPACITY
        || bytes.len() != 8 + 40 * count
    {
        return None;
    }
    let mut commands = [ScalarCommand::Input {
        machine: 0,
        packet: 0,
    }; COMMAND_BATCH_CAPACITY];
    let now = Instant::now();
    for (index, command) in commands[..count].iter_mut().enumerate() {
        let at = 8 + index * 40;
        let u64_at = |offset| -> Option<u64> {
            Some(u64::from_le_bytes(
                bytes.get(at + offset..at + offset + 8)?.try_into().ok()?,
            ))
        };
        let machine = u64_at(0)?;
        let kind = u32_at(at + 8)?;
        let argument = u32_at(at + 12)?;
        let value = u64_at(16)?;
        let extra = u64_at(24)? as i64;
        let authorization = u64_at(32)?;
        if machine > i64::MAX as u64 || value > i64::MAX as u64 || (kind != 4 && authorization != 0)
        {
            return None;
        }
        *command = match kind {
            0 if argument <= i32::MAX as u32 && extra >= -1 => ScalarCommand::Grant {
                machine,
                tick: value,
                retirements: argument,
                deadline: if extra == -1 {
                    None
                } else {
                    Some(now.checked_add(Duration::from_nanos(extra as u64))?)
                },
            },
            1 if argument > 0
                && argument <= i32::MAX as u32
                && value > 0
                && extra > 0
                && extra <= i32::MAX as i64 =>
            {
                ScalarCommand::Cost {
                    machine,
                    task: argument,
                    request: value,
                    units: extra as u32,
                }
            }
            2 if argument & !0x3fff_ffff == 0 && value == 0 && extra == 0 => ScalarCommand::Input {
                machine,
                packet: argument,
            },
            3 if argument & !0x3fff_ffff == 0 && value == 0 && extra == 0 => {
                ScalarCommand::Output {
                    machine,
                    packed: argument,
                }
            }
            4 if argument <= i32::MAX as u32 && extra >= 0 && authorization <= i64::MAX as u64 => {
                ScalarCommand::FrameGrant {
                    machine,
                    tick: value,
                    retirements: argument,
                    execution_time: Duration::from_nanos(extra as u64),
                    authorized_until: now.checked_add(Duration::from_nanos(authorization))?,
                }
            }
            _ => return None,
        };
    }
    Some((commands, count))
}

/// Validates the entire fixed-record wire before any admission. Result bytes borrow caller storage.
pub fn submit_commands_v1(fleet: &FleetTransport, bytes: &[u8], output: &mut [u8]) -> FfiStatus {
    let Some((commands, count)) = decode(bytes) else {
        return FfiStatus::InvalidArgument;
    };
    if output.len() < count {
        return FfiStatus::InvalidArgument;
    }
    let mut results = [Err(SubmitError::InvalidInput); COMMAND_BATCH_CAPACITY];
    if fleet
        .scheduler
        .submit_scalar_batch(&commands[..count], &mut results[..count])
        .is_err()
    {
        return FfiStatus::InvalidArgument;
    }
    for (result, output) in results[..count].iter().zip(output.iter_mut()) {
        *output = match result {
            Ok(()) => FfiStatus::Ok,
            Err(SubmitError::Full) => FfiStatus::BusyHandle,
            Err(SubmitError::Closed) => FfiStatus::StaleHandle,
            Err(_) => FfiStatus::InvalidArgument,
        } as u8;
    }
    FfiStatus::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(kind: u32, argument: u32, value: u64, extra: i64) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(kind.to_le_bytes());
        bytes.extend(argument.to_le_bytes());
        bytes.extend(value.to_le_bytes());
        bytes.extend(extra.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes
    }
    #[test]
    fn frame_grants_distinguish_compute_duration_from_authorization() {
        let mut bytes = record(4, 4096, 12, 2000000);
        bytes[40..48].copy_from_slice(&50000000u64.to_le_bytes());
        let before = Instant::now();
        let (commands, count) = decode(&bytes).unwrap();
        assert_eq!(count, 1);
        let ScalarCommand::FrameGrant {
            tick,
            retirements,
            execution_time,
            authorized_until,
            ..
        } = commands[0]
        else {
            panic!("wrong command");
        };
        assert_eq!((tick, retirements), (12, 4096));
        assert_eq!(execution_time, Duration::from_millis(2));
        assert!(authorized_until >= before + Duration::from_millis(50));
        bytes[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode(&bytes).is_none());
        assert!(decode(&record(4, 1, 1, -1)).is_none());
    }

    #[test]
    fn fixed_records_validate_tags_lengths_ranges_and_reserved_bytes() {
        for bytes in [
            record(0, 4096, 1, -1),
            record(1, 1, 1, 1),
            record(2, 123, 0, 0),
            record(3, 123, 0, 0),
            record(4, 4096, 1, 1000000),
        ] {
            assert!(decode(&bytes).is_some());
        }
        for bytes in [
            record(5, 0, 0, 0),
            record(0, 1, 1, -2),
            record(1, 0, 1, 1),
            record(1, 1, 0, 1),
            record(2, 1 << 30, 0, 0),
            record(3, 1, 1, 0),
        ] {
            assert!(decode(&bytes).is_none());
        }
        let valid = record(0, 1, 1, 0);
        for end in 0..valid.len() {
            assert!(decode(&valid[..end]).is_none());
        }
        for offset in [0, 4, 15, 47] {
            let mut bytes = valid.clone();
            bytes[offset] = 255;
            assert!(decode(&bytes).is_none());
        }
        let mut bytes = valid.clone();
        bytes.push(0);
        assert!(decode(&bytes).is_none());
        assert!(decode(&vec![0; COMMAND_BATCH_BYTES + 1]).is_none());
    }
}
