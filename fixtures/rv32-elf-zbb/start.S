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

    .option norvc
    .section .text.start, "ax", @progbits
    .globl _start
    .type _start, @function
_start:
    .option push
    .option norelax
    la sp, __stack_top
    .option pop
    call zbb_main
1:
    j 1b
    .size _start, . - _start

    .section .text.zbb, "ax", @progbits
    .globl zbb_results
    .type zbb_results, @function
zbb_results:
    andn t0, a1, a2
    sw t0, 0(a0)
    orn t0, a1, a2
    sw t0, 4(a0)
    xnor t0, a1, a2
    sw t0, 8(a0)
    min t0, a1, a2
    sw t0, 12(a0)
    minu t0, a1, a2
    sw t0, 16(a0)
    max t0, a1, a2
    sw t0, 20(a0)
    maxu t0, a1, a2
    sw t0, 24(a0)
    clz t0, a1
    sw t0, 28(a0)
    ctz t0, a1
    sw t0, 32(a0)
    cpop t0, a1
    sw t0, 36(a0)
    sext.b t0, a1
    sw t0, 40(a0)
    sext.h t0, a1
    sw t0, 44(a0)
    zext.h t0, a1
    sw t0, 48(a0)
    rol t0, a1, a2
    sw t0, 52(a0)
    ror t0, a1, a2
    sw t0, 56(a0)
    rori t0, a1, 7
    sw t0, 60(a0)
    orc.b t0, a1
    sw t0, 64(a0)
    rev8 t0, a1
    sw t0, 68(a0)
    ret
    .size zbb_results, . - zbb_results
