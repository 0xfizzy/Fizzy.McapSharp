using System.Runtime.InteropServices;
using System.Runtime.ExceptionServices;
using System.Runtime.CompilerServices;

namespace Fizzy.McapSharp;
// Root only the bridge, never its owner: abandoned owners can still finalize their SafeHandles.
internal sealed unsafe class StreamBridge
{
    static readonly ConditionalWeakTable<Stream, object> Active = new();
    static readonly Native.ReadCallback ReadFn = Read;
    static readonly Native.WriteCallback WriteFn = Write;
    static readonly Native.SeekCallback SeekFn = Seek;
    static readonly Native.FlushCallback FlushFn = Flush;
    readonly Stream stream;
    readonly bool leaveOpen;
    readonly long start;
    GCHandle root;
    internal Native.Callbacks Callbacks;
    internal ExceptionDispatchInfo? Error;
    internal bool InCallback;
    long position;
    internal StreamBridge(Stream stream, bool writing, bool leaveOpen)
    {
        ArgumentNullException.ThrowIfNull(stream);
        if (writing ? !stream.CanWrite : !stream.CanRead)
            throw new ArgumentException("Stream does not support the required operation.", nameof(stream));
        lock (Active)
        {
            if (Active.TryGetValue(stream, out _))
                throw new InvalidOperationException("Stream already has an active MCAP session.");
            Active.Add(stream, new object ());
        }

        this.stream = stream;
        this.leaveOpen = leaveOpen;
        try
        {
            start = stream.CanSeek ? stream.Position : 0;
            if (writing && stream.CanSeek && start != stream.Length)
                throw new ArgumentException("Writable Stream must be positioned at its end.", nameof(stream));
            root = GCHandle.Alloc(this);
            Callbacks = new()
            {
                Context = GCHandle.ToIntPtr(root),
                Read = Marshal.GetFunctionPointerForDelegate(ReadFn),
                Write = Marshal.GetFunctionPointerForDelegate(WriteFn),
                Seek = Marshal.GetFunctionPointerForDelegate(SeekFn),
                Flush = Marshal.GetFunctionPointerForDelegate(FlushFn),
                Seekable = stream.CanSeek ? 1u : 0u
            };
        }
        catch
        {
            lock (Active)
                Active.Remove(stream);
            if (root.IsAllocated)
                root.Free();
            throw;
        }
    }

    static StreamBridge Get(IntPtr ctx) => (StreamBridge)GCHandle.FromIntPtr(ctx).Target!;
    static int Read(IntPtr ctx, byte* dest, nuint length, nuint* count)
    {
        var b = Get(ctx);
        b.InCallback = true;
        try
        {
            var n = b.stream.Read(new Span<byte>(dest, checked((int)Math.Min(length, (nuint)int.MaxValue))));
            *count = (nuint)n;
            b.position = checked(b.position + n);
            return 0;
        }
        catch (Exception e)
        {
            b.Error ??= ExceptionDispatchInfo.Capture(e);
            return -1;
        }
        finally
        {
            b.InCallback = false;
        }
    }

    static int Write(IntPtr ctx, byte* src, nuint length)
    {
        var b = Get(ctx);
        b.InCallback = true;
        try
        {
            while (length != 0)
            {
                int n = (int)Math.Min(length, (nuint)int.MaxValue);
                b.stream.Write(new ReadOnlySpan<byte>(src, n));
                src += n;
                length -= (nuint)n;
                b.position = checked(b.position + n);
            }

            return 0;
        }
        catch (Exception e)
        {
            b.Error ??= ExceptionDispatchInfo.Capture(e);
            return -1;
        }
        finally
        {
            b.InCallback = false;
        }
    }

    static int Seek(IntPtr ctx, long offset, int origin, ulong* result)
    {
        var b = Get(ctx);
        b.InCallback = true;
        try
        {
            if (!b.stream.CanSeek)
            {
                if (origin != 1 || offset != 0)
                    throw new NotSupportedException("Stream is not seekable.");
                *result = checked((ulong)b.position);
                return 0;
            }

            var absolute = b.stream.Seek(origin == 0 ? checked(b.start + offset) : offset, (SeekOrigin)origin);
            b.position = checked(absolute - b.start);
            if (b.position < 0)
                throw new IOException("Seek before MCAP start.");
            *result = (ulong)b.position;
            return 0;
        }
        catch (Exception e)
        {
            b.Error ??= ExceptionDispatchInfo.Capture(e);
            return -1;
        }
        finally
        {
            b.InCallback = false;
        }
    }

    static int Flush(IntPtr ctx)
    {
        var b = Get(ctx);
        b.InCallback = true;
        try
        {
            b.stream.Flush();
            return 0;
        }
        catch (Exception e)
        {
            b.Error ??= ExceptionDispatchInfo.Capture(e);
            return -1;
        }
        finally
        {
            b.InCallback = false;
        }
    }

    internal void CheckReentry()
    {
        if (InCallback)
            throw new InvalidOperationException("An MCAP Stream callback cannot reenter its session.");
    }

    internal void ThrowIfError()
    {
        var e = Error;
        Error = null;
        e?.Throw();
    }

    internal void Release()
    {
        lock (Active)
            Active.Remove(stream);
        if (root.IsAllocated)
            root.Free();
        if (!leaveOpen)
        {
            try
            {
                stream.Dispose();
            }
            catch
            { /* SafeHandle finalization must not throw. */
            }
        }
    }
}
