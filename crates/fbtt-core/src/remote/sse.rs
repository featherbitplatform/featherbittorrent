//! The server's event stream: one `GET` that stays open and says "something changed".
//!
//! It is what makes a remote library feel live without a person pressing anything: the server announces a
//! torrent's progress, a new peer, a tracker reply, and this client re-reads at once instead of waiting
//! for the next second. It runs as its own task rather than inside the refresh loop because it is *meant*
//! to block - a body that never ends is the point of it - and a stream that stalls must not stop the loop
//! that answers commands. All it ever does is wake somebody else.
//!
//! Four things keep it from hurting anybody:
//!
//! - Every line is capped (see [`MAX_LINE`]) and only a flag is kept for a frame's payload (see [`Parser`]),
//!   so a stream of nonsense costs one buffer rather than memory without limit.
//! - It never logs in. It dials with the credential the refresh loop already holds ([`Api::held`]) and only
//!   while that loop says the link is up ([`Gate`]). A wrong password or an expired gateway key therefore
//!   costs one refused login per backoff pause rather than one a second: an Argon2 verify and a token
//!   endpoint are not things to hammer.
//! - Its own reconnects follow the same doubling ladder the refresh loop uses ([`BACKOFF`]), so a stream that
//!   will not open does not ask harder as it fails more.
//! - An attempt is cancelled outright when the link goes down or the backend ends, so a blocked stream is
//!   never the reason a connection takes longer to close.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

use crate::client::Notice;
use crate::engine::map::clean;
use crate::remote::bodies::next;
use crate::remote::http::Api;
use crate::remote::{BACKOFF, Gate};

/// The longest event line this client reads. A server that answers `GET /api/v1/events` with one megabyte
/// of no newline is not describing an event; it is testing what this client does with bytes.
const MAX_LINE: usize = 1024 * 1024;

/// The longest event name this client keeps. Nothing here branches on what an event is called - every event
/// wakes the same refresh - so a server that names one with a megabyte of text costs this cap rather than a
/// megabyte, which the tests below measure.
const MAX_EVENT: usize = 1024;

/// How long to wait before dialling the stream up again after an ordinary end: a restart, or a proxy closing
/// an idle connection. Trouble waits longer - the ladder in [`BACKOFF`] (see [`spawn`]).
const RECONNECT: Duration = Duration::from_secs(1);

/// Reads the event stream until the backend ends, waking `wake` for every event.
///
/// `notices` is where a stream that broke says so, once per run of failures rather than once per failure: a
/// server that is down is one line about being down, not a line a second forever. The task stops when the
/// gate's sender is dropped, which is how the backend ending ends this too - it holds no handle on the thing
/// it wakes, so it cannot be waiting on its own wake-up.
pub(crate) fn spawn(
    api: Arc<Api>,
    wake: Arc<Notify>,
    notices: mpsc::Sender<Notice>,
    mut gate: watch::Receiver<Gate>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut complained = false;
        // Which rung of the ladder this stream is waiting on. Its own rather than shared with the refresh
        // loop's, because a stream that will not open and a table that will not read are different troubles,
        // and neither should make the other wait longer than it has to.
        let mut backoff = 0;
        loop {
            // Wait until the refresh loop says it can talk to this server. It only says so after a round of
            // reads got through with a credential, so by then there is one to send and this task never has to
            // log in itself. The sender going away means the backend ended, which ends this too.
            if gate.wait_for(|state| *state == Gate::Up).await.is_err() {
                return;
            }
            let Some(credential) = api.held() else {
                // Nothing to send: a credential in its renewal window, or one just cleared by a refusal. The
                // refresh loop's next round fixes that; dialling with no credential would be guessing.
                match wait(RECONNECT, &mut gate).await {
                    Waited::Closed => return,
                    Waited::Slept | Waited::Changed => continue,
                }
            };

            // Which thing ended this attempt.
            enum Attempt {
                /// The stream ended; `Some(line)` when there is a reason worth saying out loud.
                Ended(Option<String>),
                /// The link went down underneath us: stop asking, and stay off until the loop says otherwise.
                /// The loop reports that trouble itself, so this adds no line of its own.
                Paused,
                /// The backend ended: nobody left to wake.
                Gone,
            }
            // One attempt, cancelled outright if the link goes down while it is open: a stream is meant to
            // block, and the one thing that must never wait on it is the connection that has already decided
            // it cannot reach this server.
            let attempt = tokio::select! {
                outcome = async {
                    match api.events_with(&credential).await {
                        Ok(response) => read(response, &wake).await,
                        Err(problem) => Some(problem.line()),
                    }
                } => Attempt::Ended(outcome),
                changed = gate.changed() => match changed {
                    Ok(()) => Attempt::Paused,
                    Err(_) => Attempt::Gone,
                },
            };
            let pause = match attempt {
                Attempt::Ended(None) => {
                    // An ordinary end clears the last complaint, so the next trouble gets its own line - and
                    // it is not trouble: one second, not the ladder.
                    complained = false;
                    backoff = 0;
                    RECONNECT
                }
                Attempt::Ended(Some(reason)) => {
                    report(&notices, &mut complained, reason);
                    let rung = BACKOFF[backoff.min(BACKOFF.len() - 1)];
                    backoff = (backoff + 1).min(BACKOFF.len() - 1);
                    rung
                }
                // A pause is not this stream's trouble to report and not its trouble to wait out: the wait at
                // the top of this loop holds until the link is worth dialling again.
                Attempt::Paused => continue,
                Attempt::Gone => return,
            };
            match wait(pause, &mut gate).await {
                // A gate change cuts the pause short: the wait at the top of this loop looks at what the link
                // says now, which is either "stay off" or "dial", and dialing early beats dialing late when
                // the refresh loop has just brought the link back.
                Waited::Slept | Waited::Changed => {}
                Waited::Closed => return,
            }
        }
    })
}

