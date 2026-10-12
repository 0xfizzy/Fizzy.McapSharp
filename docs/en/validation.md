# Validation and valid-prefix recovery

English | [简体中文](../zh-CN/validation.md)

For applications deciding whether a recording is complete and structurally valid, or extracting messages from an incomplete file. Start with the [reading guide](reading.md); this page explains what successful reading does and does not prove.

## Decide what must be checked

Ordinary high-level expanded reads parse record bodies and validate declaration IDs, conflicts and references. Optional CRC and trailing-byte checks are separate. Reaching EOF, fetching a summary or successfully querying an index does not establish full-file validation. Partial indexed access can leave most of a file unread. Raw, top-level and chunk-local capabilities have narrower documented scopes.

No container check verifies application payload meaning, schema conformance, clock synchronization or whether the producer recorded every intended message.

## Validate the entire recording

With `path` from the [writing guide](writing.md):

```csharp
var factory = new McapReaderFactory(path);
ulong records = factory.Validate();
Console.WriteLine($"Validated {records} records.");
```

This performs an independent strict full scan and returns a record count, not a message count. It checks framing, final magic, expanded record/declaration validity and present Chunk, Attachment, Data and Summary CRCs. A zero CRC indicates no checksum was supplied. A format or integrity failure throws; I/O failures also remain visible.

If an application already needs a sequential session configured strictly from its start, it can finish that session's checks:

```csharp
using var reader = factory.OpenMessages(options: McapReaderOptions.Strict);
reader.ValidateRemaining();
Console.WriteLine(reader.IsFullyValidated);
```

`ValidationCapability` describes the session's creation-time scope. Only `StrictFullScan` supports `ValidateRemaining` and session recovery; a partial indexed/sorted/chunk-local read or a non-strict full scan cannot retroactively gain skipped checks. `IsScanComplete` reports scan completion, while `IsFullyValidated` reports successful strict full validation. See [validation contracts](api.md#validation-and-recovery).

## Keep named checks distinct

`ValidateChunkCrcs` and `PrevalidateChunkCrcs` select chunk checksum checking and its timing; `ValidateDataSectionCrc` and `ValidateSummarySectionCrc` select their named sections. `SkipStartMagic`, `SkipEndMagic` and `CheckFinishesAfterEndMagic` concern framing and trailing bytes. `RecordLengthLimit` is a length restriction, not a total-memory budget. Consult [McapReaderOptions](xref:Fizzy.McapSharp.McapReaderOptions) for exact defaults and applicable readers.

The `Strict` preset combines integrity checks. Declaration-reference and record-body validation on high-level expanded reads do not depend on that preset. Disabling an optional CRC check does not disable those baseline checks. Writer checksum generation is a [separate choice](writer-options.md).

## Recover a valid message prefix

Use a separate factory operation on the incomplete input; do not reuse an already failed reader:

```csharp
var recovered = new List<McapMessage>();
var result = new McapReaderFactory(incompletePath).RecoverMessages(recovered.Add);
Console.WriteLine($"Recovered {result.RecoveredMessageCount} messages; validated: {result.IsFullyValidated}");
if (result.Error is { } error) Console.WriteLine(error.Message);
```

The factory opens a strict sequential reader and delivers independent owned messages up to a parse failure. Inspect both `IsFullyValidated` and `Error`; a delivered prefix is not proof that the source is complete. Corruption inside a chunk can prevent messages from that chunk being delivered. Bytes never written cannot be recovered. The list retains every delivered message; process each callback immediately instead when a collection is unnecessary.

Recovery does not modify or repair the source, append a footer, reset a session or resume a writer. To create a new recording from selected messages, explicitly use a new writer and its normal declaration/completion rules. I/O, binding and callback exceptions propagate instead of becoming a parse-error result. An application callback exception terminates the session and retains its original exception identity.

## Check the examples

The [validation examples](../../samples/Documentation/ValidationExamples.cs) create a complete unchunked recording, verify it, and truncate a separate copy immediately after its first complete top-level Message record. They assert one recovered message, an incomplete result and unchanged source bytes. This deterministic boundary avoids assuming a compressed chunk is partially recoverable. Run with the [sample instructions](usage.md#run-repository-examples).

For ordinary reading, return to the [reading guide](reading.md). For partial access, choose a [query](queries.md) and preserve its narrower validation claim.
