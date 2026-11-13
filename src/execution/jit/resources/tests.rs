use super::*;
use std::sync::Barrier;

fn program(global: &Arc<Budget>, modules: usize, bytes: usize) -> Arc<ProgramBudget> {
    Arc::new(ProgramBudget {
        global: global.clone(),
        local: Budget::new(modules, bytes),
    })
}

#[test]
fn pending_and_published_pages_have_one_lifetime_owned_budget() {
    let global = Arc::new(Budget::new(4, MODULE_BYTES * 4));
    let program = program(&global, 4, MODULE_BYTES * 4);
    let mut first = Reservation::try_acquire(program.clone()).unwrap();
    assert_eq!(
        program.usage().0,
        Usage {
            modules: 1,
            bytes: MODULE_BYTES,
            pending: 1
        }
    );
    assert_eq!(
        Reservation::try_acquire(program.clone()).unwrap_err().scope,
        Scope::Program
    );
    first.settle(4096);
    assert_eq!(
        program.usage().0,
        Usage {
            modules: 1,
            bytes: 4096,
            pending: 0
        }
    );
    let second = Reservation::try_acquire(program.clone()).unwrap();
    assert_eq!(program.usage().0.bytes, 4096 + MODULE_BYTES);
    drop(second); // Queue rejection, cancelled owner and lowering failure use this path.
    assert_eq!(program.usage().0.bytes, 4096);
    let final_owner = Arc::new(first);
    let caller = final_owner.clone();
    drop(final_owner);
    assert_eq!(program.usage().0.modules, 1);
    drop(caller);
    assert_eq!(program.usage(), (Usage::default(), Usage::default()));
}

#[test]
fn small_modules_fill_actual_bytes_while_module_metadata_stays_bounded() {
    let global = Arc::new(Budget::new(6, MODULE_BYTES * 2));
    let program = program(&global, 6, MODULE_BYTES * 2);
    let mut modules = Vec::new();
    for _ in 0..6 {
        let mut reservation = Reservation::try_acquire(program.clone()).unwrap();
        reservation.settle(4096);
        modules.push(reservation);
    }
    assert_eq!(
        program.usage().0,
        Usage {
            modules: 6,
            bytes: 6 * 4096,
            pending: 0
        }
    );
    assert_eq!(
        Reservation::try_acquire(program.clone()).unwrap_err().scope,
        Scope::Program
    );
    modules.clear();
    assert_eq!(program.usage().0, Usage::default());
}

#[test]
fn independent_programs_cannot_monopolize_global_modules_or_bytes() {
    let global = Arc::new(Budget::new(4, MODULE_BYTES * 2));
    let a = program(&global, 1, MODULE_BYTES);
    let b = program(&global, 4, MODULE_BYTES * 2);
    let mut first = Reservation::try_acquire(a.clone()).unwrap();
    first.settle(4096);
    assert_eq!(
        Reservation::try_acquire(a.clone()).unwrap_err().scope,
        Scope::Program
    );
    let second = Reservation::try_acquire(b.clone()).unwrap();
    let c = program(&global, 4, MODULE_BYTES * 2);
    let denied = Reservation::try_acquire(c.clone()).unwrap_err();
    assert_eq!(denied.scope, Scope::Global);
    assert_eq!(c.epoch(Scope::Global), denied.epoch);
    drop(second);
    assert_ne!(c.epoch(Scope::Global), denied.epoch);
    assert!(Reservation::try_acquire(c).is_ok());
    drop(first);
    assert_eq!(*global.lock(), Usage::default());
}

#[test]
fn failed_and_panicking_compilers_release_in_flight_resources() {
    let global = Arc::new(Budget::new(1, MODULE_BYTES));
    let p = program(&global, 1, MODULE_BYTES);
    let result = std::panic::catch_unwind(|| {
        let _reservation = Reservation::try_acquire(p.clone()).unwrap();
        panic!("simulated compiler panic");
    });
    assert!(result.is_err());
    assert_eq!(p.usage(), (Usage::default(), Usage::default()));
    assert!(Reservation::try_acquire(p).is_ok());
}

