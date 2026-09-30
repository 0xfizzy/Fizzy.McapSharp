using System.Runtime.CompilerServices;
using System.Threading.Tasks.Sources;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

public readonly record struct McapRecordReadResult(McapReadStatus Status, byte Opcode, ulong Length);

/// <summary>Incremental asynchronous record reader backed by the official Sans-I/O parser.</summary>
public sealed partial class McapAsyncReader : IDisposable, IAsyncDisposable, IValueTaskSource<McapRecordReadResult>, IValueTaskSource<McapMessageBatchLease?>
{
    readonly Stream stream;
    readonly StreamBridge bridge;
    readonly McapSansIoReader parser;
    readonly NativeInputMemory input;
    readonly int inputBufferSize;
    readonly bool emitChunks;
    readonly AsyncResources resources;
    readonly Action resume;
    readonly object gate = new();
    ManualResetValueTaskSourceCore<McapRecordReadResult> completion;
    ConfiguredValueTaskAwaitable<int>.ConfiguredValueTaskAwaiter awaiter;
    Memory<byte> destination;
    CancellationToken cancellation;
    bool active, failed, disposed;

    public McapAsyncReader(Stream stream, McapReaderOptions? options = null, bool leaveOpen = false, int inputBufferSize = 65536)
    {
        ArgumentNullException.ThrowIfNull(stream);
        if (inputBufferSize <= 0) throw new ArgumentOutOfRangeException(nameof(inputBufferSize));
        this.stream = stream;
        emitChunks = options?.EmitChunks ?? false;
        this.inputBufferSize = inputBufferSize;
        bridge = new(stream, false, leaveOpen);
        try { parser = McapSansIoReader.CreateLinear(options); input = new(parser); }
        catch { bridge.Release(); throw; }
        resources = new(parser, bridge);
        resume = Resume;
        // Inline completion avoids allocating a ThreadPool work item for every await.
        // A consumer may resume on the completing I/O thread; no context is imposed.
        completion.RunContinuationsAsynchronously = false;
    }
    public McapMemoryStatistics GetMemoryStatistics() { lock (gate) { ObjectDisposedException.ThrowIf(disposed, this); if (active) throw new InvalidOperationException("Consume the pending operation first."); return parser.GetMemoryStatistics(); } }
    public ValueTask<McapRecordReadResult> ReadNextRecordAsync(Memory<byte> destination, CancellationToken cancellationToken = default)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            if (failed) throw new InvalidOperationException("Reader failed; open a new reader.");
            if (active) throw new InvalidOperationException("Consume the outstanding operation before starting another.");
            if (consumptionMode == 2) throw new InvalidOperationException("Record and message-lease consumption cannot be mixed on an async reader.");
            consumptionMode = 1;
            active = true;
            this.destination = destination;
            cancellation = cancellationToken;
            completion.Reset();
            Drive(false);
            return new(this, completion.Version);
        }
    }
    void Resume() { lock (gate) Drive(true); }
    void Drive(bool resumed)
    {
        try
        {
            // Consume the Stream's ValueTask even when cancellation won the race.
            // Some reusable I/O sources release their operation slot in GetResult.
            if (resumed)
            {
                int count = awaiter.GetResult();
                cancellation.ThrowIfCancellationRequested();
                input.Complete(count);
            }
            while (true)
            {
                cancellation.ThrowIfCancellationRequested();
                var status = parser.NextEvent(destination.Span, out var e);
                if (status != McapReadStatus.Message || e.Kind == McapReadEventKind.Record)
                {
                    destination = default;
                    completion.SetResult(new(status, e.Opcode, e.Length));
                    return;
                }
                if (e.Kind != McapReadEventKind.Read) throw new InvalidOperationException("Unexpected parser event.");
                awaiter = stream.ReadAsync(input.Prepare((int)Math.Min((ulong)inputBufferSize, e.Length)), cancellation).ConfigureAwait(false).GetAwaiter();
                if (!awaiter.IsCompleted) { awaiter.UnsafeOnCompleted(resume); return; }
                input.Complete(awaiter.GetResult());
            }
        }
        catch (Exception ex) { failed = true; destination = default; completion.SetException(ex); }
    }
    McapRecordReadResult IValueTaskSource<McapRecordReadResult>.GetResult(short token)
    {
        lock (gate)
        {
            // Validate token and completion before releasing the operation slot.
            if (completion.GetStatus(token) == ValueTaskSourceStatus.Pending) throw new InvalidOperationException("Operation is not complete.");
            try { return completion.GetResult(token); }
            finally { active = false; }
        }
    }
    ValueTaskSourceStatus IValueTaskSource<McapRecordReadResult>.GetStatus(short token) => completion.GetStatus(token);
    void IValueTaskSource<McapRecordReadResult>.OnCompleted(Action<object?> continuation, object? state, short token, ValueTaskSourceOnCompletedFlags flags) => completion.OnCompleted(continuation, state, token, flags);
    public Stream IntoInner()
    {
        lock (gate) { CheckDispose(); bridge.Detach(); Dispose(); return stream; }
    }
    void CheckDispose() { ObjectDisposedException.ThrowIf(disposed, this); if (active) throw new InvalidOperationException("Complete and consume the outstanding operation before disposal."); }
    public void Dispose()
    {
        lock (gate) { if (disposed) return; CheckDispose(); disposed = true; resources.Dispose(); }
    }
    public ValueTask DisposeAsync() { Dispose(); return ValueTask.CompletedTask; }
}

internal sealed class AsyncResources : SafeHandleZeroOrMinusOneIsInvalid
{
    readonly McapSansIoReader parser;
    readonly StreamBridge bridge;
    internal AsyncResources(McapSansIoReader parser, StreamBridge bridge) : base(true) { this.parser = parser; this.bridge = bridge; SetHandle((IntPtr)1); }
    protected override bool ReleaseHandle() { parser.Dispose(); bridge.Release(); return true; }
}
