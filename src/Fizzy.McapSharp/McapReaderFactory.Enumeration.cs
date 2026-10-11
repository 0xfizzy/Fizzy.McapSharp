namespace Fizzy.McapSharp;

public sealed partial class McapReaderFactory
{
    // The factory owns both enumeration and input cleanup, including early termination.
    static IEnumerable<T> EnumerateAndDispose<T>(IDisposable owner, IEnumerable<T> sequence)
    {
        IEnumerator<T>? iterator = null;
        Exception? operation = null;
        try
        {
            try { iterator = sequence.GetEnumerator(); }
            catch (Exception error) { operation = error; throw; }
            while (true)
            {
                bool more;
                T item;
                try
                {
                    more = iterator.MoveNext();
                    item = more ? iterator.Current : default!;
                }
                catch (Exception error) { operation = error; throw; }
                if (!more) yield break;
                yield return item;
            }
        }
        finally
        {
            Exception? cleanup = null;
            try { iterator?.Dispose(); }
            catch (Exception error) { cleanup = error; }
            try { owner.Dispose(); }
            catch (Exception error)
            {
                cleanup = cleanup is null ? error : new AggregateException(cleanup, error);
            }
            if (cleanup is not null)
            {
                if (operation is not null) throw new AggregateException(operation, cleanup);
                System.Runtime.ExceptionServices.ExceptionDispatchInfo.Capture(cleanup).Throw();
            }
        }
    }
}
