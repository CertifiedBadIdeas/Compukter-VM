/*
 * The Compukters Developers
 * Copyright 2026 Vsevolod Petrov (lazyhat)
 * Licensed under the Apache License, Version 2.0.
 */

use std::time::Duration;

/// One allowance per worker and tick, independent of the number of actors and
/// elapsed host waiting time. No clock is read outside an actual execution slice.
#[derive(Default)]
pub(super) struct WorkerFrameBudget {
    tick: Option<u64>,
    remaining: Duration,
}

impl WorkerFrameBudget {
    pub(super) fn grant(&mut self, tick: u64, allowance: Duration) -> bool {
        match self.tick {
            Some(current) if tick < current => false,
            Some(current) if tick == current => true,
            _ => {
                self.tick = Some(tick);
                self.remaining = allowance;
                true
            }
        }
    }

    pub(super) fn can_execute(&self, tick: u64) -> bool {
        self.tick == Some(tick) && !self.remaining.is_zero()
    }

    pub(super) fn charge(&mut self, tick: u64, elapsed: Duration) {
        if self.tick == Some(tick) {
            self.remaining = self.remaining.saturating_sub(elapsed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credit_is_shared_and_repeated_grants_do_not_refill_it() {
        let mut budget = WorkerFrameBudget::default();
        assert!(!budget.can_execute(1));
        assert!(budget.grant(1, Duration::from_millis(3)));
        budget.charge(1, Duration::from_millis(2));
        assert!(budget.grant(1, Duration::from_secs(10)));
        budget.charge(1, Duration::from_millis(1));
        assert!(!budget.can_execute(1));
    }

    #[test]
    fn rollover_replaces_credit_and_prevents_old_frames_borrowing_it() {
        let mut budget = WorkerFrameBudget::default();
        budget.grant(10, Duration::from_secs(5));
        assert!(budget.grant(11, Duration::from_millis(1)));
        assert!(!budget.grant(10, Duration::from_secs(10)));
        assert!(!budget.can_execute(10));
        budget.charge(10, Duration::from_secs(100));
        assert!(budget.can_execute(11));
        budget.charge(11, Duration::from_millis(2));
        assert!(!budget.can_execute(11));
    }
}
