/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 * You may obtain a copy at https://www.apache.org/licenses/LICENSE-2.0
 */
//! Real boot/shell/child trees; observations never enter the product ABI.
use super::*;
use crate::filesystem::{
    ComputerFileSystem, FileCapability, FileRights, FileSystemLimits, SystemImage, VirtualPath,
};
use std::collections::HashSet;
use std::io::{BufWriter, Write};

fn rom(inputs: &Path) -> SystemImage {
    use sha2::{Digest, Sha256};
    let mut bytes = b"CPKTSYS\0".to_vec();
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&3_u32.to_le_bytes());
    for (path, file) in [
        ("/bin/child", "child.cpkt"),
        ("/bin/shell", "shell.cpkt"),
        ("/boot/init", "boot.cpkt"),
    ] {
        let content = fs::read(inputs.join(file)).unwrap();
        bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
        bytes.extend_from_slice(path.as_bytes());
        bytes.extend_from_slice(&[2, 1, 0, 0]);
        bytes.extend_from_slice(&(content.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&content);
    }
    let digest = Sha256::digest(&bytes);
    bytes.extend_from_slice(&digest);
    SystemImage::admit(bytes.into(), &FileSystemLimits::default()).unwrap()
}

fn resident() -> [u64; 3] {
    let text = fs::read_to_string("/proc/self/smaps_rollup").expect("Linux smaps_rollup");
    let mut values = [0; 3];
    for line in text.lines() {
        let mut words = line.split_whitespace();
        match words.next().unwrap_or("") {
            "Rss:" => values[0] = words.next().unwrap().parse().unwrap(),
            "Pss:" => values[1] = words.next().unwrap().parse().unwrap(),
            "Private_Clean:" | "Private_Dirty:" => {
                values[2] += words.next().unwrap().parse::<u64>().unwrap()
            }
            _ => {}
        }
    }
    values
}

fn observe(
    output: &mut impl Write,
    directory: &Path,
    phase: &str,
    machines: &[ComputerMachine],
    cpu_ns: u128,
) {
    // Capture requested bytes before allocating observation strings/sets.
    let live = allocation_counter::live_bytes();
    let peak = allocation_counter::peak_bytes();
    let [rss, pss, private] = resident();
    let identities: HashSet<_> = machines
        .iter()
        .flat_map(ComputerMachine::process_program_identities)
        .collect();
    let layout_identities: HashSet<_> = machines
        .iter()
        .flat_map(ComputerMachine::process_layout_identities)
        .collect();
    let depth = machines
        .first()
        .map_or(0, |m| m.process_program_identities().count());
    let state = machines.first().map_or_else(
        ComputerResourceSnapshot::default,
        ComputerMachine::resource_snapshot,
    );
    for machine in machines {
        assert_eq!(depth, machine.process_program_identities().count());
        assert_eq!(state, machine.resource_snapshot());
    }
    writeln!(output, "{phase}\t{}\t{depth}\t{}\t{}\t{live}\t{peak}\t{rss}\t{pss}\t{private}\t{cpu_ns}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        machines.len(), identities.len(), layout_identities.len(), state.heap_capacity_bytes, state.heap_used_bytes,
        state.mutable_execution_resident_bytes, state.fixed_guest_units, state.dynamic_guest_units,
        state.maintenance_units, state.entered_blocks, state.executed_instructions, state.retired_instructions).unwrap();
    if let Some(machine) = machines.first() {
        let mut components = String::from("process\tprogram_identity\tcomponent\tvalue\n");
        machine.write_memory_statistics(&mut components).unwrap();
        fs::write(
            directory.join(format!("{phase}-components.tsv")),
            components,
        )
        .unwrap();
    }
}

fn command(machines: &mut [ComputerMachine], command: impl Fn(usize) -> String) {
    for (index, machine) in machines.iter_mut().enumerate() {
        machine.terminal_mut().push_text(&command(index)).unwrap();
    }
    sharing_benchmark::settle(machines, true);
    for machine in machines {
        machine
            .submit_terminal_key(TerminalKeyEvent::new(
                TerminalKey::Enter,
                TerminalKeyAction::Press,
                TerminalModifiers::new(0).unwrap(),
            ))
            .unwrap();
    }
}

