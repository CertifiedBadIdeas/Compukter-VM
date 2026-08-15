/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use super::{
    machine::validate_config, Rv32ElfLoader, Rv32LoadedImage, Rv32Machine, Rv32MachineBuildError,
    Rv32MachineConfig,
};
use crate::bus::MmioDevice;
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

pub struct Rv32DeviceHandle<T> {
    ordinal: usize,
    marker: PhantomData<fn() -> T>,
}

impl<T> Rv32DeviceHandle<T> {
    fn new(ordinal: usize) -> Self {
        Self {
            ordinal,
            marker: PhantomData,
        }
    }

    pub(super) fn ordinal(self) -> usize {
        self.ordinal
    }
}

impl<T> Copy for Rv32DeviceHandle<T> {}

impl<T> Clone for Rv32DeviceHandle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Debug for Rv32DeviceHandle<T> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Rv32DeviceHandle")
            .field("ordinal", &self.ordinal)
            .finish()
    }
}

impl<T> PartialEq for Rv32DeviceHandle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.ordinal == other.ordinal
    }
}

impl<T> Eq for Rv32DeviceHandle<T> {}

impl<T> Hash for Rv32DeviceHandle<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.ordinal.hash(state);
    }
}

pub(super) struct PendingMmioDevice {
    pub(super) base: u32,
    pub(super) device: Box<dyn MmioDevice>,
}

pub struct Rv32MachineBuilder {
    image: Rv32LoadedImage,
    config: Rv32MachineConfig,
    devices: Vec<PendingMmioDevice>,
}

impl Rv32MachineBuilder {
    pub fn from_elf(elf: &[u8], config: Rv32MachineConfig) -> Result<Self, Rv32MachineBuildError> {
        validate_config(config)?;
        let image = Rv32ElfLoader::load(elf, config.ram_size)?;
        Ok(Self {
            image,
            config,
            devices: Vec::new(),
        })
    }

    pub fn add_mmio_device<T: MmioDevice>(&mut self, base: u32, device: T) -> Rv32DeviceHandle<T> {
        let handle = Rv32DeviceHandle::new(self.devices.len());
        self.devices.push(PendingMmioDevice {
            base,
            device: Box::new(device),
        });
        handle
    }

    pub fn build(self) -> Result<Rv32Machine, Rv32MachineBuildError> {
        Rv32Machine::from_loaded_image(self.image, self.config, self.devices)
    }
}
