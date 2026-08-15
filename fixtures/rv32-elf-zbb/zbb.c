/*
 * The Compukter Kraft Developers
 *
 * Copyright (C) 2026 Vsevolod Petrov (lazyhat)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

typedef unsigned int u32;

#define CONTROL_STATUS (*(volatile u32 *)0x10000000u)
#define CONTROL_PANIC_CODE (*(volatile u32 *)0x10000004u)
#define CONTROL_EXIT_CODE (*(volatile u32 *)0x10000008u)
#define DEBUG_WRITE (*(volatile unsigned char *)0x10000100u)

extern void zbb_results(u32 *results, u32 lhs, u32 rhs);

static const u32 expected[] = {
    0x80018080u, 0xfffffffau, 0x7ffe7f7au, 0x80018080u, 0x00000005u, 0x00000005u,
    0x80018080u, 0x00000000u, 0x00000007u, 0x00000004u, 0xffffff80u, 0xffff8080u,
    0x00008080u, 0x00301010u, 0x04000c04u, 0x01000301u, 0xffffffffu, 0x80800180u,
};
static const volatile unsigned char marker[] = "RV32 ELF ZBB OK\n";

static __attribute__((noreturn)) void panic(u32 code) {
    CONTROL_PANIC_CODE = code;
    CONTROL_STATUS = 4u;
    for (;;) {
    }
}

__attribute__((noreturn)) void zbb_main(void) {
    u32 results[sizeof(expected) / sizeof(expected[0])];
    zbb_results(results, 0x80018080u, 5u);
    for (u32 index = 0; index < sizeof(expected) / sizeof(expected[0]); ++index) {
        if (results[index] != expected[index]) {
            panic(index + 1u);
        }
    }
    for (u32 index = 0; index + 1u < sizeof(marker); ++index) {
        DEBUG_WRITE = (unsigned char)marker[index];
    }
    CONTROL_EXIT_CODE = 0u;
    CONTROL_STATUS = 3u;
    for (;;) {
    }
}