/// What ended a pause between attempts.
enum Waited {
    /// The pause ran out.
    Slept,
    /// The link's state moved: worth looking at again at once.
    Changed,
    /// The backend ended: nobody left to wake.
    Closed,
}

/// Waits `pause`, or comes back early when the gate moves or the backend ends - whichever happens first.
async fn wait(pause: Duration, gate: &mut watch::Receiver<Gate>) -> Waited {
    tokio::select! {
        _ = tokio::time::sleep(pause) => Waited::Slept,
        changed = gate.changed() => match changed {
            Ok(()) => Waited::Changed,
            Err(_) => Waited::Closed,
        },
    }
}

/// One line about a stream that stopped working, unless this run of failures has already said it.
fn report(notices: &mpsc::Sender<Notice>, complained: &mut bool, reason: String) {
    if *complained {
        return;
    }
    *complained = true;
    let _ = notices.try_send(Notice::Error(format!(
        "the event stream stopped: {reason}; watching by polling instead"
    )));
}

/// Reads frames until the stream ends, waking `wake` for each one. The answer is why it stopped, when
/// there is a reason worth saying out loud; `None` covers an ordinary end, which needs no notice because
/// dialling again is what an end means.
async fn read(response: reqwest::Response, wake: &Notify) -> Option<String> {
    let mut stream = Box::pin(response.bytes_stream());
    let mut parser = Parser::default();
    // One line at a time, and never more than one line's worth of it.
    let mut line: Vec<u8> = Vec::new();
    loop {
        let chunk = next(&mut stream).await?;
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(problem) => {
                return Some(format!(
                    "the connection dropped: {}",
                    clean(&problem.to_string())
                ));
            }
        };
        for byte in chunk.as_ref() {
            if *byte == b'\n' {
                if parser.line(&line) {
                    // One permit is enough however many events arrive meanwhile: the refresh loop takes one
                    // wake per round of reads rather than one per event (see `crate::remote`).
                    wake.notify_one();
                }
                line.clear();
            } else if line.len() >= MAX_LINE {
                return Some(format!(
                    "the server sent a line with no end in it, longer than {} bytes",
                    MAX_LINE
                ));
            } else {
                line.push(*byte);
            }
        }
    }
}

/// The frames of one stream, assembled line by line.
///
/// A frame is `event:` and `data:` fields and a blank line; the blank line is what says the frame is over,
/// so a frame split across packets is still one frame. Lines that are not fields - a `:` comment, which is
/// how a server keeps an idle connection alive - say nothing happened.
///
/// What a frame carries is thrown away on purpose: this client's whole answer to an event is to re-read the
/// server, so the payload is fetched again rather than kept. That also bounds what a frame can cost - a
/// server that never finishes one holds a flag and a capped name, not a growing list of its lines, which the
/// tests below measure.
#[derive(Default)]
struct Parser {
    /// The name this frame has been given, kept only as far as [`MAX_EVENT`]. Keeping the whole thing would
    /// be memory for something this client never looks at.
    event: Option<String>,
    /// Whether this frame carried a `data:` line at all, which is the only thing its payload is used for.
    has_data: bool,
}

