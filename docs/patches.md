# Local patch boundaries

English | [简体中文](zh-CN/patches.md)

Maintainers should prefer unmodified upstream APIs. A patch is permitted only when a documented binding ownership or performance contract cannot be met by upstream APIs or reasonable binding-only adaptation. Preserve format state machines and semantics; do not patch codec allocators, add total-memory accounting, or optimize upstream internals. Keep patches local; do not submit them upstream.

When adaptation requires disproportionate duplicated state or lifetime machinery, compare it with the smallest necessary access or storage ownership extension. Document the contract, the rejected adaptation and its concrete complexity, and the correctness, maintenance and upstream-update costs of both options. Choose the simpler maintainable solution within these boundaries; complexity is not permission for convenience APIs or unrelated upstream changes. Independent behavior, lifetime and performance evidence remains required.

## Permitted extensions

| Surface | Upstream limitation and rejected alternative | Local contract |
| --- | --- | --- |
| storage.rs, linear_reader.rs, indexed_reader.rs; lib.rs export | Borrowed events expire on advancement. Copying each leased message or retry body adds binding payload copies; keeping the parser alive alone does not prevent buffer reuse. | Shared immutable storage ranges retain owners across advancement, parser disposal and cache eviction. Published ranges are never overwritten; retained storage forces relocation before overlapping writes. Shared indexed input transfers an existing buffer or mapping and rejects uncompressed input whose length disagrees with the declared decompressed length. Direct-fill input exposes only writable unpublished bytes. |
| storage.rs: shares_backing | Shared slices expose capacity but not backing identity. Payload pointers can differ within one allocation, and equal capacities do not establish shared ownership. Binding-only grouping would have to guess identity or copy dense selections too. | Read-only Arc identity comparison, used to group consecutive owned sort ranges before publication. No addresses cross the C ABI. The binding chooses whether to compact; upstream state machines and allocation policy are unchanged. |
| write.rs: contains_channel | Batch preflight needs to reject unknown channels before any write. A binding-side channel registry duplicates upstream declaration state, including automatic declarations. | Read-only membership query; upstream validates and writes records as before. |
| write.rs: Drop after into_inner | Upstream Drop calls finish even after output extraction, conflicting with explicit Complete and disposal without completion. A dummy output still executes unnecessary format finalization. | Skip Drop finalization when output has been extracted. Ordinary upstream Drop with output present remains unchanged. The binding extracts output when disposing without Complete. |

No local patch changes CRC rules, record interpretation, compression algorithms, index ordering or writer defaults. The wrapper's optional complete-chunk cache validates the entire target chunk, which can report tail corruption earlier than an upstream prefix seek. This is a binding behavior, not an upstream guarantee.

## Evidence and maintenance

`UPSTREAM.json` preserves hashes of the official crate files; `PATCHES.json` lists only local differences. `scripts/check_vendor.py` rejects undeclared differences. The API inventory labels official declarations separately from local extensions.

`python scripts/test_upstream.py` compiles a registry-only reference independently of the patched crate and compares deterministic writer output, records, messages, indexed seeks, every truncation of bounded fixtures and sampled corruption under None/Lz4/Zstd. These checks are finite evidence, not proof of all format behavior. Native shared-storage tests assert pointer identity, retained lifetime and allocation-free pending delivery; managed tests cover disposal, mapped storage, retries and asynchronous ownership. The mandatory Release allocation gate verifies warmed managed hot paths separately.

`sort_arena` tests check backing identity, compaction thresholds, exact-once selected-payload copying, segment boundaries and retained results after disposal. `memory_probe::sort_compaction_profile` compares shared and compact policies on identical inputs, verifies released backing allocations and reports retained capacity, copy bytes and old/new group overlap. `SortStorageTests` covers filtered fallback sorting, ties, reverse order, retries and lease lifetimes across compression and input modes. These tests establish binding storage behavior; the independent upstream comparison remains the format reference.

For each patch change, review its necessity and binding-only alternatives, update this contract and both manifests as appropriate (never rewrite upstream hashes to accept a patch), and rerun independent behavior, lifetime and allocation checks. New convenience APIs or upstream resource policies do not justify expanding the patch.
