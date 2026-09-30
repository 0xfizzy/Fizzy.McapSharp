# Native allocation inventory

English | [简体中文](memory-accounting.zh-CN.md)

This inventory is for maintainers auditing the native memory ceiling. A reservation is not evidence of a live allocation. An open item below is unfinished accounting work, not an approved exclusion from the budget.

| Storage | Category | Allocation / release | Evidence and remaining work |
| --- | --- | --- | --- |
| Codec allocator state | Encoder / decoder | `ChargedBox::new` / `ChargedBox::drop` | Exact explicit Layout, including reservation storage; refusal, panic cleanup and codec failure matrices |
| Codec C workspace | Encoder / decoder | custom alloc/calloc / custom free | Header and workspace reserved together; encoder threads 0/1/2 and decoder failure injection; independent identity-level probe reconciliation remains open |
| Codec output | Writer | `charged::bytes` / Vec followed by Reservation drop | Exact requested byte capacity; allocator refusal rolls back |
| Buffered writer chunks | Writer | `charged::bytes` / replacement or disposal | Geometric growth; old and new storage charged concurrently |
| Owned input copy | Input | `charged::bytes` / backing disposal | Payload exact; outer shared-owner control block remains open |
| Caller-buffer pending/scratch | Scratch | `Delivery::reserve_resource` / discard | Replacement transaction charges both allocations; final delivery and copy-counter audit remains open |
| Index and descriptor pages | Index / descriptor | segmented page allocation / tree disposal | Bounded radix directory; nested strings/maps and allocation-identity probe remain open |
| Shared index root | Index | lazy `ChargedShared::new` / last owner drop | Explicit reference-count/control layout; concurrent owner test |
| Cache entry directories | Scratch | segmented page allocation / cache disposal | Bounded pages; cached object control blocks and ownership eligibility remain open |
| Domain reclaimer registry | Scratch | fixed registry page / pruning or domain disposal | Weak reservations; cross-page and no-reference-cycle test |
| Storage pool and domain roots | Input / decompressed / scratch | storage allocation / pooling or disposal | Payload accounted; pool directory, shared headers and domain bootstrap still require exact accounting |
| Declarations, nested indexes, prepared controls | Declaration / index | record/JSON construction / owner disposal | Conservative reservations or missing control accounting; paged typed representation and streaming parsing remain open |
| ABI result and error construction | Scratch | response creation / buffer free | Fixed-capacity budget-failure response and accounted result materialization remain open |

## Codec failure contract

The pinned zstd-sys source preserves the configured worker count. Its reviewed patches reject null custom allocations before clearing memory, preserve custom allocator identity during partial pool initialization, and avoid traversing a missing job table during cleanup. Patch fingerprints include these C sources and the build-script dependency on the source directory. Both the production crate and standalone MCAP tests use this same patched codec version.

Allocation failure inside a codec is terminal. The callback never waits for a consumer or invokes managed code. Tests reject each observed allocation attempt and verify domain occupancy returns to zero after context teardown. System allocator failure and domain refusal are separate errors; their final structured ABI representation remains unfinished.

## Acceptance boundary

The current inventory does not establish a complete heap ceiling. Finish every open accounting item, domain wait tickets and complete ownership statistics before removing that limitation. OS thread stacks/runtime resources, allocator fragmentation, managed/caller buffers and mapped resident pages remain separate from the controlled heap budget.
