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

//! Opt-in benchmark of the existing interpreter sources without test instrumentation.
#![allow(dead_code, unused_imports)]

// Compile the same private runtime modules as lib.rs. No copy of the execution
// implementation or benchmark-only public Session entrypoint is needed.
mod artifact;
mod bytes;
mod decode;
mod diagnostic;
mod execution;
mod limits;
mod verify;

use artifact::{EntryArguments, VerifiedArtifact};
use execution::{
    AccountingSnapshot, AdvanceOutcome, CapabilityBinding, EntryArgumentLimits, ExecutionProfile,
    HostResponse, HostValueInput, HostValueType, HostValueView, OperationSchema, RequestId,
    Session, TaskId,
};
use limits::ArtifactLimits;
use std::{fs, path::Path, sync::Arc, time::Instant};
use verify::verify_artifact;

const SLICE: u32 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Work([u64; 8]);

impl Work {
    fn between(after: AccountingSnapshot, before: AccountingSnapshot) -> Self {
        Self([
            after.fixed_guest_units - before.fixed_guest_units,
            after.dynamic_guest_units - before.dynamic_guest_units,
            after.maintenance_units - before.maintenance_units,
            after.entered_blocks - before.entered_blocks,
            after.executed_instructions - before.executed_instructions,
            after.retired_instructions - before.retired_instructions,
            after.published_requests - before.published_requests,
            after.accepted_responses - before.accepted_responses,
        ])
    }
}

struct Case {
    id: String,
    artifact: VerifiedArtifact,
    expected: String,
    samples: Vec<Measurement>,
}

#[derive(Clone, Copy)]
struct Measurement {
    create_ns: u128,
    hot_ns: u128,
    create: Work,
    hot: Work,
}

fn profile(heap_bytes: u32) -> ExecutionProfile {
    ExecutionProfile {
        heap_bytes,
        frame_storage_bytes: 1024 * 1024,
        maximum_call_depth: 64,
        maximum_coroutines: 1,
        maximum_channels: 0,
        maximum_channel_values: 0,
        maximum_host_requests: 64,
        maximum_events: 0,
        maximum_slice_budget: u32::MAX,
        compiler_abi: [0; 32],
        platform_abi: [0; 32],
        maximum_host_arguments: 16,
        maximum_outbound_utf16_code_units: 4096,
        maximum_inbound_utf16_code_units: 4096,
        maximum_accepted_responses: 64,
        entry_argument_limits: EntryArgumentLimits {
            maximum_count: 16,
            maximum_code_units_per_argument: 4096,
            maximum_total_code_units: 4096,
        },
    }
}

fn request(session: &mut Session) -> (RequestId, TaskId, String) {
    for _ in 0..100_000 {
        match session.advance(SLICE, SLICE).expect("benchmark advance") {
            AdvanceOutcome::SliceExhausted => {}
            AdvanceOutcome::HostRequestBatch(batch) => {
                assert_eq!(batch.len(), 1);
                let request = batch.get(0).unwrap();
                assert_eq!(request.namespace(), "compukter");
                assert_eq!(request.name(), "stdio");
                assert_eq!(request.operation(), 1);
                let text = match request.arguments().get(0) {
                    Some(HostValueView::String(text)) => String::from_utf16(text).unwrap(),
                    other => panic!("unexpected output: {other:?}"),
                };
                return (request.id(), request.task_id(), text);
            }
            outcome => panic!("unexpected benchmark outcome: {outcome:?}"),
        }
    }
    panic!("benchmark exceeded bounded slice count");
}

fn run(case: &Case, heap: u32, traced: bool) -> Measurement {
    let arguments = [HostValueType::String];
    let operations = [
        OperationSchema::asynchronous(&[], HostValueType::String),
        OperationSchema::synchronous(&arguments, HostValueType::Unit),
        OperationSchema::synchronous(&arguments, HostValueType::Unit),
    ];
    let bindings = [CapabilityBinding::new(
        "compukter",
        "stdio",
        1,
        0,
        &operations,
    )];
    let mut session = if traced {
        Session::admit(case.artifact.clone(), profile(heap), &bindings)
    } else {
        Session::admit_untraced(case.artifact.clone(), profile(heap), &bindings)
    }
    .expect("benchmark admission");
    session.start(&[]).expect("benchmark start");
    let before = session.accounting();
    let start = Instant::now();
    let (ready, task, text) = request(&mut session);
    let create_ns = start.elapsed().as_nanos();
    assert_eq!(text, "ready\n", "ready marker for {}", case.id);
    let create = Work::between(session.accounting(), before);
    let before = session.accounting();
    session
        .resume_for(task, ready, HostResponse::Success(HostValueInput::Unit))
        .expect("ready acknowledgement");
    let start = Instant::now();
    let (done, task, text) = request(&mut session);
    let hot_ns = start.elapsed().as_nanos();
    assert_eq!(text, case.expected, "checksum for {}", case.id);
    let hot = Work::between(session.accounting(), before);
    session
        .resume_for(task, done, HostResponse::Success(HostValueInput::Unit))
        .expect("output acknowledgement");
    for _ in 0..100_000 {
        match session
            .advance(SLICE, SLICE)
            .expect("benchmark termination")
        {
            AdvanceOutcome::SliceExhausted => {}
            AdvanceOutcome::Halted(None) => {
                return Measurement {
                    create_ns,
                    hot_ns,
                    create,
                    hot,
                };
            }
            outcome => panic!("unexpected benchmark termination: {outcome:?}"),
        }
    }
    panic!("benchmark termination exceeded slice bound");
}

