//! The server's event stream: one `GET` that stays open and says "something changed".
//!
//! It is what makes a remote library feel live without a person pressing anything: the server announces a
//! torrent's progress, a new peer, a tracker reply, and this client re-reads at once instead of waiting
//! for the next second. It runs as its own task rather than inside the refresh loop because it is *meant*
//! to block - a body that never ends is the point of it - and a stream that stalls must not stop the loop
//! that answers commands. All it ever does is wake somebody else.
//!
//! Two things keep a hostile or half-broken stream from hurting this process: every line is capped (see
//! [`MAX_LINE`]), so an endless line costs one buffer and a notice rather than memory without limit, and
//! a stream that ends - on its own, in pieces, or because the server went away - is dialled again.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::client::Notice;
use crate::engine::map::clean;
use crate::remote::http::{self, Api};

/// The longest event line this client reads. A server that answers `GET /api/v1/events` with one megabyte
/// of no newline is not describing an event; it is testing what this client does with bytes.
const MAX_LINE: usize = 1024 * 1024;

/// How long to wait before dialling the stream up again: after it ends, after a line outgrows
/// [`MAX_LINE`], or after a connection that failed. A second is what the brief's reconnect means, and it
/// is short because this client is already being kept honest by the refresh loop's own backoff - the two
/// together never let a dead server be hammered.
const RECONNECT: Duration = Duration::from_secs(1);

/// Reads the event stream until the backend ends, waking `wake` for every event.
///
/// `notices` is where a stream that broke says so, once per run of failures rather than once per failure:
/// a server that is down is one line about being down, not a line a second forever. The task stops when
/// `stop`'s sender is dropped, which is how the backend ending ends this too - it holds no handle on the
/// channel it wakes, so it cannot be waiting on its own wake-up.
pub(crate) fn spawn(
    api: Arc<Api>,
    wake: mpsc::UnboundedSender<()>,
    notices: mpsc::Sender<Notice>,
    mut stop: watch::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut complained = false;
        loop {
            // One attempt at the stream, cancelled outright if the backend goes away while it is open:
            // a stream is meant to block, and the one thing that must never wait on it is the connection
            // that has already decided to stop.
            tokio::select! {
                attempt = async {
                    match api.events().await {
                        Ok(response) => read(response, &wake).await,
                        Err(problem) => Some(problem.notice()),
                    }
                } => match attempt {
                    // A stream that stopped for a reason worth saying. The streak flag is what keeps a
                    // server that always answers this way to one line rather than one a second.
                    Some(reason) => report(&notices, &mut complained, reason),
                    // An ordinary end - a restart, a proxy closing an idle connection - clears the last
                    // complaint, so the next trouble gets its own line.
                    None => complained = false,
                },
                // The backend is gone: nobody left to wake.
                Err(_) = stop.changed() => return,
            }
            tokio::select! {
                _ = tokio::time::sleep(RECONNECT) => {}
                // The backend is gone: nobody left to wake.
                Err(_) = stop.changed() => return,
            }
        }
    })
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
async fn read(response: reqwest::Response, wake: &mpsc::UnboundedSender<()>) -> Option<String> {
    let mut stream = Box::pin(response.bytes_stream());
    let mut parser = Parser::default();
    // One line at a time, and never more than one line's worth of it.
    let mut line: Vec<u8> = Vec::new();
    loop {
        let chunk = http::next(&mut stream).await?;
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
                    // The wake failing means the backend has gone, which is the end of this stream too.
                    if wake.send(()).is_err() {
                        return None;
                    }
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
#[derive(Default)]
struct Parser {
    /// The event name this frame has been given so far.
    event: Option<String>,
    /// The `data:` lines so far, which a dispatch joins with newlines as the protocol spells one value.
    data: Vec<String>,
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
            let dispatched = !self.data.is_empty() || self.event.take().is_some();
            self.data.clear();
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
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            // `id:` names the last event and `retry:` asks for a reconnect pause; this client re-reads
            // everything after any event and dials again on its own timer, so neither changes what it does.
            _ => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::Parser;

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
}