#[test]
fn concurrent_admission_never_exceeds_global_or_program_envelopes() {
    let global = Arc::new(Budget::new(4, MODULE_BYTES * 3));
    let programs: Vec<_> = (0..8)
        .map(|_| program(&global, 2, MODULE_BYTES * 2))
        .collect();
    let barrier = Barrier::new(programs.len());
    std::thread::scope(|scope| {
        for p in &programs {
            let barrier = &barrier;
            scope.spawn(move || {
                for _ in 0..128 {
                    barrier.wait();
                    if let Ok(mut token) = Reservation::try_acquire(p.clone()) {
                        let (global, local) = p.usage();
                        assert!(global.modules <= 4 && global.bytes <= MODULE_BYTES * 3);
                        assert!(
                            local.modules <= 2
                                && local.bytes <= MODULE_BYTES * 2
                                && local.pending <= 1
                        );
                        token.settle(4096);
                        std::thread::yield_now();
                    }
                }
            });
        }
    });
    assert_eq!(*global.lock(), Usage::default());
    assert!(programs.iter().all(|p| *p.local.lock() == Usage::default()));
}

fn slot() -> Slot {
    Slot {
        key: AtomicUsize::new(0),
        progress: AtomicU64::new(COLD),
        native: OnceLock::new(),
        #[cfg(feature = "jit-admission-diagnostics")]
        first_request_ns: AtomicU64::new(0),
    }
}

#[test]
fn deferred_hot_code_retries_after_release_even_before_wait_is_published() {
    let global = Arc::new(Budget::new(1, MODULE_BYTES));
    let a = program(&global, 1, MODULE_BYTES);
    let b = program(&global, 1, MODULE_BYTES);
    let occupied = Reservation::try_acquire(a).unwrap();
    let denied = Reservation::try_acquire(b.clone()).unwrap_err();
    let waiting = slot();
    waiting.defer(denied);
    for _ in 0..HOT * 2 {
        assert!(!waiting.try_queue(&b, &QUEUE.epoch));
    }
    assert_eq!(waiting.status(), WAIT_GLOBAL);
    drop(occupied);
    assert!(!waiting.try_queue(&b, &QUEUE.epoch)); // Reheat after release; do not compile immediately.
    assert!((0..HOT).any(|_| waiting.try_queue(&b, &QUEUE.epoch)));

    let occupied = Reservation::try_acquire(b.clone()).unwrap();
    let denied = Reservation::try_acquire(b.clone()).unwrap_err();
    drop(occupied); // Release races ahead of publishing the deferred state.
    waiting.defer(denied);
    assert!(!waiting.try_queue(&b, &QUEUE.epoch));
    assert!((0..HOT).any(|_| waiting.try_queue(&b, &QUEUE.epoch)));
}

#[test]
fn full_queue_retries_after_one_dequeue_without_reheating() {
    let program = ProgramBudget::default();
    let slot = slot();
    let epoch = AtomicU32::new(0);
    let (sender, receiver) = sync_channel(1);
    sender.try_send(()).unwrap();
    assert!((0..HOT).any(|_| slot.try_queue(&program, &epoch)));
    let observed = epoch.load(Ordering::Acquire);
    assert!(matches!(
        sender.try_send(()),
        Err(std::sync::mpsc::TrySendError::Full(()))
    ));
    slot.retry_queue(observed);
    for _ in 0..16384 {
        assert!(!slot.try_queue(&program, &epoch));
    }
    receiver.recv().unwrap();
    epoch.fetch_add(1, Ordering::Release);
    assert!(slot.try_queue(&program, &epoch));
    assert!(!slot.try_queue(&program, &epoch));
    sender.try_send(()).unwrap();
}

#[test]
fn queue_progress_before_wait_publication_and_epoch_wrap_are_not_lost() {
    let program = ProgramBudget::default();
    for initial in [0, u32::MAX] {
        let slot = slot();
        let epoch = AtomicU32::new(initial);
        let observed = epoch.load(Ordering::Acquire);
        epoch.fetch_add(1, Ordering::Release);
        slot.retry_queue(observed);
        assert!(slot.try_queue(&program, &epoch));
    }
}