pub(super) fn main(args: &[String]) {
    assert_eq!(
        4,
        args.len(),
        "usage: guest-benchmark memory INPUT_DIR REPORT_DIR POPULATION boot|root-shell"
    );
    let inputs = Path::new(&args[0]);
    let directory = Path::new(&args[1]);
    let population: usize = args[2].parse().unwrap();
    assert!((1..=1000).contains(&population));
    let boot = match args[3].as_str() {
        "boot" => true,
        "root-shell" => false,
        _ => panic!("mode"),
    };
    fs::create_dir_all(directory).unwrap();
    let image = rom(inputs);
    let mut output = BufWriter::new(fs::File::create(directory.join("memory.tsv")).unwrap());
    writeln!(output, "phase\tpopulation\tprocess_depth\tunique_programs\tunique_allocation_layouts\tlive_requested\tpeak_requested\trss_kib\tpss_kib\tprivate_kib\tphase_cpu_ns\theap_capacity_per_computer\theap_used_per_computer\tadmitted_mutable_per_computer\tfixed\tdynamic\tmaintenance\tblocks\texecuted\tretired").unwrap();
    let mut machines = Vec::new();
    let _ = thread_cpu_ns();
    // Input ROM/template and report buffers are outside the requested-byte baseline.
    // ComputerMachine carrier storage is allocated after it and included in live bytes.
    let _ = resident();
    allocation_counter::reset_peak();
    observe(&mut output, directory, "baseline", &machines, 0);
    allocation_counter::reset_peak();
    let start = thread_cpu_ns();
    machines.try_reserve_exact(population).unwrap();
    for _ in 0..population {
        let mut selected = profile(256 * 1024);
        selected.maximum_coroutines = 64;
        selected.maximum_channels = 64;
        selected.maximum_channel_values = 4096;
        selected.maximum_events = 64;
        let filesystem =
            ComputerFileSystem::with_system_image(FileSystemLimits::default(), image.clone())
                .unwrap();
        let owner = FileCapability::new(VirtualPath::root(), FileRights::OWNER);
        let machine = if boot {
            ComputerMachine::boot_in_filesystem(
                selected,
                ProcessLimits::default(),
                &[],
                filesystem,
                owner,
            )
        } else {
            let artifact = verify_artifact(
                Arc::from(fs::read(inputs.join("shell.cpkt")).unwrap()),
                ArtifactLimits::default(),
            )
            .unwrap();
            ComputerMachine::start_in_filesystem_with_process_limits(
                artifact,
                selected,
                ProcessLimits::default(),
                &[],
                &[],
                filesystem,
                owner,
            )
        }
        .unwrap();
        machines.push(machine);
    }
    observe(
        &mut output,
        directory,
        "admitted",
        &machines,
        cpu_elapsed(start).unwrap(),
    );
    allocation_counter::reset_peak();
    let start = thread_cpu_ns();
    sharing_benchmark::settle(&mut machines, true);
    for machine in &mut machines {
        assert_eq!(sharing_benchmark::text(machine), ">\n");
    }
    assert_eq!(
        machines[0].process_program_identities().count(),
        if boot { 2 } else { 1 }
    );
    observe(
        &mut output,
        directory,
        "prompt",
        &machines,
        cpu_elapsed(start).unwrap(),
    );
    allocation_counter::reset_peak();
    let start = thread_cpu_ns();
    command(&mut machines, |_| "/bin/child".to_owned());
    for machine in &mut machines {
        for _ in 0..1000 {
            let outcome = machine.advance(SLICE, SLICE, 64).unwrap();
            if machine.process_program_identities().count() == if boot { 3 } else { 2 } {
                break;
            }
            assert_eq!(outcome, ComputerAdvanceOutcome::SliceExhausted);
        }
        assert_eq!(
            machine.process_program_identities().count(),
            if boot { 3 } else { 2 }
        );
    }
    observe(
        &mut output,
        directory,
        "child",
        &machines,
        cpu_elapsed(start).unwrap(),
    );
    allocation_counter::reset_peak();
    let start = thread_cpu_ns();
    sharing_benchmark::settle(&mut machines, true);
    for machine in &mut machines {
        assert!(sharing_benchmark::text(machine).contains("ready\n20000100000\n>\n"));
        assert_eq!(
            machine.process_program_identities().count(),
            if boot { 2 } else { 1 }
        );
    }
    command(&mut machines, |i| format!("echo vm-{i:04}"));
    sharing_benchmark::settle(&mut machines, true);
    for (i, machine) in machines.iter_mut().enumerate() {
        assert!(sharing_benchmark::text(machine)
            .ends_with(&format!("> echo vm-{i:04}\nvm-{i:04}\n>\n")));
    }
    observe(
        &mut output,
        directory,
        "resumed",
        &machines,
        cpu_elapsed(start).unwrap(),
    );
    drop(machines);
    allocation_counter::reset_peak();
    observe(&mut output, directory, "dropped", &[], 0);
}
