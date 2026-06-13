//! `pactl subscribe` event source for the reconciler (daemon-design.md §6).
use crate::sys::process::ProcessSpawner;
use std::io;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

/// A graph change relevant to the audio pipeline's desired state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphEvent {
    /// A sink was added/changed/removed (our null sink may be affected).
    Sink,
    /// Server change (the default sink may have changed).
    Server,
}

/// Parse one `pactl subscribe` line, e.g. `Event 'change' on sink #42` ->
/// `Sink`, `Event 'change' on server` -> `Server`. Irrelevant facilities
/// (sink-input, source, client, card, module, …) and unparseable lines -> None.
pub fn parse_pactl_event(line: &str) -> Option<GraphEvent> {
    let rest = line.trim().strip_prefix("Event ")?;
    let target = rest.split_once(" on ")?.1.trim();
    if target == "server" {
        Some(GraphEvent::Server)
    } else if target == "sink" || target.starts_with("sink #") {
        Some(GraphEvent::Sink)
    } else {
        None
    }
}

/// Supervised event source: run `pactl subscribe`, parse its stdout, and forward
/// relevant `GraphEvent`s on `tx`. Returns when the stream ends (clean or error)
/// — the supervisor restarts it; the child is killed on drop. Returns early if
/// the receiver is gone.
pub async fn run_event_source<S: ProcessSpawner>(
    spawner: &S,
    tx: mpsc::Sender<GraphEvent>,
) -> io::Result<()> {
    let (_child, stdout) = spawner.spawn_capture("pactl", &["subscribe"]).await?;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        if let Some(ev) = parse_pactl_event(&line) {
            if tx.send(ev).await.is_err() {
                break; // reconciler gone — stop
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sink_and_server_events() {
        assert_eq!(
            parse_pactl_event("Event 'change' on sink #42"),
            Some(GraphEvent::Sink)
        );
        assert_eq!(
            parse_pactl_event("Event 'remove' on sink #42"),
            Some(GraphEvent::Sink)
        );
        assert_eq!(
            parse_pactl_event("Event 'new' on sink #7"),
            Some(GraphEvent::Sink)
        );
        assert_eq!(
            parse_pactl_event("Event 'change' on server"),
            Some(GraphEvent::Server)
        );
        // bare `on sink` (no #N) is also accepted
        assert_eq!(
            parse_pactl_event("Event 'change' on sink"),
            Some(GraphEvent::Sink)
        );
    }

    #[test]
    fn ignores_irrelevant_events() {
        assert_eq!(parse_pactl_event("Event 'new' on sink-input #10"), None);
        assert_eq!(parse_pactl_event("Event 'change' on source #3"), None);
        assert_eq!(parse_pactl_event("Event 'new' on client #99"), None);
        assert_eq!(parse_pactl_event("garbage line"), None);
        assert_eq!(parse_pactl_event(""), None);
    }

    #[tokio::test]
    async fn run_event_source_parses_stdout_into_channel() {
        use crate::sys::testing::FakeProcessSpawner;

        let stdout = b"Event 'change' on sink #42\n\
                       Event 'new' on sink-input #10\n\
                       Event 'change' on server\n"
            .to_vec();
        let spawner = FakeProcessSpawner::new(Some(0)).with_stdout(stdout);
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);

        run_event_source(&spawner, tx).await.unwrap();

        assert_eq!(rx.recv().await, Some(GraphEvent::Sink)); // the sink-input line was filtered out
        assert_eq!(rx.recv().await, Some(GraphEvent::Server));
        assert_eq!(rx.recv().await, None); // channel closed (source returned, tx dropped)
                                           // spawned the right command
        assert_eq!(spawner.spawns()[0].0, "pactl");
        assert_eq!(spawner.spawns()[0].1, vec!["subscribe"]);
    }
}
