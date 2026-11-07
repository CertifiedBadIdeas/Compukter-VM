/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 * You may obtain a copy at https://www.apache.org/licenses/LICENSE-2.0
 */
//! Real ComputerMachine population, one ownership/engine variant per process.
use super::*;
use std::collections::HashSet;

fn text(machine: &mut ComputerMachine) -> String {
    let mut result = String::new();
    for row in machine
        .terminal_mut()
        .logical_cells()
        .collect::<Vec<_>>()
        .chunks(usize::from(TERMINAL_WIDTH))
    {
        let line: String = row
            .iter()
            .map(|cell| char::from_u32(cell.code_point()).unwrap())
            .collect();
        result.push_str(line.trim_end());
        result.push('\n');
    }
    format!("{}\n", result.trim_end_matches('\n'))
}

fn settle(machines: &mut [ComputerMachine], shell: bool) {
    let mut finished = vec![false; machines.len()];
    for _ in 0..100_000 {
        for (machine, finished) in machines.iter_mut().zip(&mut finished) {
            if *finished {
                continue;
            }
            match machine
                .advance(SLICE, SLICE, 64)
                .expect("population advance")
            {
                ComputerAdvanceOutcome::SliceExhausted => {}
                ComputerAdvanceOutcome::WaitingForTerminalEvent if shell => *finished = true,
                ComputerAdvanceOutcome::Halted(None) if !shell => *finished = true,
                other => panic!("unexpected population outcome: {other:?}"),
            }
        }
        if finished.iter().all(|done| *done) {
            return;
        }
    }
    panic!("population exceeded bounded advance count");
}

fn work(snapshot: ComputerResourceSnapshot) -> [u64; 6] {
    [
        snapshot.fixed_guest_units,
        snapshot.dynamic_guest_units,
        snapshot.maintenance_units,
        snapshot.entered_blocks,
        snapshot.executed_instructions,
        snapshot.retired_instructions,
    ]
}

