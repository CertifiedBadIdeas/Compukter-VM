# host-runtime

- envelope length: 95
- record count: 5

| Record | Instruction | Offset | Opcode | Form | Length | Fixed cost |
|---:|---:|---:|---:|---:|---:|---:|
| 0 | 0 | 40 | `0x50` | 0 | 8 | 6 |
| 0 | 1 | 48 | `0xe3` | 0 | 6 | 1 |
| 1 | 0 | 54 | `0xe7` | 0 | 7 | 3 |
| 2 | 0 | 61 | `0xe8` | 0 | 9 | 4 |
| 3 | 0 | 70 | `0x51` | 0 | 9 | 5 |
| 3 | 1 | 79 | `0xe3` | 0 | 6 | 1 |
| 4 | 0 | 85 | `0xe9` | 0 | 10 | 6 |
