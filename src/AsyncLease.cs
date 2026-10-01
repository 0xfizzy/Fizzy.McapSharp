using System.Runtime.InteropServices;
using System.Threading.Tasks.Sources;

namespace Fizzy.McapSharp;

internal sealed class NativeStorageSignal
{
    static readonly System.Collections.Concurrent.ConcurrentDictionary<ulong,WeakReference<NativeStorageSignal>> Signals = new();
    static readonly Native.BudgetNotification Callback = Notify;
    readonly object gate = new();
    TaskCompletionSource? signal;
    internal void Register(MemoryBudgetHandle handle, ulong id)
    {
        Signals[id] = new(this);
        int status=Native.fm_budget_notify(handle, Callback, out var result);
        if(status<0) {Signals.TryRemove(id,out _);throw Native.ConsumeError(result);}
    }
    internal static void Remove(ulong id)=>Signals.TryRemove(id,out _);
    internal Task Observe() { lock(gate) return (signal ??= new(TaskCreationOptions.RunContinuationsAsynchronously)).Task; }
    static void Notify(ulong id)
    {
        if (!Signals.TryGetValue(id,out var weak) || !weak.TryGetTarget(out var target)) return;
        TaskCompletionSource? old;
        lock(target.gate) {old=target.signal;target.signal=null;}
        old?.TrySetResult();
    }
    internal static void Pulse()=>Native.fm_budget_dispatch();
}

public sealed partial class McapAsyncReader
{
    int consumptionMode;
    ManualResetValueTaskSourceCore<McapMessageBatchLease?> leaseCompletion;
    /// <summary>Reads stable messages directly from native parser storage. Waits for occupied
    /// storage to become available without holding the reader lock. Cancellation terminates this reader.</summary>
    public ValueTask<McapMessageBatchLease?> ReadBatchLeaseAsync(int maxMessages = 256,
        int targetPayloadBytes = 4 * 1024 * 1024, CancellationToken cancellationToken = default)
    {
        Native.CheckLeaseRequest(maxMessages, targetPayloadBytes);
        if (emitChunks) throw new InvalidOperationException("Message leases require EmitChunks to be disabled.");
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            if (failed) throw new InvalidOperationException("Reader failed; open a new reader.");
            if (active) throw new InvalidOperationException("Consume the outstanding operation first.");
            if (consumptionMode == 1) throw new InvalidOperationException("Record and message-lease consumption cannot be mixed on an async reader.");
            memoryBudget.EnableNotifications();
            parser.PrepareCapacityWait();
            consumptionMode = 2; active = true; leaseCompletion.Reset();
        }
        CompleteLease(maxMessages, targetPayloadBytes, cancellationToken);
        return new(this, leaseCompletion.Version);
    }
    async void CompleteLease(int count, int target, CancellationToken token)
    {
        try { var batch=await ReadLeaseCore(count,target,token).ConfigureAwait(false); lock(gate) leaseCompletion.SetResult(batch); }
        catch(Exception e) { lock(gate) { failed=true; leaseCompletion.SetException(e); } }
    }
    McapMessageBatchLease? IValueTaskSource<McapMessageBatchLease?>.GetResult(short token)
    {
        lock(gate) {
            if(leaseCompletion.GetStatus(token)==ValueTaskSourceStatus.Pending) throw new InvalidOperationException("Operation is not complete.");
            try {return leaseCompletion.GetResult(token);} finally {active=false;}
        }
    }
    ValueTaskSourceStatus IValueTaskSource<McapMessageBatchLease?>.GetStatus(short token)=>leaseCompletion.GetStatus(token);
    void IValueTaskSource<McapMessageBatchLease?>.OnCompleted(Action<object?> continuation,object? state,short token,ValueTaskSourceOnCompletedFlags flags)
        =>leaseCompletion.OnCompleted(continuation,state,token,flags);
    async ValueTask WaitForCapacity(CancellationToken token)
    {
        try
        {
            while (true)
            {
                token.ThrowIfCancellationRequested();
                var changed = memoryBudget.Signal.Observe();
                int status = parser.CapacityWaitStatus();
                if (status == 2) return;
                if (status != 1) throw new InvalidOperationException("Native capacity ticket is not registered.");
                await changed.WaitAsync(token).ConfigureAwait(false);
            }
        }
        finally { parser.CancelCapacityWait(); }
    }
    async ValueTask<McapMessageBatchLease?> ReadLeaseCore(int count, int target, CancellationToken token)
    {
        try
        {
            while (true)
            {
                token.ThrowIfCancellationRequested();
                var (status, batch, needed) = parser.LeaseStep(count, target);
                if (batch is not null) return batch;
                if (status == 1) return null;
                if (status == 4) { await WaitForCapacity(token).ConfigureAwait(false); continue; }
                int size = checked((int)Math.Min((ulong)inputBufferSize, needed));
                if (size == 0) throw new InvalidOperationException("Parser did not request input.");
                Memory<byte> memory;
                try { memory=input.Prepare(size); }
                catch(McapMemoryBudgetUnavailableException) { await WaitForCapacity(token).ConfigureAwait(false); continue; }
                int read = await stream.ReadAsync(memory, token).ConfigureAwait(false);
                // Consume completion before cancellation, preserving native-buffer lifetime.
                token.ThrowIfCancellationRequested();
                input.Complete(read);
            }
        }
        catch { lock (gate) failed = true; throw; }

    }
}
public sealed partial class McapSansIoReader
{
    internal void PrepareCapacityWait()
    {
        int status = Native.fm_engine_wait_prepare(handle, out var result);
        if (status < 0) throw Native.ConsumeError(result);
    }
    internal int CapacityWaitStatus()
    {
        int status = Native.fm_engine_wait_status(handle, false, out var result);
        if (status < 0) throw Native.ConsumeError(result);
        return status;
    }
    internal void CancelCapacityWait()
    {
        int status = Native.fm_engine_wait_status(handle, true, out var result);
        if (status < 0) throw Native.ConsumeError(result);
    }
    internal (int Status, McapMessageBatchLease? Batch, ulong Needed) LeaseStep(int count, int target)
    {
        int status = Native.fm_engine_lease_step(handle, (nuint)count, (nuint)target, out var p, out var e, out var result);
        if (status < 0) throw Native.ConsumeError(result);
        return (status, p == IntPtr.Zero ? null : new(p, checked((int)result.Value)), e.Length);
    }
}
internal static partial class Native
{
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_engine_wait_prepare(EngineHandle engine, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_engine_wait_status(EngineHandle engine, [MarshalAs(UnmanagedType.I1)] bool cancel, out Result result);
    [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int fm_engine_lease_step(EngineHandle engine, nuint count, nuint target, out IntPtr batch, out ReadEvent e, out Result result);
}