impl Parser {
    /// Consumes one line and says whether it completed an event.
    fn line(&mut self, raw: &[u8]) -> bool {
        // Bytes that are not UTF-8 become the replacement character rather than an error: this is a server's
        // text about its own torrents, and a mangled name is not a reason to lose the stream.
        let text = String::from_utf8_lossy(raw);
        // A line ending in CRLF has the CR as part of the previous line's end, not of its value.
        let text = match text.strip_suffix('\r') {
            Some(stripped) => stripped,
            None => text.as_ref(),
        };

        if text.is_empty() {
            // The blank line: dispatch whatever was collected, and start the next frame empty.
            let dispatched = self.has_data || self.event.take().is_some();
            self.has_data = false;
            return dispatched;
        }
        if text.starts_with(':') {
            // A comment (a keep-alive): it belongs to no field and dispatches nothing.
            return false;
        }
        let (field, value) = match text.split_once(':') {
            Some((field, value)) => (field, value),
            // A line with no colon is a field name with an empty value, which is what the protocol says.
            None => (text, ""),
        };
        // One space after the colon is part of the framing, not the start of the value.
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(kept(value)),
            // The payload itself is dropped: `has_data` is all this client takes from it.
            "data" => self.has_data = true,
            // `id:` names the last event and `retry:` asks for a reconnect pause; this client re-reads
            // everything after any event and dials again on its own timer, so neither changes what it does.
            _ => {}
        }
        false
    }

    /// The bytes this parser is holding for the frame it is in the middle of. A frame that never ends must
    /// not grow without bound, which is what the tests below check.
    #[cfg(test)]
    fn retained(&self) -> usize {
        self.event.as_ref().map_or(0, |name| name.capacity())
    }
}

/// The part of an event name this client keeps: whole characters up to [`MAX_EVENT`] bytes, so a cap that
/// lands in the middle of a multi-byte character cuts at the character rather than through it.
fn kept(value: &str) -> String {
    let end = value.floor_char_boundary(MAX_EVENT);
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::{MAX_EVENT, MAX_LINE, Parser};

    /// A frame is only an event once its blank line arrives, however the lines were split up.
    #[test]
    fn a_blank_line_dispatches_one_frame() {
        let mut parser = Parser::default();
        assert!(!parser.line(b"event: torrent-changed"));
        assert!(!parser.line(b"data: {\"id\":\"aa\"}"));
        assert!(parser.line(b""), "the blank line is the event");
        assert!(
            !parser.line(b"event: x"),
            "and the next frame is not one yet"
        );
        assert!(parser.line(b""));
    }

    /// Keep-alive comments and unknown fields say nothing happened, which is what a server keeping an idle
    /// stream open is for.
    #[test]
    fn comments_and_unknown_fields_wake_nobody() {
        let mut parser = Parser::default();
        assert!(!parser.line(b": keep-alive"));
        assert!(!parser.line(b"id: 17"));
        assert!(!parser.line(b"retry: 2000"));
        assert!(
            !parser.line(b""),
            "nothing was collected, so nothing dispatches"
        );
    }

    /// A CRLF line ending is a line ending, and several `data:` lines are one event rather than several.
    #[test]
    fn crlf_and_several_data_lines_are_one_event() {
        let mut parser = Parser::default();
        assert!(!parser.line(b"event: progress\r"));
        assert!(!parser.line(b"data: one\r"));
        assert!(!parser.line(b"data: two\r"));
        assert!(parser.line(b"\r"), "the blank line, even with its CR");
    }

    /// A frame that names an event but carries no data is still something that happened: this client's whole
    /// answer to an event is to re-read, and it does not need payload to do that.
    #[test]
    fn an_event_with_no_data_still_counts() {
        let mut parser = Parser::default();
        assert!(!parser.line(b"event: session-changed"));
        assert!(parser.line(b""));
    }

    /// A server that never finishes a frame - `data:` lines with no blank line between them, each one as long
    /// as this client will read - costs a flag and nothing else. The payload is re-read from the server
    /// anyway, so keeping it would be memory for something nobody looks at.
    #[test]
    fn a_frame_that_never_ends_costs_a_flag() {
        let mut line = b"data: ".to_vec();
        line.extend_from_slice(&vec![b'x'; MAX_LINE]);
        let mut parser = Parser::default();
        for _ in 0..64 {
            assert!(
                !parser.line(&line),
                "no blank line, so no event and nothing collected"
            );
        }
        assert_eq!(
            parser.retained(),
            0,
            "sixty-four megabyte payloads left not one allocation behind"
        );
        // And the flag is a real one: the frame that finally ends does wake somebody.
        assert!(parser.line(b""));
    }

    /// The name is capped rather than chased: absurd event names cost one small buffer, and a cap that lands
    /// inside a multi-byte character cuts at the character rather than through it.
    #[test]
    fn an_event_name_is_kept_only_as_far_as_the_cap() {
        let mut parser = Parser::default();
        let long = format!("event: {}", "n".repeat(MAX_EVENT * 4));
        assert!(!parser.line(long.as_bytes()));
        assert!(
            parser.retained() <= MAX_EVENT,
            "the name is kept up to the cap, not in full"
        );
        assert!(parser.line(b""), "and it is still one event");

        let mut parser = Parser::default();
        let multibyte = format!("event: {}", "é".repeat(MAX_EVENT));
        assert!(
            !parser.line(multibyte.as_bytes()),
            "a cap inside a character does not panic"
        );
        assert!(parser.retained() <= MAX_EVENT);
    }
}
