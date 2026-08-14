# RV32 DBT rotate peephole

Date: 2026-08-14  
Issue: #17  
Decision: **REJECT**

## Candidate

The experiment recognized this exact compiler-generated RV32I sequence:

```asm
sub  temporary, x0, shift
srl  temporary, value, temporary
sll  value, value, shift
addi shift, shift, 1
or   value, value, temporary
```

The x86-64 candidate preserved all three architectural results using `SHLD` and
`ROL`. Since an x86 double shift with a zero count leaves the destination
unchanged, the lowering also needed `AND` and `CMOVZ` to preserve `temporary`
when `shift % 32 == 0`.

Correctness tests covered shift counts 0, 1, 31, 32, and 63. Both benchmark
variants produced checksum `ee053d58`, retired the same number of guest
instructions as their baselines, and performed zero steady-state allocations.

## Cached DBT result

The focused same-process gate used 21 warm samples, a 1,024-kernel batch, 512
cache sets, a 128 KiB code cache, and 16 guest instructions per block.

| Variant | Median ns/kernel | p95 total ns | Emitted bytes | Delta |
|---|---:|---:|---:|---:|
| Baseline | 384,755.996 | 396,699,081 | 39,990 | — |
| Candidate | 392,684.455 | 408,284,334 | 39,981 | **+2.061%** |

Median lowering time increased from 107,340 ns to 116,869 ns (**+8.877%**).
The candidate saved only nine emitted bytes across the resident workload.

## Direct DBT control

The focused Direct DBT control also used 21 warm samples.

| Variant | Median ns/kernel | p95 total ns | Emitted bytes | Delta |
|---|---:|---:|---:|---:|
| Baseline | 474,932,046.000 | 483,380,572 | 253,072,769 | — |
| Candidate | 487,081,537.000 | 495,026,592 | 252,946,769 | **+2.558%** |

Median lowering time increased from 302,596,732 ns to 314,694,048 ns
(**+3.998%**).

## Conclusion

The fused form is correct but slower in both the product Cached DBT path and
the translation-heavy Direct DBT control. It introduces extra matcher work and
a dependency chain through `ECX` without removing enough host instructions or
code bytes to compensate. The peephole and its temporary product selector were
removed; the regular lowering remains the product implementation.

