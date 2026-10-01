# Local patch boundaries

English | [简体中文](patches.zh-CN.md)

Maintainers should prefer unmodified upstream APIs. A patch is permitted only when a documented binding ownership or performance contract cannot be met by upstream APIs or binding-only adaptation. Preserve format state machines and semantics; do not patch codec allocators, add total-memory accounting, or optimize upstream internals. Keep patches local; do not submit them upstream.

## Permitted extensions

| Surface | Upstream limitation and rejected alternative | Local contract |
| --- | --- | --- |
| storage.rs, linear_reader.rs, indexed_reader.rs; lib.rs export | Borrowed events expire on advancement. Copying each leased message or retry body adds binding payload copies; keeping the parser alive alone does not prevent buffer reuse. | Shared immutable storage ranges retain owners across advancement, parser disposal and cache eviction. Published ranges are never overwritten; retained storage forces relocation before overlapping writes. Shared indexed input transfers an existing buffer or mapping and rejects uncompressed input whose length disagrees with the declared decompressed length. Direct-fill input exposes only writable unpublished bytes. |
| write.rs: contains_channel | Batch preflight needs to reject unknown channels before any write. A binding-side channel registry duplicates upstream declaration state, including automatic declarations. | Read-only membership query; upstream validates and writes records as before. |
| write.rs: Drop after into_inner | Upstream Drop calls finish even after output extraction, conflicting with explicit Complete and disposal without completion. A dummy output still executes unnecessary format finalization. | Skip Drop finalization when output has been extracted. Ordinary upstream Drop with output present remains unchanged. The binding extracts output when disposing without Complete. |

No local patch changes CRC rules, record interpretation, compression algorithms, index ordering or writer defaults. The wrapper's optional complete-chunk cache validates the entire target chunk, which can report tail corruption earlier than an upstream prefix seek. This is a binding behavior, not an upstream guarantee.

## Evidence and maintenance

`UPSTREAM.json` preserves hashes of the official crate files; `PATCHES.json` lists only local differences. `scripts/check_vendor.py` rejects undeclared differences. The API inventory labels official declarations separately from local extensions.

`python scripts/test_upstream.py` compiles a registry-only reference independently of the patched crate and compares deterministic writer output, records, messages, indexed seeks, every truncation of bounded fixtures and sampled corruption under None/Lz4/Zstd. These checks are finite evidence, not proof of all format behavior. Native shared-storage tests assert pointer identity, retained lifetime and allocation-free pending delivery; managed tests cover disposal, mapped storage, retries and asynchronous ownership. The mandatory Release allocation gate verifies warmed managed hot paths separately.

For each patch change, review its necessity and binding-only alternatives, update this contract and both manifests as appropriate (never rewrite upstream hashes to accept a patch), and rerun independent behavior, lifetime and allocation checks. New convenience APIs or upstream resource policies do not justify expanding the patch.
