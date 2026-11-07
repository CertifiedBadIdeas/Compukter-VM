//! End-to-end paired experiment, including a retained round-robin VM population.
use super::*;
use execution::prototype::{ExecutionMode, PreparationStats};
use std::io::{BufRead, BufReader, Write};

// Optional perf stat --control FIFO protocol. Only hot execution enables PMU
// events; verification, admission, preparation, ready output and teardown stay out.
struct PerfControl(Option<(fs::File, BufReader<fs::File>)>);

impl PerfControl {
    fn from_env() -> Self {
        let control = std::env::var_os("COMPUKTER_PERF_CONTROL");
        let ack = std::env::var_os("COMPUKTER_PERF_ACK");
        Self(match (control, ack) {
            (None, None) => None,
            (Some(control), Some(ack)) => Some((
                fs::OpenOptions::new()
                    .write(true)
                    .open(control)
                    .expect("perf control FIFO"),
                BufReader::new(fs::File::open(ack).expect("perf acknowledgement FIFO")),
            )),
            _ => panic!("both perf control and acknowledgement paths are required"),
        })
    }

    fn switch(&mut self, enabled: bool) {
        if let Some((control, ack)) = &mut self.0 {
            control
                .write_all(if enabled { b"enable\n" } else { b"disable\n" })
                .unwrap();
            control.flush().unwrap();
            let mut response = String::new();
            ack.read_line(&mut response).unwrap();
            // Some perf versions NUL-terminate acknowledgements; read_line leaves
            // the terminator before the following acknowledgement.
            assert_eq!(
                response.trim_start_matches('\0'),
                "ack\n",
                "perf control acknowledgement"
            );
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Engine {
    Reference,
    Packed,
    PackedBatch,
    PackedSimd,
    Offsets,
}

impl Engine {
    fn name(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Packed => "packed",
            Self::PackedBatch => "packed-batch",
            Self::PackedSimd => "packed-simd",
            Self::Offsets => "offsets",
        }
    }
    fn mode(self) -> Option<ExecutionMode> {
        match self {
            Self::Reference => None,
            Self::Packed => Some(ExecutionMode::Packed),
            Self::PackedBatch => Some(ExecutionMode::PackedBatch),
            Self::PackedSimd => Some(ExecutionMode::PackedSimd),
            Self::Offsets => Some(ExecutionMode::Offsets),
        }
    }
}

struct Trial {
    admit_ns: u128,
    admit_cpu: Option<u128>,
    prepare_ns: u128,
    prepare_cpu: Option<u128>,
    create_ns: u128,
    create_cpu: Option<u128>,
    hot_ns: u128,
    hot_cpu: Option<u128>,
    rss_kib: Option<u64>,
    storage: Option<PreparationStats>,
    create: Work,
    hot: Work,
}

fn add_work(total: &mut Work, work: Work) {
    for (total, value) in total.0.iter_mut().zip(work.0) {
        *total = total.checked_add(value).expect("work counter overflow");
    }
}

fn add_cpu(total: &mut Option<u128>, value: Option<u128>) {
    *total = total.zip(value).map(|(a, b)| a.checked_add(b).unwrap());
}

fn rss_kib() -> Option<u64> {
    // A process-wide observation, not per-VM RSS or a resident-storage charge.
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("VmRSS:")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

fn trial(
    case: &Case,
    heap: u32,
    engine: Engine,
    population: usize,
    control: &mut PerfControl,
) -> Trial {
    let mut result = Trial {
        admit_ns: 0,
        admit_cpu: thread_cpu_ns().map(|_| 0),
        prepare_ns: 0,
        prepare_cpu: thread_cpu_ns().map(|_| 0),
        create_ns: 0,
        create_cpu: thread_cpu_ns().map(|_| 0),
        hot_ns: 0,
        hot_cpu: None,
        rss_kib: None,
        storage: None,
        create: Work([0; 8]),
        hot: Work([0; 8]),
    };
    let mut sessions = Vec::with_capacity(population);
    let mut before_hot = Vec::with_capacity(population);
    for _ in 0..population {
        let cpu = thread_cpu_ns();
        let start = Instant::now();
        let mut session = admit(case, heap, false);
        result.admit_ns += start.elapsed().as_nanos();
        add_cpu(&mut result.admit_cpu, cpu_elapsed(cpu));
        if let Some(mode) = engine.mode() {
            let cpu = thread_cpu_ns();
            let start = Instant::now();
            let stats = session
                .prepare_execution(mode)
                .expect("candidate preparation");
            result.prepare_ns += start.elapsed().as_nanos();
            add_cpu(&mut result.prepare_cpu, cpu_elapsed(cpu));
            assert!(
                stats.eligible > 0,
                "candidate never executes for {}",
                case.id
            );
            if let Some(previous) = result.storage {
                assert_eq!(previous.code_bytes, stats.code_bytes);
                assert_eq!(previous.eligible, stats.eligible);
            }
            result.storage = Some(stats);
        }
        session.start(&[]).expect("benchmark start");
        let before = session.accounting();
        let cpu = thread_cpu_ns();
        let start = Instant::now();
        let (ready, task, text) = request(&mut session);
        result.create_ns += start.elapsed().as_nanos();
        add_cpu(&mut result.create_cpu, cpu_elapsed(cpu));
        assert_eq!(text, "ready\n", "ready marker for {}", case.id);
        add_work(
            &mut result.create,
            Work::between(session.accounting(), before),
        );
        before_hot.push(session.accounting());
        session
            .resume_for(task, ready, HostResponse::Success(HostValueInput::Unit))
            .unwrap();
        sessions.push(session);
    }
    result.rss_kib = rss_kib();
    let mut done = vec![None; population];
    let mut remaining = population;
    control.switch(true);
    let cpu = thread_cpu_ns();
    let start = Instant::now();
    for _ in 0..100_000 {
        for (index, session) in sessions.iter_mut().enumerate() {
            if done[index].is_some() {
                continue;
            }
            match session.advance(SLICE, SLICE).expect("benchmark advance") {
                AdvanceOutcome::SliceExhausted => {}
                AdvanceOutcome::HostRequestBatch(batch) => {
                    assert_eq!(batch.len(), 1);
                    let request = batch.get(0).unwrap();
                    assert_eq!(
                        (request.namespace(), request.name(), request.operation()),
                        ("compukter", "stdio", 1)
                    );
                    let Some(HostValueView::String(text)) = request.arguments().get(0) else {
                        panic!("expected checksum string");
                    };
                    assert_eq!(
                        String::from_utf16(text).unwrap(),
                        case.expected,
                        "checksum for {}",
                        case.id
                    );
                    done[index] = Some((request.task_id(), request.id()));
                    remaining -= 1;
                }
                outcome => panic!("unexpected benchmark outcome: {outcome:?}"),
            }
        }
        if remaining == 0 {
            break;
        }
    }
    result.hot_ns = start.elapsed().as_nanos();
    result.hot_cpu = cpu_elapsed(cpu);
    control.switch(false);
    assert_eq!(remaining, 0, "population exceeded bounded slice count");
    for (index, session) in sessions.iter_mut().enumerate() {
        add_work(
            &mut result.hot,
            Work::between(session.accounting(), before_hot[index]),
        );
        let (task, request) = done[index].unwrap();
        session
            .resume_for(task, request, HostResponse::Success(HostValueInput::Unit))
            .unwrap();
        let mut halted = false;
        for _ in 0..100_000 {
            match session.advance(SLICE, SLICE).unwrap() {
                AdvanceOutcome::SliceExhausted => {}
                AdvanceOutcome::Halted(None) => {
                    halted = true;
                    break;
                }
                outcome => panic!("unexpected termination: {outcome:?}"),
            }
        }
        assert!(halted, "population termination exceeded slice bound");
    }
    result
}

fn load(inputs: &Path, prefix: &str) -> Vec<Case> {
    let manifest = fs::read_to_string(inputs.join("manifest.tsv")).unwrap();
    let mut lines = manifest.lines();
    let headers: Vec<_> = lines.next().unwrap().split('\t').collect();
    let id = headers.iter().position(|name| *name == "id").unwrap();
    let checksum = headers.iter().position(|name| *name == "checksum").unwrap();
    let mut cases = Vec::new();
    for line in lines {
        let fields: Vec<_> = line.split('\t').collect();
        assert_eq!(fields.len(), headers.len());
        let name = fields[id];
        if prefix != "-" && !name.starts_with(prefix) {
            continue;
        }
        assert!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        );
        assert!(
            !cases.iter().any(|case: &Case| case.id == name),
            "duplicate case"
        );
        let bytes = fs::read(inputs.join(format!("{name}.cpkt"))).unwrap();
        cases.push(Case {
            id: name.into(),
            artifact: verify_artifact(Arc::from(bytes), ArtifactLimits::default()).unwrap(),
            expected: format!("{}\n", fields[checksum]),
            samples: Vec::new(),
        });
    }
    assert!(!cases.is_empty(), "no cases selected");
    cases
}

fn record(case: &Case, engine: Engine, population: usize, sample: usize, result: &Trial) -> String {
    let mut fields = vec![
        case.id.clone(),
        engine.name().into(),
        population.to_string(),
        sample.to_string(),
        result.admit_ns.to_string(),
        optional_ns(result.admit_cpu),
        result.prepare_ns.to_string(),
        optional_ns(result.prepare_cpu),
        result.create_ns.to_string(),
        optional_ns(result.create_cpu),
        result.hot_ns.to_string(),
        optional_ns(result.hot_cpu),
        result
            .rss_kib
            .map_or_else(String::new, |rss| rss.to_string()),
    ];
    let storage = result.storage.map(|stats| {
        [
            stats.instructions,
            stats.eligible,
            stats.code_bytes,
            stats.index_bytes,
            stats.reference_enum_bytes,
        ]
    });
    for column in 0..5 {
        fields.push(storage.map_or_else(String::new, |values| values[column].to_string()));
    }
    for work in [result.create, result.hot] {
        fields.extend(work.0.map(|value| value.to_string()));
    }
    fields.join("\t") + "\n"
}

pub(super) fn main(args: &[String]) {
    assert!((5..=7).contains(&args.len()), "usage: guest-benchmark compare INPUT OUTPUT SAMPLES HEAP_BYTES POPULATION [ID_PREFIX|-] [reference,packed,packed-batch,packed-simd,offsets]");
    let inputs = Path::new(&args[0]);
    let outputs = Path::new(&args[1]);
    let samples: usize = args[2].parse().unwrap();
    assert!(samples >= 3 && samples % 2 == 1);
    let heap: u32 = args[3].parse().unwrap();
    assert!(heap >= 32 && heap.is_multiple_of(16));
    let population: usize = args[4].parse().unwrap();
    assert!((1..=4096).contains(&population));
    let prefix = args.get(5).map_or("-", String::as_str);
    let selection = args.get(6).map_or(
        "reference,packed,packed-batch,packed-simd,offsets",
        String::as_str,
    );
    let mut engines = Vec::new();
    for value in selection.split(',') {
        let engine = match value {
            "reference" => Engine::Reference,
            "packed" => Engine::Packed,
            "packed-batch" => Engine::PackedBatch,
            "packed-simd" => Engine::PackedSimd,
            "offsets" => Engine::Offsets,
            _ => panic!("unknown engine {value}"),
        };
        assert!(!engines
            .iter()
            .any(|previous: &Engine| previous.name() == engine.name()));
        engines.push(engine);
    }
    let cases = load(inputs, prefix);
    // Existing traced interpreter supplies an independent control for checksum and all eight counters.
    let controls: Vec<_> = cases.iter().map(|case| run(case, heap, true)).collect();
    let mut control = PerfControl::from_env();
    fs::create_dir_all(outputs).unwrap();
    fs::write(
        outputs.join("metadata.tsv"),
        format!(
            "key\tvalue\ninstance_decode_cache_bytes\t{}\nsimd_backend\t{}\n",
            execution::prototype::decode_cache_bytes(),
            if cfg!(target_arch = "x86_64") {
                "sse2"
            } else {
                "scalar-fallback"
            }
        ),
    )
    .unwrap();
    let mut raw = String::from("id\tengine\tpopulation\tsample\tadmit_ns\tadmit_cpu_ns\tprepare_ns\tprepare_cpu_ns\tcreate_ns\tcreate_cpu_ns\thot_ns\thot_cpu_ns\tprocess_rss_kib\tinstructions\teligible\tcode_bytes\tindex_bytes\treference_enum_bytes");
    for phase in ["create", "hot"] {
        for name in [
            "fixed",
            "dynamic",
            "maintenance",
            "blocks",
            "executed",
            "retired",
            "requests",
            "responses",
        ] {
            raw.push_str(&format!("\t{phase}_{name}"));
        }
    }
    raw.push('\n');
    for sample in 0..samples {
        for position in 0..cases.len() {
            let index = if sample % 2 == 0 {
                position
            } else {
                cases.len() - position - 1
            };
            let case = &cases[index];
            for engine_position in 0..engines.len() {
                let position = if sample % 2 == 0 {
                    engine_position
                } else {
                    engines.len() - engine_position - 1
                };
                let engine = engines[(position + sample / 2) % engines.len()];
                let result = trial(case, heap, engine, population, &mut control);
                let mut create = Work([0; 8]);
                let mut hot = Work([0; 8]);
                for _ in 0..population {
                    add_work(&mut create, controls[index].create);
                    add_work(&mut hot, controls[index].hot);
                }
                assert_eq!(
                    result.create, create,
                    "construction accounting for {}",
                    case.id
                );
                assert_eq!(result.hot, hot, "hot accounting for {}", case.id);
                raw.push_str(&record(case, engine, population, sample + 1, &result));
                fs::write(outputs.join("samples.tsv"), &raw).unwrap();
                eprintln!(
                    "sample {}/{} {} {} population={}",
                    sample + 1,
                    samples,
                    case.id,
                    engine.name(),
                    population
                );
            }
        }
    }
}
