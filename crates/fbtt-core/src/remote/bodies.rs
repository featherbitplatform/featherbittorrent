//! Reading a server's answer, and what can go wrong with one.
//!
//! These are the parts of [`crate::remote::http`] that do not need a connection to talk about: a size cap,
//! a stream reader, a JSON parse, and the enum every caller sorts its failure into. They live apart because
//! a login and a gateway token request need exactly these rules too, and a login cannot go through the API's
//! own client (see [`crate::remote::login`]).

use std::pin::Pin;

// Named rather than imported anonymously: the stream reader below bounds a body with `poll_next`, and a
// generic bound has to name the trait it is bounding.
use futures_core::Stream;
use reqwest::Response;

use crate::engine::map::clean;

/// The largest response body this client reads. A torrent list, a file list and a peer list are all far
/// below it; what reaches it is a server answering a question about a million-file torrent, or one that
/// is not answering in JSON at all.
pub(crate) const MAX_BODY: usize = 16 * 1024 * 1024;

/// What can go wrong with one call to a server, sorted by what this client should do about it.
#[derive(Debug)]
pub(crate) enum ApiError {
    /// The server could not be reached, or dropped the connection partway. The link is gone: the last
    /// snapshot stays on the screen and the backend dials again with a growing pause.
    Transport(String),
    /// The credential was refused and signing in again did not change that. Without a credential there
    /// is no library to list, so this counts as a lost link too. What it carries is the reason - the
    /// server's own line when it gave one, or what this client found wrong with the settings.
    Refused(String),
    /// An ordinary answer to a request this client should not have made: no such torrent, a folder
    /// outside the server's roots, a body the server could not parse. The link is fine; one line tells
    /// the person what the server said.
    Answer(String),
}

impl ApiError {
    /// The one line this error becomes on the screen, fit to draw.
    pub(crate) fn notice(&self) -> String {
        match self {
            ApiError::Transport(problem) => format!("the server could not be reached: {problem}"),
            ApiError::Refused(reason) => format!("sign-in was refused: {reason}"),
            ApiError::Answer(message) => message.clone(),
        }
    }

    /// Whether this error means the link to the server is gone, rather than this one request having
    /// asked for something the server does not have.
    pub(crate) fn breaks_the_link(&self) -> bool {
        matches!(self, ApiError::Transport(_) | ApiError::Refused(_))
    }
}

/// Reads a response body, refusing one larger than [`MAX_BODY`]. Public to this module's neighbours
/// because a login answer needs the same bound as a torrent list: a hostile server answers everything.
pub(crate) async fn read_capped(response: Response) -> Result<Vec<u8>, ApiError> {
    let mut stream = Box::pin(response.bytes_stream());
    let mut body = Vec::new();
    loop {
        match next(&mut stream).await {
            Some(Ok(chunk)) => {
                if !push(&mut body, chunk.as_ref()) {
                    return Err(too_big());
                }
            }
            Some(Err(problem)) => {
                return Err(ApiError::Transport(clean(&problem.to_string())));
            }
            None => return Ok(body),
        }
    }
}

/// Adds one chunk to a body, saying whether it stayed within [`MAX_BODY`]. Split out because the API's own
/// reader races this against somebody asking to stop, and both need the same answer.
pub(crate) fn push(body: &mut Vec<u8>, chunk: &[u8]) -> bool {
    if body.len() + chunk.len() > MAX_BODY {
        return false;
    }
    body.extend_from_slice(chunk);
    true
}

/// The line for a body over the cap: this client stopped reading at its own limit, so that is what it says.
pub(crate) fn too_big() -> ApiError {
    ApiError::Answer(format!(
        "the server sent more than {} MiB of response",
        MAX_BODY / (1024 * 1024)
    ))
}

/// The next item of a boxed stream.
///
/// `bytes_stream` answers with an `impl Stream` this crate cannot name, so both readers box it and come
/// through here; the item type stays generic so neither has to name `reqwest`'s chunk type either.
pub(crate) async fn next<S>(stream: &mut Pin<Box<S>>) -> Option<S::Item>
where
    S: Stream,
{
    std::future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

/// Parses a body this client has already bounded. A server that answers JSON-shaped nonsense, or not
/// JSON at all, is a broken server rather than a fatal one: the last good snapshot stays on the screen.
pub(crate) fn from_json<T: serde::de::DeserializeOwned>(body: Vec<u8>) -> Result<T, ApiError> {
    serde_json::from_slice(&body).map_err(|problem| {
        ApiError::Answer(format!(
            "the server sent a response this client could not read: {}",
            clean(&problem.to_string())
        ))
    })
}

/// The line for a failed response with no reason in it: the status, spelled as a person would say it.
pub(crate) fn status_line(status: reqwest::StatusCode) -> String {
    let reason = status.canonical_reason().unwrap_or("an error");
    format!("the server answered {status} {reason}")
}
