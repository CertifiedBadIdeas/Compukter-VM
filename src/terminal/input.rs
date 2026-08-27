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

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalInputError {
    InvalidLimits,
    InvalidKey,
    InvalidModifiers,
    QueueFull,
    TextTooLarge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalInputLimits {
    maximum_events: usize,
    maximum_text_code_points: usize,
}

impl TerminalInputLimits {
    pub fn new(
        maximum_events: usize,
        maximum_text_code_points: usize,
    ) -> Result<Self, TerminalInputError> {
        if maximum_events == 0 || maximum_text_code_points == 0 {
            return Err(TerminalInputError::InvalidLimits);
        }
        Ok(Self {
            maximum_events,
            maximum_text_code_points,
        })
    }
}

impl Default for TerminalInputLimits {
    fn default() -> Self {
        Self {
            maximum_events: 256,
            maximum_text_code_points: 4_096,
        }
    }
}

#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalKey {
    Escape = 1,
    Backspace = 8,
    Tab = 9,
    Enter = 13,
    S = 83,
    X = 88,
    Insert = 256,
    Delete = 257,
    Home = 258,
    End = 259,
    PageUp = 260,
    PageDown = 261,
    Up = 262,
    Left = 263,
    Down = 264,
    Right = 265,
    F1 = 272,
    F2 = 273,
    F3 = 274,
    F4 = 275,
    F5 = 276,
    F6 = 277,
    F7 = 278,
    F8 = 279,
    F9 = 280,
    F10 = 281,
    F11 = 282,
    F12 = 283,
}

impl TerminalKey {
    pub const fn code(self) -> u16 {
        self as u16
    }
}

impl TryFrom<u16> for TerminalKey {
    type Error = TerminalInputError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Escape),
            8 => Ok(Self::Backspace),
            9 => Ok(Self::Tab),
            13 => Ok(Self::Enter),
            83 => Ok(Self::S),
            88 => Ok(Self::X),
            256 => Ok(Self::Insert),
            257 => Ok(Self::Delete),
            258 => Ok(Self::Home),
            259 => Ok(Self::End),
            260 => Ok(Self::PageUp),
            261 => Ok(Self::PageDown),
            262 => Ok(Self::Up),
            263 => Ok(Self::Left),
            264 => Ok(Self::Down),
            265 => Ok(Self::Right),
            272 => Ok(Self::F1),
            273 => Ok(Self::F2),
            274 => Ok(Self::F3),
            275 => Ok(Self::F4),
            276 => Ok(Self::F5),
            277 => Ok(Self::F6),
            278 => Ok(Self::F7),
            279 => Ok(Self::F8),
            280 => Ok(Self::F9),
            281 => Ok(Self::F10),
            282 => Ok(Self::F11),
            283 => Ok(Self::F12),
            _ => Err(TerminalInputError::InvalidKey),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalKeyAction {
    Press,
    Repeat,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerminalModifiers(u8);

impl TerminalModifiers {
    pub const SHIFT: u8 = 1 << 0;
    pub const CONTROL: u8 = 1 << 1;
    pub const ALT: u8 = 1 << 2;
    pub const SUPER: u8 = 1 << 3;
    const ALL: u8 = Self::SHIFT | Self::CONTROL | Self::ALT | Self::SUPER;

    pub fn new(bits: u8) -> Result<Self, TerminalInputError> {
        if bits & !Self::ALL != 0 {
            return Err(TerminalInputError::InvalidModifiers);
        }
        Ok(Self(bits))
    }

    pub const fn bits(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalKeyEvent {
    key: TerminalKey,
    action: TerminalKeyAction,
    modifiers: TerminalModifiers,
}

impl TerminalKeyEvent {
    pub const fn new(
        key: TerminalKey,
        action: TerminalKeyAction,
        modifiers: TerminalModifiers,
    ) -> Self {
        Self {
            key,
            action,
            modifiers,
        }
    }

    pub const fn key(self) -> TerminalKey {
        self.key
    }

    pub const fn action(self) -> TerminalKeyAction {
        self.action
    }

    pub const fn modifiers(self) -> TerminalModifiers {
        self.modifiers
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalInputEvent {
    Key(TerminalKeyEvent),
    Text(Box<str>),
}

#[derive(Debug)]
pub(super) struct TerminalInputQueue {
    limits: TerminalInputLimits,
    events: VecDeque<TerminalInputEvent>,
}

impl TerminalInputQueue {
    pub(super) fn new(limits: TerminalInputLimits) -> Self {
        Self {
            limits,
            events: VecDeque::new(),
        }
    }

    pub(super) fn push_key(&mut self, event: TerminalKeyEvent) -> Result<(), TerminalInputError> {
        self.ensure_capacity()?;
        self.events.push_back(TerminalInputEvent::Key(event));
        Ok(())
    }

    pub(super) fn push_text(&mut self, text: &str) -> Result<(), TerminalInputError> {
        if text.chars().count() > self.limits.maximum_text_code_points {
            return Err(TerminalInputError::TextTooLarge);
        }
        self.ensure_capacity()?;
        self.events.push_back(TerminalInputEvent::Text(text.into()));
        Ok(())
    }

    pub(super) fn poll(&mut self) -> Option<TerminalInputEvent> {
        self.events.pop_front()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    fn ensure_capacity(&self) -> Result<(), TerminalInputError> {
        if self.events.len() >= self.limits.maximum_events {
            Err(TerminalInputError::QueueFull)
        } else {
            Ok(())
        }
    }
}
