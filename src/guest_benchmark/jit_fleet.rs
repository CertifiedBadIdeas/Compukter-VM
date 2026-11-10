/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */
//! Retained native sessions: shared-code/memory evidence, not Minecraft carrier throughput.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

fn digest(session: &Session) -> Vec<u8> {
    let mut writer = execution::checkpoint::Writer::new(4 * 1024 * 1024);
    session.write_checkpoint_state(&mut writer).unwrap();
    Sha256::digest(writer.finish()).to_vec()
}

pub(super) fn main(args: &[String]) {
    assert_eq!(
        args.len(),
        6,
        "jit-fleet INPUT_DIR REPORT_DIR POPULATION HEAP_BYTES off|on CASE"
    );
    let inputs = Path::new(&args[0]);
    let outputs = Path::new(&args[1]);
    let count: usize = args[2].parse().unwrap();
    assert!((1..=2000).contains(&count));
    let heap: u32 = args[3].parse().unwrap();
    let enabled = match args[4].as_str() {
        "off" => false,
        "on" => true,
        _ => panic!("JIT mode"),
    };
    JIT_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
    let manifest = fs::read_to_string(inputs.join("manifest.tsv")).unwrap();
    let row = manifest
        .lines()
        .skip(1)
        .find(|line| line.split('\t').next() == Some(args[5].as_str()))
        .unwrap();
    let fields: Vec<_> = row.split('\t').collect();
    let case = Case {
        id: fields[0].into(),
        expected: format!("{}\n", fields[1]),
        samples: Vec::new(),
        artifact: verify_artifact(
            Arc::from(fs::read(inputs.join(format!("{}.cpkt", fields[0]))).unwrap()),
            ArtifactLimits::default(),
        )
        .unwrap(),
    };
    let warm_start = Instant::now();
    for _ in 0..66 {
        run(&case, heap, false);
    }
    let probe = admit(&case, heap, false);
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    while probe.jit_statistics().pending != 0 {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let warm_ns = warm_start.elapsed().as_nanos();
    let base_live = allocation_counter::live_bytes();
    let base_rss = fs::read_to_string("/proc/self/smaps_rollup").unwrap();
    fs::create_dir_all(outputs).unwrap();
    fs::write(outputs.join("base-smaps.txt"), base_rss).unwrap();
    let create_start = Instant::now();
    let mut machines = Vec::with_capacity(count);
    for _ in 0..count {
        let mut session = admit(&case, heap, false);
        session.start(&[]).unwrap();
        let (ready, task, text) = request(&mut session);
        assert_eq!(text, "ready\n");
        session
            .resume_for(task, ready, HostResponse::Success(HostValueInput::Unit))
            .unwrap();
        machines.push(session);
    }
    let admission_wall = create_start.elapsed().as_nanos();
    let ready_live = allocation_counter::live_bytes() - base_live;
    let identities: HashSet<_> = machines
        .iter()
        .map(|session| session.shared_code_identity().0)
        .collect();
    assert_eq!(identities.len(), 1, "one shared verified program");
    let mut raw = String::from("batch\twall_ns\tcpu_ns\tretired_per_machine\n");
    let mut retired = 0;
    for batch in 0..20 {
        let before = machines[0].accounting();
        let cpu = thread_cpu_ns();
        let wall = Instant::now();
        for session in &mut machines {
            assert!(matches!(
                session
                    .advance_with_retirement_limit(16384, 16384, 4096)
                    .unwrap(),
                AdvanceOutcome::SliceExhausted
            ));
        }
        let elapsed_wall = wall.elapsed().as_nanos();
        let elapsed_cpu = cpu_elapsed(cpu).unwrap();
        let work = Work::between(machines[0].accounting(), before);
        assert_eq!(work.0[5], 4096);
        retired += work.0[5];
        raw.push_str(&format!(
            "{batch}\t{elapsed_wall}\t{elapsed_cpu}\t{}\n",
            work.0[5]
        ));
    }
    let active_live = allocation_counter::live_bytes() - base_live;
    let stats = machines[0].jit_statistics();
    let active_rss = fs::read_to_string("/proc/self/smaps_rollup").unwrap();
    fs::write(outputs.join("active-smaps.txt"), active_rss).unwrap();
    let expected = digest(&machines[0]);
    let work = machines[0].accounting();
    for session in &machines {
        assert_eq!(digest(session), expected);
        assert_eq!(session.accounting(), work);
    }
    // Finish two representatives after the timed/snapshot interval; all retained
    // sessions already matched exact serialized state, including accounting and heap.
    for index in [0, count - 1] {
        let (_, _, text) = request(&mut machines[index]);
        assert_eq!(text, case.expected);
        if count == 1 {
            break;
        }
    }
    fs::create_dir_all(outputs).unwrap();
    fs::write(outputs.join("batches.tsv"), raw).unwrap();
    let summary = format!("population\tmode\tunique_programs\twarm_ns\tadmission_wall_ns\tbase_requested_bytes\tready_requested_bytes\tactive_requested_bytes\tretired_per_machine\tcompiled\tcode_bytes\tmapped_bytes_upper_bound\tstate_bytes\tcompile_ns\tcheckpoint_sha256\n{}\t{}\t1\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n", count, args[4], warm_ns, admission_wall, base_live, ready_live, active_live, retired, stats.compiled, stats.code_bytes, stats.mapped_bytes, stats.state_bytes, stats.compile_ns, expected.iter().map(|x| format!("{x:02x}")).collect::<String>());
    fs::write(outputs.join("fleet.tsv"), summary).unwrap();
    drop(machines);
    assert_eq!(probe.jit_statistics().compiled, stats.compiled);
}
