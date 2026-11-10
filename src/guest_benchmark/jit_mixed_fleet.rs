/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */
//! Native worker scaling; this does not exercise the Minecraft carrier scheduler.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::Barrier;

fn pin(cpu: usize) {
    #[cfg(target_os = "linux")]
    {
        unsafe extern "C" {
            fn sched_setaffinity(pid: i32, size: usize, mask: *const usize) -> i32;
        }
        let mut mask = [0usize; 16];
        assert!(cpu < 1024);
        mask[cpu / usize::BITS as usize] |= 1 << (cpu % usize::BITS as usize);
        // SAFETY: a valid, initialized CPU mask is passed for the calling thread.
        assert_eq!(
            unsafe { sched_setaffinity(0, std::mem::size_of_val(&mask), mask.as_ptr()) },
            0
        );
    }
}
fn digest(session: &Session) -> String {
    let mut writer = execution::checkpoint::Writer::new(4 * 1024 * 1024);
    session.write_checkpoint_state(&mut writer).unwrap();
    Sha256::digest(writer.finish())
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}
fn allocated() -> String {
    #[cfg(feature = "program-sharing-benchmark")]
    {
        allocation_counter::live_bytes().to_string()
    }
    #[cfg(not(feature = "program-sharing-benchmark"))]
    {
        String::new()
    }
}
fn stats(probes: &[Session], phase: &str, output: &mut String, cases: &[Case]) {
    for (case, probe) in cases.iter().zip(probes) {
        let s = probe.jit_statistics();
        output.push_str(&format!(
            "{}\t{phase}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            case.id,
            s.tracked,
            s.compiled,
            s.regions,
            s.pending,
            s.failures,
            s.code_bytes,
            s.mapped_bytes,
            s.state_bytes,
            s.compile_ns,
            s.hits,
            s.attempts,
            s.hot_telemetry
        ));
    }
}
fn drain(probes: &[Session]) {
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    while probes.iter().any(|p| p.jit_statistics().pending != 0) {
        assert!(Instant::now() < deadline, "compiler queue did not drain");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

pub(super) fn main(args: &[String]) {
    assert_eq!(args.len(), 9, "jit-mixed-fleet INPUT OUTPUT POPULATION PROGRAMS WORKERS HEAP off|on forward|reverse BATCHES");
    let inputs = Path::new(&args[0]);
    let outputs = Path::new(&args[1]);
    let population: usize = args[2].parse().unwrap();
    let programs: usize = args[3].parse().unwrap();
    let workers: usize = args[4].parse().unwrap();
    let heap: u32 = args[5].parse().unwrap();
    let enabled = match args[6].as_str() {
        "off" => false,
        "on" => true,
        _ => panic!("JIT mode"),
    };
    let reverse = match args[7].as_str() {
        "forward" => false,
        "reverse" => true,
        _ => panic!("order"),
    };
    let batches: usize = args[8].parse().unwrap();
    assert!(
        (1..=2000).contains(&population) && programs <= population && (1..=32).contains(&programs)
    );
    assert!([1, 4, 8].contains(&workers) && workers <= population && (1..=20).contains(&batches));
    let cpus: Vec<_> = match workers {
        1 => vec![6],
        4 => (3..7).collect(),
        _ => (0..8).collect(),
    };
    pin(8); // The compiler inherits this core; workers explicitly select different physical cores.
    JIT_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
    fs::create_dir_all(outputs).unwrap();
    let manifest = fs::read_to_string(inputs.join("manifest.tsv")).unwrap();
    let cases: Vec<_> = manifest
        .lines()
        .skip(1)
        .take(programs)
        .map(|row| {
            let fields: Vec<_> = row.split('\t').collect();
            assert!(fields[0]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'));
            let bytes: Arc<[u8]> =
                Arc::from(fs::read(inputs.join(format!("{}.cpkt", fields[0]))).unwrap());
            let artifact = verify_artifact(bytes.clone(), ArtifactLimits::default()).unwrap();
            let copy = verify_artifact(bytes, ArtifactLimits::default()).unwrap();
            assert!(
                Arc::ptr_eq(
                    &artifact.prepared_program().unwrap(),
                    &copy.prepared_program().unwrap()
                ),
                "identical bytes must share prepared code"
            );
            Case {
                id: fields[0].into(),
                expected: format!("{}\n", fields[1]),
                artifact,
                samples: Vec::new(),
            }
        })
        .collect();
    assert_eq!(cases.len(), programs);
    let identities: HashSet<_> = cases
        .iter()
        .map(|c| Arc::as_ptr(&c.artifact.prepared_program().unwrap()) as usize)
        .collect();
    assert_eq!(
        identities.len(),
        programs,
        "programs must be genuinely distinct"
    );
    let probes: Vec<_> = cases.iter().map(|c| admit(c, heap, false)).collect();
    let mut telemetry = String::from("id\tphase\ttracked\tcompiled\tregions\tpending\tfailures\tcode_bytes\tmapped_bytes\tstate_bytes\tcompile_ns\thits\tnative_attempts\thot_telemetry\n");
    let warm_start = Instant::now();
    // Warm low-frequency construction paths too: a handful of complete runs
    // leaves once-per-session initializers below HOT even while loop bodies are hot.
    if enabled {
        for pass in 0..512 {
            for offset in 0..programs {
                let index = (offset + pass) % programs;
                let index = if reverse { programs - 1 - index } else { index };
                let mut session = admit(&cases[index], heap, false);
                session.start(&[]).unwrap();
                let (ready, task, text) = request(&mut session);
                assert_eq!(text, "ready\n");
                session
                    .resume_for(task, ready, HostResponse::Success(HostValueInput::Unit))
                    .unwrap();
                for _ in 0..20 {
                    assert!(matches!(
                        session
                            .advance_with_retirement_limit(16384, 16384, 4096)
                            .unwrap(),
                        AdvanceOutcome::SliceExhausted
                    ));
                }
                drain(&probes[index..index + 1]);
            }
        }
        stats(&probes, "initializers-warm", &mut telemetry, &cases);
    }
    // Keep program owners alive, visit every program each pass, and expose admission order.
    for pass in 0..if enabled { 2 } else { 1 } {
        for offset in 0..programs {
            let index = (offset + pass) % programs;
            let index = if reverse { programs - 1 - index } else { index };
            run(&cases[index], heap, false);
        }
        drain(&probes);
        stats(&probes, &format!("warm-{pass}"), &mut telemetry, &cases);
    }
    let warm_ns = warm_start.elapsed().as_nanos();
    let base_live = allocated();
    fs::write(
        outputs.join("base-smaps.txt"),
        fs::read_to_string("/proc/self/smaps_rollup").unwrap(),
    )
    .unwrap();
    let mut groups: Vec<Vec<(usize, Session)>> = (0..workers).map(|_| Vec::new()).collect();
    let create = Instant::now();
    for index in 0..population {
        let program = index % programs;
        let mut session = admit(&cases[program], heap, false);
        session.start(&[]).unwrap();
        let (ready, task, text) = request(&mut session);
        assert_eq!(text, "ready\n");
        session
            .resume_for(task, ready, HostResponse::Success(HostValueInput::Unit))
            .unwrap();
        groups[index * workers / population].push((program, session));
    }
    let admission_ns = create.elapsed().as_nanos();
    drain(&probes);
    let ready_live = allocated();
    stats(&probes, "before", &mut telemetry, &cases);
    let start = Barrier::new(workers + 1);
    let end = Barrier::new(workers + 1);
    let mut walls = Vec::with_capacity(batches);
    let (groups, measurements) = std::thread::scope(|scope| {
        let handles: Vec<_> = groups
            .into_iter()
            .zip(&cpus)
            .enumerate()
            .map(|(worker, (mut group, &cpu))| {
                let start = &start;
                let end = &end;
                scope.spawn(move || {
                    pin(cpu);
                    let mut samples = Vec::with_capacity(batches);
                    for batch in 0..batches {
                        start.wait();
                        let before = thread_cpu_ns();
                        let wall = Instant::now();
                        for (_, session) in &mut group {
                            assert!(matches!(
                                session
                                    .advance_with_retirement_limit(16384, 16384, 4096)
                                    .unwrap(),
                                AdvanceOutcome::SliceExhausted
                            ));
                        }
                        let elapsed_wall = wall.elapsed().as_nanos();
                        let cpu_ns = cpu_elapsed(before).unwrap();
                        samples.push((batch, worker, cpu, group.len(), cpu_ns, elapsed_wall));
                        end.wait();
                    }
                    (group, samples)
                })
            })
            .collect();
        for batch in 0..batches {
            let wall = Instant::now();
            start.wait();
            end.wait();
            walls.push(wall.elapsed().as_nanos());
            stats(&probes, &format!("batch-{batch}"), &mut telemetry, &cases);
        }
        let mut groups = Vec::new();
        let mut measurements = Vec::new();
        for handle in handles {
            let (group, samples) = handle.join().unwrap();
            groups.push(group);
            measurements.extend(samples);
        }
        (groups, measurements)
    });
    let active_live = allocated();
    fs::write(
        outputs.join("active-smaps.txt"),
        fs::read_to_string("/proc/self/smaps_rollup").unwrap(),
    )
    .unwrap();
    stats(&probes, "after", &mut telemetry, &cases);
    fs::write(outputs.join("jit.tsv"), telemetry).unwrap();
    let mut batch_rows = String::from("batch\twall_ns\tcpu_ns\tworker_max_wall_ns\tretired\n");
    for (batch, wall) in walls.into_iter().enumerate() {
        let selected: Vec<_> = measurements.iter().filter(|s| s.0 == batch).collect();
        batch_rows.push_str(&format!(
            "{batch}\t{wall}\t{}\t{}\t{}\n",
            selected.iter().map(|s| s.4).sum::<u128>(),
            selected.iter().map(|s| s.5).max().unwrap(),
            population * 4096
        ));
    }
    let mut worker_rows = String::from("batch\tworker\tcpu\tpopulation\tcpu_ns\twall_ns\n");
    for (batch, worker, cpu, count, cpu_ns, wall) in measurements {
        worker_rows.push_str(&format!(
            "{batch}\t{worker}\t{cpu}\t{count}\t{cpu_ns}\t{wall}\n"
        ));
    }
    fs::write(outputs.join("batches.tsv"), batch_rows).unwrap();
    fs::write(outputs.join("workers.tsv"), worker_rows).unwrap();
    let mut machines: Vec<_> = groups.into_iter().flatten().collect();
    let mut states: Vec<Option<(String, AccountingSnapshot)>> =
        (0..programs).map(|_| None).collect();
    let mut populations = vec![0usize; programs];
    for (program, session) in &machines {
        let state = (digest(session), session.accounting());
        if let Some(expected) = &states[*program] {
            assert_eq!(&state, expected);
        } else {
            states[*program] = Some(state);
        }
        populations[*program] += 1;
    }
    let mut checks = String::from("id\tpopulation\tcheckpoint_sha256\tfixed\tdynamic\tmaintenance\tblocks\texecuted\tretired\trequests\tresponses\n");
    for (index, case) in cases.iter().enumerate() {
        let (hash, accounting) = states[index].as_ref().unwrap();
        let work = Work([
            accounting.fixed_guest_units,
            accounting.dynamic_guest_units,
            accounting.maintenance_units,
            accounting.entered_blocks,
            accounting.executed_instructions,
            accounting.retired_instructions,
            accounting.published_requests,
            accounting.accepted_responses,
        ]);
        checks.push_str(&format!(
            "{}\t{}\t{hash}\t{}\n",
            case.id,
            populations[index],
            work.0.map(|v| v.to_string()).join("\t")
        ));
        let (_, session) = machines.iter_mut().find(|(p, _)| *p == index).unwrap();
        let (_, _, text) = request(session);
        assert_eq!(text, case.expected, "checksum for {}", case.id);
    }
    fs::write(outputs.join("checks.tsv"), checks).unwrap();
    fs::write(outputs.join("fleet.tsv"),format!("population\tprograms\tworkers\tmode\torder\twarm_ns\tadmission_ns\tbase_requested_bytes\tready_requested_bytes\tactive_requested_bytes\n{population}\t{programs}\t{workers}\t{}\t{}\t{warm_ns}\t{admission_ns}\t{base_live}\t{ready_live}\t{active_live}\n",args[6],args[7])).unwrap();
}
