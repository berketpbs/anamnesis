//! Stopping, and saying why.
//!
//! Pulled out of `lib.rs` because it is a subject of its own: what asked the
//! server to stop, how long the operating system will let it take, and what
//! happens to a summary that was still being written. The knowledge here was
//! bought rather than designed — this repository ran for weeks with a server
//! that had never once logged a clean shutdown, because on Windows only Ctrl-C
//! was being listened for and the console close, logoff and shutdown events
//! killed it silently.

use tokio_util::task::TaskTracker;

/// How long a stopping server waits for summaries already being written.
///
/// Not the model's timeout, which is 90 seconds by default: `docker stop`
/// sends SIGKILL 10 seconds after SIGTERM, so a longer wait would mostly be a
/// promise the container runtime breaks. Fifteen seconds covers a model that
/// is nearly done and an operator's Ctrl-C, and says plainly what it gave up
/// on when it does.
pub(crate) const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(15);

/// The same wait, when the operating system is holding the stopwatch.
///
/// Windows gives a console process about five seconds to handle the close,
/// logoff and shutdown events and then kills it whatever it is doing. Waiting
/// the usual fifteen there would not be generous, it would be a promise
/// Windows breaks: the process dies mid-wait and the line naming the summaries
/// it abandoned — the reason for waiting at all — is never written. Four
/// seconds leaves room to write it.
#[cfg(windows)]
pub(crate) const OS_DEADLINE_GRACE: std::time::Duration = std::time::Duration::from_secs(4);

/// Why the server is stopping, and how long it has left to finish up.
///
/// The two travel together because they are not independent: a signal that
/// arrives with an operating-system deadline attached cannot be given the same
/// grace as one that does not.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Stop {
    /// What asked the server to stop.
    ///
    /// It is logged because for a process nobody is watching this is the only
    /// account of why it went away, and the absence of that account is what
    /// made this repository's own four-day silence so hard to explain: that
    /// the server was gone could be seen, that it had been killed when a
    /// console closed could not.
    pub(crate) cause: &'static str,
    /// How long work already in flight may take before it is abandoned.
    pub(crate) grace: std::time::Duration,
}

impl Stop {
    /// What to assume when `serve` returns for a reason that was not a signal.
    ///
    /// Reachable only if the shutdown future is dropped without resolving, so
    /// it says what it knows — nothing — rather than naming a signal that
    /// never arrived.
    pub(crate) const UNKNOWN: Self = Self {
        cause: "the listener stopped",
        grace: SHUTDOWN_GRACE,
    };
}

/// Resolve when the operating system asks this process to stop.
///
/// Every way it can be asked, because they arrive from different places and
/// mean the same thing here: Ctrl-C from a terminal, SIGTERM from `docker
/// stop`, systemd or a supervisor, and on Windows the console window being
/// closed, the session logging off, or the machine shutting down.
///
/// Windows is not a footnote. `ctrl_c` covers CTRL_C_EVENT and nothing else,
/// so until the console events were registered a server whose window was
/// closed took the abrupt path — which is precisely how this project's own
/// memory has died, on the one platform where closing the window is how people
/// stop things.
pub(crate) async fn stopped() -> Stop {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
        Stop {
            cause: "interrupted",
            grace: SHUTDOWN_GRACE,
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
                Stop {
                    cause: "asked to terminate",
                    grace: SHUTDOWN_GRACE,
                }
            }
            // A process that cannot register the handler still stops on
            // Ctrl-C; refusing to serve over it would be worse.
            Err(error) => {
                tracing::warn!(%error, "could not listen for SIGTERM");
                std::future::pending::<Stop>().await
            }
        }
    };

    #[cfg(windows)]
    let terminate = async {
        // All three, because they are one event with three names and one
        // deadline. A server that handled only the window closing would still
        // go silently every time the machine restarts.
        let registered = (|| {
            Ok::<_, std::io::Error>((
                tokio::signal::windows::ctrl_close()?,
                tokio::signal::windows::ctrl_logoff()?,
                tokio::signal::windows::ctrl_shutdown()?,
            ))
        })();

        let (mut close, mut logoff, mut shutdown) = match registered {
            Ok(signals) => signals,
            // Same reasoning as SIGTERM above: it still stops on Ctrl-C.
            Err(error) => {
                tracing::warn!(%error, "could not listen for the console events");
                return std::future::pending::<Stop>().await;
            }
        };

        let cause = tokio::select! {
            _ = close.recv() => "the console was closed",
            _ = logoff.recv() => "the session logged off",
            _ = shutdown.recv() => "the system is shutting down",
        };
        Stop {
            cause,
            grace: OS_DEADLINE_GRACE,
        }
    };

    #[cfg(not(any(unix, windows)))]
    let terminate = std::future::pending::<Stop>();

    tokio::select! {
        stop = interrupt => stop,
        stop = terminate => stop,
    }
}

/// Wait for tracked work, up to `grace`. Returns whether it all finished.
///
/// Separate from [`crate::serve`] because the interesting half is what happens when
/// the wait runs out, and a signal is a poor thing to write a test around.
pub(crate) async fn finish_in_flight(tasks: &TaskTracker, grace: std::time::Duration) -> bool {
    tasks.close();
    if tasks.is_empty() {
        return true;
    }

    tracing::info!(
        summaries = tasks.len(),
        "waiting for sessions still being summarised"
    );
    match tokio::time::timeout(grace, tasks.wait()).await {
        Ok(()) => true,
        Err(_) => {
            // Named, because the alternative is a session that ended and left
            // no page with nothing anywhere saying why. The observations are
            // in the index and the raw spool; only the prose is lost.
            tracing::warn!(
                summaries = tasks.len(),
                seconds = grace.as_secs(),
                "gave up waiting; these sessions end without a summary, though their transcripts are kept"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model that has hung must not turn a stop into a hang. The transcript
    /// survives; the summary is the part that is lost, and the log says so.
    #[tokio::test]
    async fn work_that_will_not_finish_does_not_hold_the_shutdown_open() {
        let tasks = TaskTracker::new();
        tasks.spawn(async {
            std::future::pending::<()>().await;
        });

        let finished = finish_in_flight(&tasks, std::time::Duration::from_millis(50)).await;

        assert!(!finished);
    }

    #[tokio::test]
    async fn a_server_with_nothing_in_flight_stops_at_once() {
        let tasks = TaskTracker::new();

        assert!(finish_in_flight(&tasks, std::time::Duration::from_secs(30)).await);
    }
}
