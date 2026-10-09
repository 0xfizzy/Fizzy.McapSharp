using System.Runtime.InteropServices;
using System.Threading.Tasks.Sources;

namespace Fizzy.McapSharp;

public sealed partial class McapAsyncReader
{
    int consumptionMode;
    int leaseCount, leaseTarget;
    ManualResetValueTaskSourceCore<McapMessageBatchLease?> leaseCompletion;
    /// <summary>Reads stable messages directly from native parser storage. Cancellation terminates this reader.
    /// Returns a stable shared batch, or null at EOF. Failure publishes no partial lease. Consume the ValueTask exactly once before starting another operation. Retain the lease throughout access and never dispose it concurrently.</summary>
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
            consumptionMode = 2;
            active = true;
            leaseCount = maxMessages;
            leaseTarget = targetPayloadBytes;
            cancellation = cancellationToken;
            leaseCompletion.Reset();
            DriveLease(false);
            return new(this, leaseCompletion.Version);
        }
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
    void DriveLease(bool resumed)
    {
        try
        {
            if (resumed)
            {
                // Consume I/O completion before checking cancellation or releasing parser storage.
                int read = awaiter.GetResult();
                cancellation.ThrowIfCancellationRequested();
                input.Complete(read);
            }
            while (true)
            {
                cancellation.ThrowIfCancellationRequested();
                var (status, batch, needed) = parser.LeaseStep(leaseCount, leaseTarget);
                if (batch is not null || status == Protocol.Status.End)
                {
                    leaseCompletion.SetResult(batch);
                    return;
                }
                int size = checked((int)Math.Min((ulong)inputBufferSize, needed));
                if (size == 0) throw new InvalidOperationException("Parser did not request input.");
                awaiter = stream.ReadAsync(input.Prepare(size), cancellation).ConfigureAwait(false).GetAwaiter();
                if (!awaiter.IsCompleted) { awaiter.UnsafeOnCompleted(resume); return; }
                int read = awaiter.GetResult();
                cancellation.ThrowIfCancellationRequested();
                input.Complete(read);
            }
        }
        catch (Exception error) { failed = true; leaseCompletion.SetException(error); }
    }
}
