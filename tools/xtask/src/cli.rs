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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BumpKind {
    Revision,
    Abi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Check,
    Bump(BumpKind),
    Release,
}

impl Command {
    pub fn parse<I, S>(arguments: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let arguments = arguments
            .into_iter()
            .map(|argument| argument.as_ref().to_owned())
            .collect::<Vec<_>>();
        match arguments.as_slice() {
            [command] if command == "check" => Ok(Self::Check),
            [command, kind] if command == "bump" && kind == "revision" => {
                Ok(Self::Bump(BumpKind::Revision))
            }
            [command, kind] if command == "bump" && kind == "abi" => Ok(Self::Bump(BumpKind::Abi)),
            [command] if command == "release" => Ok(Self::Release),
            _ => Err("usage: cargo xtask <check|bump revision|bump abi|release>".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BumpKind, Command};

    #[test]
    fn parses_the_exact_release_commands() {
        assert_eq!(Command::Check, Command::parse(["check"]).unwrap());
        assert_eq!(
            Command::Bump(BumpKind::Revision),
            Command::parse(["bump", "revision"]).unwrap()
        );
        assert_eq!(
            Command::Bump(BumpKind::Abi),
            Command::parse(["bump", "abi"]).unwrap()
        );
        assert_eq!(Command::Release, Command::parse(["release"]).unwrap());
        assert!(Command::parse(["runtime", "release"]).is_err());
        assert!(Command::parse(["release", "now"]).is_err());
    }
}
