using Andruna.Manager.Core.Sysd;

namespace Andruna.Manager.Core.Events;

/// <summary>Receives the events of the sysd event stream, in order.</summary>
public interface IEventSink
{
    void OnEvent(SysdEvent e);
}

/// <summary>Why <see cref="EventPump.RunAsync"/> returned.</summary>
public enum EventPumpEnd
{
    /// <summary>The token was cancelled.</summary>
    Cancelled,

    /// <summary>sysd is not running (and the caller did not ask to wait for it).</summary>
    ServiceGone,

    /// <summary>The running sysd does not know the event stream.</summary>
    Unsupported,

    /// <summary>The pipe belongs to another user; nothing is trusted from it.</summary>
    Untrusted,
}

/// <summary>
/// Follows the sysd event stream (WaitEvents, 0x0010) on a connection of
/// its own: a long poll that returns as soon as an event arrives. It starts
/// at the current position, so events from before the manager started are
/// not shown again; after sysd restarts (a new epoch) it delivers what the
/// new instance has kept. The connection is passive and does not keep sysd
/// running.
/// </summary>
public sealed class EventPump(ISysdClient client, IEventSink sink, TimeSpan? retryDelay = null, uint waitMs = 25_000)
{
    private readonly TimeSpan _retryDelay = retryDelay ?? TimeSpan.FromSeconds(3);
    private ulong _epoch;
    private ulong _after = ulong.MaxValue;

    /// <summary>Events delivered so far.</summary>
    public int Delivered { get; private set; }

    /// <summary>
    /// Runs until cancelled. When sysd is not reachable, <paramref name="waitForService"/>
    /// decides whether to keep trying (a manager window is open) or to stop
    /// (a background manager has nothing to do without sysd).
    /// </summary>
    public async Task<EventPumpEnd> RunAsync(Func<bool> waitForService, CancellationToken ct)
    {
        while (!ct.IsCancellationRequested)
        {
            SysdResult<SysdEventBatch> r;
            try
            {
                r = await client.WaitEventsAsync(_epoch, _after, _after == ulong.MaxValue ? 0 : waitMs, ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                return EventPumpEnd.Cancelled;
            }

            if (r.IsOk)
            {
                var batch = r.Value!;
                foreach (var e in batch.Events)
                {
                    // A batch of the same instance never repeats what was seen.
                    if (batch.Epoch == _epoch && _after != ulong.MaxValue && e.Seq <= _after)
                    {
                        continue;
                    }

                    Delivered++;
                    sink.OnEvent(e);
                }

                _epoch = batch.Epoch;
                _after = batch.Next;
                continue;
            }

            if (r.IsUnsupported)
            {
                return EventPumpEnd.Unsupported;
            }

            if (r.Outcome == SysdOutcome.WrongServerUser)
            {
                return EventPumpEnd.Untrusted;
            }

            if (r.Outcome == SysdOutcome.NotRunning && !waitForService())
            {
                return EventPumpEnd.ServiceGone;
            }

            try
            {
                await Task.Delay(_retryDelay, ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                return EventPumpEnd.Cancelled;
            }
        }

        return EventPumpEnd.Cancelled;
    }
}