#[test]
fn concurrent_visitors_retry_a_released_queue_slot_only_once() {
    let program = ProgramBudget::default();
    let slot = slot();
    let epoch = AtomicU32::new(0);
    slot.retry_queue(0);
    epoch.store(1, Ordering::Release);
    let claims = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..128 {
                    if slot.try_queue(&program, &epoch) {
                        claims.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    assert_eq!(claims.load(Ordering::Relaxed), 1);
}

#[test]
fn concurrent_hot_visitors_claim_only_one_compilation() {
    let program = ProgramBudget::default();
    let slot = slot();
    let claims = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..128 {
                    if slot.try_queue(&program, &QUEUE.epoch) {
                        claims.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    assert_eq!(claims.load(Ordering::Relaxed), 1);
}

#[test]
fn global_metadata_and_local_bytes_are_independent_guards() {
    let global = Arc::new(Budget::new(2, MODULE_BYTES * 8));
    let a = program(&global, 8, MODULE_BYTES);
    let b = program(&global, 8, MODULE_BYTES * 8);
    let c = program(&global, 8, MODULE_BYTES * 8);
    let mut first = Reservation::try_acquire(a.clone()).unwrap();
    first.settle(MODULE_BYTES);
    assert_eq!(
        Reservation::try_acquire(a).unwrap_err().scope,
        Scope::Program
    );
    let mut second = Reservation::try_acquire(b).unwrap();
    second.settle(4096);
    assert_eq!(
        Reservation::try_acquire(c.clone()).unwrap_err().scope,
        Scope::Global
    );
    drop(first);
    assert!(Reservation::try_acquire(c).is_ok());
}

#[test]
fn release_epoch_wrap_does_not_strand_a_waiting_slot() {
    let global = Arc::new(Budget::new(1, MODULE_BYTES));
    global.epoch.store(u32::MAX, Ordering::Relaxed);
    let p = program(&global, 2, MODULE_BYTES * 2);
    let a = Reservation::try_acquire(program(&global, 1, MODULE_BYTES)).unwrap();
    let waiting = slot();
    waiting.defer(Reservation::try_acquire(p.clone()).unwrap_err());
    drop(a);
    assert_eq!(p.epoch(Scope::Global), 0);
    assert!(!waiting.try_queue(&p, &QUEUE.epoch));
    assert!((0..HOT).any(|_| waiting.try_queue(&p, &QUEUE.epoch)));
}

#[test]
fn queue_backpressure_is_shared_by_hot_entries_without_reserving_more_code() {
    let owner = JitState::default();
    let epoch = AtomicU32::new(7);
    owner.queue_blocked.store(8, Ordering::Release);
    for _ in 0..32 {
        let slot = slot();
        slot.progress
            .store(u64::from(HOT - 1) << 32, Ordering::Relaxed);
        assert!(slot.try_queue(&owner.budget, &epoch));
        assert!(owner.defer_for_queue(&slot, &epoch));
        assert_eq!(slot.status(), WAIT_QUEUE);
        assert_eq!(owner.budget.usage().1, Usage::default());
        epoch.fetch_add(1, Ordering::Release);
        assert!(slot.try_queue(&owner.budget, &epoch));
        assert!(!owner.defer_for_queue(&slot, &epoch));
        let reservation = Reservation::try_acquire(owner.budget.clone()).unwrap();
        assert_eq!(owner.budget.usage().1.pending, 1);
        drop(reservation);
        owner.queue_blocked.store(
            u64::from(epoch.load(Ordering::Acquire)) + 1,
            Ordering::Release,
        );
    }
}

#[test]
fn shared_queue_pressure_clears_after_early_progress_and_epoch_wrap() {
    let owner = JitState::default();
    for initial in [0, u32::MAX] {
        let epoch = AtomicU32::new(initial);
        epoch.fetch_add(1, Ordering::Release);
        owner
            .queue_blocked
            .store(u64::from(initial) + 1, Ordering::Release);
        let slot = slot();
        slot.retry_queue(initial);
        assert!(slot.try_queue(&owner.budget, &epoch));
        assert!(!owner.defer_for_queue(&slot, &epoch));
        assert_eq!(owner.queue_blocked.load(Ordering::Acquire), 0);
    }
}