pub(super) fn main(args: &[String]) {
    assert_eq!(7, args.len(), "usage: guest-benchmark sharing ARTIFACT OUTPUT POPULATION shared|isolated shell|CHECKSUM reference|offsets HEAP_BYTES");
    let input = Path::new(&args[0]);
    let output = Path::new(&args[1]);
    let population: usize = args[2].parse().unwrap();
    assert!((1..=1024).contains(&population));
    let shared = match args[3].as_str() {
        "shared" => true,
        "isolated" => false,
        _ => panic!("ownership mode"),
    };
    let shell = args[4] == "shell";
    let offsets = match args[5].as_str() {
        "reference" => false,
        "offsets" => true,
        _ => panic!("engine"),
    };
    assert!(!offsets || cfg!(feature = "execution-prototype"));
    let heap: u32 = args[6].parse().unwrap();
    let bytes = fs::read(input).unwrap();
    let mut identities = Vec::with_capacity(population);
    let mut boot_work = Vec::with_capacity(population);
    let mut final_work = Vec::with_capacity(population);
    // Initialize thread clock/TLS before the live-allocation baseline.
    let _ = thread_cpu_ns();
    let live_before = allocation_counter::live_bytes();
    let mut machines = Vec::with_capacity(population);
    let mut verify_cpu = 0;
    let mut admit_cpu = 0;
    #[cfg(feature = "execution-prototype")]
    let mut prepare_cpu = 0;
    #[cfg(not(feature = "execution-prototype"))]
    let prepare_cpu = 0;
    for _ in 0..population {
        let start = thread_cpu_ns();
        let input: Arc<[u8]> = Arc::from(bytes.clone());
        let artifact = if shared {
            verify_artifact(input, ArtifactLimits::default())
        } else {
            verify::verify_artifact_unshared(input, ArtifactLimits::default())
        }
        .expect("verified artifact");
        verify_cpu += cpu_elapsed(start).unwrap_or(0);
        let start = thread_cpu_ns();
        let mut selected = profile(heap);
        selected.maximum_coroutines = 64;
        selected.maximum_channels = 64;
        selected.maximum_channel_values = 4096;
        selected.maximum_events = 64;
        let machine =
            ComputerMachine::start(artifact, selected, &[], &[]).expect("computer admission/start");
        admit_cpu += cpu_elapsed(start).unwrap_or(0);
        #[cfg(feature = "execution-prototype")]
        let mut machine = machine;
        if offsets {
            #[cfg(feature = "execution-prototype")]
            {
                let start = thread_cpu_ns();
                machine
                    .prepare_execution_benchmark(execution::prototype::ExecutionMode::Offsets)
                    .unwrap();
                prepare_cpu += cpu_elapsed(start).unwrap_or(0);
            }
        }
        identities.push(machine.shared_code_identity());
        machines.push(machine);
    }
    let live_admitted = allocation_counter::live_bytes();
    let start = thread_cpu_ns();
    settle(&mut machines, shell);
    let boot_cpu = cpu_elapsed(start).unwrap_or(0);
    boot_work.extend(
        machines
            .iter()
            .map(|machine| work(machine.resource_snapshot())),
    );
    assert!(boot_work.iter().all(|value| *value == boot_work[0]));
    for machine in &mut machines {
        assert_eq!(
            if shell {
                ">\n".to_owned()
            } else {
                format!("ready\n{}\n", args[4])
            },
            text(machine)
        );
    }
    let live_settled = allocation_counter::live_bytes();
    let mut echo_cpu = 0;
    if shell {
        let start = thread_cpu_ns();
        // Distinct text proves one computer cannot overwrite another's shell state.
        for (index, machine) in machines.iter_mut().enumerate() {
            machine
                .terminal_mut()
                .push_text(&format!("echo vm-{index:04}"))
                .unwrap();
        }
        settle(&mut machines, true);
        for machine in &mut machines {
            machine
                .submit_terminal_key(TerminalKeyEvent::new(
                    TerminalKey::Enter,
                    TerminalKeyAction::Press,
                    TerminalModifiers::new(0).unwrap(),
                ))
                .unwrap();
        }
        settle(&mut machines, true);
        echo_cpu = cpu_elapsed(start).unwrap_or(0);
        for (index, machine) in machines.iter_mut().enumerate() {
            assert_eq!(
                format!("> echo vm-{index:04}\nvm-{index:04}\n>\n"),
                text(machine)
            );
        }
    }
    final_work.extend(
        machines
            .iter()
            .map(|machine| work(machine.resource_snapshot())),
    );
    assert!(final_work.iter().all(|value| *value == final_work[0]));
    let unique_programs = identities
        .iter()
        .map(|id| id.0)
        .collect::<HashSet<_>>()
        .len();
    let unique_typed = identities
        .iter()
        .filter_map(|id| id.1)
        .collect::<HashSet<_>>()
        .len();
    assert_eq!(if shared { 1 } else { population }, unique_programs);
    assert_eq!(if offsets { unique_programs } else { 0 }, unique_typed);
    let boot_counts = boot_work[0];
    let final_counts = final_work[0];
    drop(machines);
    let live_after_drop = allocation_counter::live_bytes();
    let mut report = String::from("ownership\tengine\tpopulation\tverify_cpu_ns\tadmit_cpu_ns\tprepare_cpu_ns\tboot_cpu_ns\techo_cpu_ns\tlive_before\tlive_admitted\tlive_settled\tlive_after_drop\tunique_programs\tunique_typed");
    for phase in ["boot", "final"] {
        for name in [
            "fixed",
            "dynamic",
            "maintenance",
            "blocks",
            "executed",
            "retired",
        ] {
            report.push_str(&format!("\t{phase}_{name}"));
        }
    }
    report.push_str(&format!("\n{}\t{}\t{population}\t{verify_cpu}\t{admit_cpu}\t{prepare_cpu}\t{boot_cpu}\t{echo_cpu}\t{live_before}\t{live_admitted}\t{live_settled}\t{live_after_drop}\t{unique_programs}\t{unique_typed}", args[3], args[5]));
    for count in boot_counts.into_iter().chain(final_counts) {
        report.push_str(&format!("\t{count}"));
    }
    report.push('\n');
    fs::write(output, report).unwrap();
}