fn median(values: impl Iterator<Item = u128>) -> u128 {
    let mut values: Vec<_> = values.collect();
    values.sort_unstable();
    values[values.len() / 2]
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert!(
        (5..=6).contains(&args.len()),
        "usage: guest-benchmark ARTIFACT_DIR REPORT_DIR SAMPLES HEAP_BYTES traced|untraced [ID_PREFIX,...]"
    );
    let inputs = Path::new(&args[0]);
    let outputs = Path::new(&args[1]);
    let repetitions: usize = args[2].parse().expect("sample count");
    assert!(
        repetitions >= 3 && repetitions % 2 == 1,
        "use at least three and an odd number of samples"
    );
    let heap: u32 = args[3].parse().expect("heap bytes");
    assert!(heap >= 32 && heap.is_multiple_of(16));
    let traced = match args[4].as_str() {
        "traced" => true,
        "untraced" => false,
        _ => panic!("expected traced or untraced"),
    };
    let filters: Vec<_> = args
        .get(5)
        .map_or(Vec::new(), |value| value.split(',').collect());
    assert!(filters.iter().all(|filter| !filter.is_empty()));
    let manifest = fs::read_to_string(inputs.join("manifest.tsv")).expect("benchmark manifest");
    let mut lines = manifest.lines();
    let header: Vec<_> = lines.next().expect("manifest header").split('\t').collect();
    let id_column = header
        .iter()
        .position(|value| *value == "id")
        .expect("id column");
    let checksum_column = header
        .iter()
        .position(|value| *value == "checksum")
        .expect("checksum column");
    let mut cases: Vec<_> = lines
        .filter_map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            assert_eq!(fields.len(), header.len());
            let id = fields[id_column];
            if !filters.is_empty() && !filters.iter().any(|filter| id.starts_with(filter)) {
                return None;
            }
            // Manifest IDs must remain filenames inside the explicit input directory.
            assert!(
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            );
            let bytes = fs::read(inputs.join(format!("{id}.cpkt"))).expect("benchmark artifact");
            Some(Case {
                id: id.into(),
                artifact: verify_artifact(Arc::from(bytes), ArtifactLimits::default())
                    .expect("benchmark verification"),
                expected: format!("{}\n", fields[checksum_column]),
                samples: Vec::with_capacity(repetitions),
            })
        })
        .collect();
    assert!(!cases.is_empty(), "no benchmark cases selected");
    for filter in filters {
        assert!(
            cases.iter().any(|case| case.id.starts_with(filter)),
            "no cases for {filter}"
        );
    }
    // Cross-check the production path against traced semantics before timing samples.
    for case in &cases {
        let warmup = run(case, heap, traced);
        let control = run(case, heap, !traced);
        assert_eq!(
            warmup.create, control.create,
            "construction work for {}",
            case.id
        );
        assert_eq!(warmup.hot, control.hot, "operation work for {}", case.id);
    }
    let mut raw = String::from("id\tmode\tsample\theap_bytes\tcreate_ns\thot_ns\n");
    for sample in 0..repetitions {
        for position in 0..cases.len() {
            let index = if sample % 2 == 0 {
                position
            } else {
                cases.len() - 1 - position
            };
            let case = &mut cases[index];
            let measurement = run(case, heap, traced);
            if let Some(previous) = case.samples.first() {
                assert_eq!(
                    previous.create, measurement.create,
                    "construction work for {}",
                    case.id
                );
                assert_eq!(
                    previous.hot, measurement.hot,
                    "operation work for {}",
                    case.id
                );
            }
            raw.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                case.id,
                args[4],
                sample + 1,
                heap,
                measurement.create_ns,
                measurement.hot_ns
            ));
            case.samples.push(measurement);
        }
        eprintln!("measurement round {}/{} complete", sample + 1, repetitions);
    }
    let mut summary = String::from("id\tmode\tsamples\theap_bytes\tcreate_median_ns\thot_min_ns\thot_median_ns\thot_max_ns\thot_fixed_units\thot_dynamic_units\thot_maintenance_units\thot_blocks\thot_executed_instructions\thot_retired_instructions\thot_requests\thot_responses\n");
    for case in cases {
        summary.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            case.id,
            args[4],
            repetitions,
            heap,
            median(case.samples.iter().map(|sample| sample.create_ns)),
            case.samples
                .iter()
                .map(|sample| sample.hot_ns)
                .min()
                .unwrap(),
            median(case.samples.iter().map(|sample| sample.hot_ns)),
            case.samples
                .iter()
                .map(|sample| sample.hot_ns)
                .max()
                .unwrap()
        ));
        for count in case.samples[0].hot.0 {
            summary.push_str(&format!("\t{count}"));
        }
        summary.push('\n');
    }
    fs::create_dir_all(outputs).expect("report directory");
    fs::write(outputs.join("samples.tsv"), raw).expect("sample report");
    fs::write(outputs.join("measurements.tsv"), &summary).expect("summary report");
    eprintln!(
        "{} cases saved in {}",
        summary.lines().count() - 1,
        outputs.display()
    );
}
