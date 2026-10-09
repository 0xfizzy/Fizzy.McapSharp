using System.Runtime.CompilerServices;
using System.Threading.Tasks.Sources;
using Microsoft.Win32.SafeHandles;

namespace Fizzy.McapSharp;

/// <summary>Caller-buffer read outcome. On Success, Opcode identifies the record and Length is the body bytes copied; on BufferTooSmall, Length is the required capacity and the same record remains pending. EOF does not prove full-file integrity.</summary>
public readonly record struct McapRecordReadResult(McapReadStatus Status, byte Opcode, ulong Length);

/// <summary>One incremental asynchronous read session backed by the official Sans-I/O parser. Owns its Stream unless leaveOpen is true. Consume each ValueTask exactly once before another operation; I/O, parsing and cancellation failures terminate the session. Record and lease consumption cannot be mixed.</summary>
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
        McapSansIoReader? opened = null;
        try { parser = opened = McapSansIoReader.CreateLinear(options); input = new(parser); }
        catch (Exception operation)
        {
            Exception error = operation;
            try { opened?.Dispose(); }
            catch (Exception cleanup) { error = new AggregateException(error, cleanup); }
            if (opened is null || opened.NativeReleased)
            {
                try { bridge.Release(); }
                catch (Exception cleanup) { error = new AggregateException(error, cleanup); }
            }
            System.Runtime.ExceptionServices.ExceptionDispatchInfo.Capture(error).Throw();
            throw;
        }
        resources = new(parser, bridge);
        resume = Resume;
        // Inline completion avoids allocating a ThreadPool work item for every await.
        // A consumer may resume on the completing I/O thread; no context is imposed.
        completion.RunContinuationsAsynchronously = false;
        leaseCompletion.RunContinuationsAsynchronously = false;
    }
    /// <summary>Copies one record body into destination. Keep the memory valid and untouched until consuming the returned ValueTask exactly once. BufferTooSmall retains the pending record for a larger-buffer retry; other failures terminate the session.</summary>
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
    void Resume()
    {
        lock (gate)
        {
            if (consumptionMode == 2) DriveLease(true);
            else Drive(true);
        }
    }
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
                if (status != McapReadStatus.Success || e.Kind == McapReadEventKind.Record)
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
        lock (gate) { CheckDispose(); disposed = true; return resources.Transfer(); }
    }
    void CheckDispose() { ObjectDisposedException.ThrowIf(disposed, this); if (active) throw new InvalidOperationException("Complete and consume the outstanding operation before disposal."); }
    public void Dispose()
    {
        lock (gate) { if (disposed) return; CheckDispose(); disposed = true; resources.Dispose(); }
    }
    /// <summary>Releases resources synchronously, including Stream.Dispose when owned. The returned task is already complete; this does not call Stream.DisposeAsync.</summary>
    public ValueTask DisposeAsync() { Dispose(); return ValueTask.CompletedTask; }
}

internal sealed class AsyncResources : OwnedNativeHandle
{
    readonly McapSansIoReader parser;
    internal AsyncResources(McapSansIoReader parser, StreamBridge bridge) : base((IntPtr)1, bridge) { this.parser = parser; }
    protected override bool DependenciesReleased => parser.NativeReleased;
    protected override int ReleaseNative(IntPtr value, out Native.Result result) { result = default; parser.Dispose(); return 0; }
}
