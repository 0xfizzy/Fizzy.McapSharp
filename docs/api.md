# API and ownership

`McapWriter` creates a new file and never overwrites an existing path. Register schemas and channels before messages. Schema IDs and channel IDs are assigned by the official implementation; equivalent registrations may return an existing ID.

All writer calls are serialized. Data is copied from the input span and consumed before the call returns. Registration/write/flush/complete errors make the writer terminal. Dispose it and start another file. `Complete` flushes the MCAP summary/footer and syncs the file, and is idempotent after success. `Dispose` closes resources without implicitly completing a recording. The source buffer can be reused after a write returns.

`McapWriterOptions` selects None (default), Lz4 or Zstd compression, chunk size (4 MiB default), chunking, indexes and profile. All supported CRCs are emitted. Disabling indexes allows sequential reading. Timestamps are unsigned nanoseconds; the library does not infer a clock or convert application times.

`McapReader` opens a new independent reader per enumeration. Dispose an enumerator if stopping early; `foreach` does this automatically. The Windows file handle prevents writing/deleting while an enumeration is active. Payloads, schema data and metadata are copied into managed objects; returned records outlive the enumerator.

`ReadMessages` returns file/chunk order, not sorted timestamp order. Query StartTime is inclusive and EndTime exclusive. Indexed files use overlapping chunk indexes; files without usable indexes or with messages outside chunks use sequential filtering. `ReadSchemas` and `ReadChannels` include declarations even if no messages use them, deduplicated by ID. Metadata and attachments are separate enumerations.

Querying checks touched chunks and parsed records; it is **not** a full-file integrity check. `Validate` scans all records and verifies every present chunk, attachment, data and summary CRC, record framing and final magic. A zero CRC denotes an absent checksum under the MCAP specification and cannot provide integrity assurance. Normal reading rejects incomplete files. `RecoverMessages` explicitly delivers valid prefix messages to a callback and returns `McapRecoveryResult`; callers must check `IsComplete`. It stops at the first malformed record and does not invent records or skip corrupted chunks. Callback exceptions propagate.

Files must remain stable while reading. Windows sharing protects ordinary concurrent mutation; external writable mappings created before opening are outside this protection. Untrusted files are parsed by the pinned upstream MCAP crate with record lengths bounded to the mapped file size. Large legitimate records require correspondingly large managed allocations.

Only win-x64 native execution is supported in 0.1.0. Other platforms throw `PlatformNotSupportedException`. Missing native assets remain loader errors. File/MCAP/native operation failures are `McapException`; invalid managed state uses standard argument, disposed or invalid-operation exceptions.
