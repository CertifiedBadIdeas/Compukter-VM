/*
 * The Compukter Kraft Developers
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

#include <stdint.h>

#include "kernel.h"

__attribute__((export_name("benchmark_batch"))) uint32_t
benchmark_batch(uint32_t iterations, uint32_t seed, uint32_t batch) {
    volatile uint32_t runtime_iterations = iterations;
    volatile uint32_t runtime_seed = seed;
    volatile uint32_t sink = 0u;

    for (uint32_t index = 0; index < batch; ++index) {
        sink = benchmark_kernel(runtime_iterations, runtime_seed);
        runtime_iterations = iterations;
        runtime_seed = seed;
    }

    return sink;
}
