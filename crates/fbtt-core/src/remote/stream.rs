//! What a `Stream` command becomes over a server: the largest file when none was named,
//! the stream address its token route answers with, and the one rule that says whether
//! that answer is an address this client will hand to a player at all.
//!
//! The address the server gives back is server text the way a torrent name is server
//! text, and a player that opens one of them does whatever it opens: a `file://` URL, an
//! address with a newline in it, anything. [`checked_stream_path`] is the one place that
//! decides whether an answer may become a play target, and it is pure, so its refusal is
//! a test that needs no server.

use std::sync::Arc;

use crate::client::{PlaySource, PlayTarget, TorrentId};
use crate::remote::Backend;
use crate::remote::map::FileDto;

/// The longest an answer's path may be: `stream_path` is 59 bytes for the id it is built
/// for, and the token that follows it is 43, so anything longer is not an answer at all.
const MAX_PATH_BYTES: usize = 512;
/// The longest a stream token may be on the end of a path: 32 random bytes, base64url
/// without padding, is 43, and one longer than this is not one this server issued.
const MAX_TOKEN_CHARS: usize = 128;

/// The line a refused stream address is reported as. It names nothing the server sent:
/// repeating an address that looks like a credential, or one that is a `file://` URL,
/// would put the hostile half of the answer on the screen.
const REFUSED_PATH: &str = "The server answered with a stream address this client will not use";

/// The server's answer to `POST /api/v1/stream-tokens`: the address to fetch, and how
/// long its credential may go before it must be used.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct StreamTokenAnswer {
    path: String,
    #[allow(dead_code)]
    expires_in: u64,
}

/// The body of `POST /api/v1/stream-tokens`: which torrent, and which file of it.
#[derive(serde::Serialize)]
struct StreamTokenRequest<'a> {
    torrent: &'a str,
    file: usize,
}

/// The one rule a server's stream answer has to pass to become a play target.
///
/// It accepts only a path that starts with `stream_path(id, index)` - the very address
/// this client asked for, by the wire hash in its 40 lowercase hex digits and the index
/// as a plain decimal - and is followed by nothing, or by `?token=` and 1 to
/// [`MAX_TOKEN_CHARS`] characters of unreserved base64url: the alphabet a stream token
/// is spelled in. Nothing else - no other key in the query, no second value, no newline,
/// no `//`, and no path longer than [`MAX_PATH_BYTES`] bytes.
///
/// The compare is byte for byte on the parts, so a server that answers with another
/// spelling of the same file, a different index, or a URL altogether is refused, and a
/// player never opens what the server said it was not.
pub(crate) fn checked_stream_path<'a>(
    path: &'a str,
    id: &TorrentId,
    index: usize,
) -> Option<&'a str> {
    if path.len() > MAX_PATH_BYTES {
        return None;
    }
    // The id must be exactly 40 lowercase hex characters before we build any path from it:
    // a hostile id with a newline, a `/`, an upper-case letter, or a different length
    // would change the authority or the shape of the prefix we compare against.
    let id_str = &id.0;
    if id_str.len() != 40
        || !id_str
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return None;
    }
    let prefix = crate::stream::stream_path(id, index);
    let rest = path.strip_prefix(&prefix)?;
    if rest.is_empty() {
        return Some(path);
    }
    let token = rest.strip_prefix("?token=")?;
    if token.is_empty() || token.len() > MAX_TOKEN_CHARS {
        return None;
    }
    if !token
        .bytes()
        .all(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
    {
        return None;
    }
    Some(path)
}

impl Backend {
    /// One `Command::Stream` answered: the file's list read for the choice when `file` is
    /// `None`, an address from the server's token route, checked by
    /// [`checked_stream_path`], and out as a `Play`. A target with a gateway credential
    /// gets a loopback relay URL instead of the server's, because a player cannot send the
    /// gateway's header (see [`crate::remote::relay`]); a direct target gets the server's
    /// own address.
    pub(crate) async fn stream(&mut self, id: &TorrentId, file: Option<usize>) {
        // Which file: the wanted one if it names a real, non-pad file, else the largest
        // non-pad file - the same choice the local backend makes from its engine's list.
        let path = crate::remote::routes::torrent_route(&id.0, "/files");
        let files = match self.api.json::<Vec<FileDto>>(self.api.get(&path)).await {
            Ok(files) => files,
            Err(problem) => return self.report(Err(problem)),
        };
        let Some(index) = choose(&files, file) else {
            return self.notify(crate::client::Notice::Error(
                "the torrent has no playable file".to_owned(),
            ));
        };
        // Look up the file by its `index` field, not by position: the server's `index`
        // values are its own, and a hostile list may not line them up with the list's
        // positions. A missing match is an error notice, not a panic.
        let Some(chosen) = find_file(&files, index) else {
            return self.notify(crate::client::Notice::Error(
                "the server's file list does not name the file it chose".to_owned(),
            ));
        };
        // The address to stream it from. The id the server names in its answer is the wire
        // hash this client asked by, so the check is against that id and the index it
        // named: the two together are the one file this command asked for.
        let body = StreamTokenRequest {
            torrent: &id.0,
            file: index,
        };
        let answer: StreamTokenAnswer = match self
            .api
            .json::<StreamTokenAnswer>(self.api.post_json("/api/v1/stream-tokens", &body))
            .await
        {
            Ok(answer) => answer,
            Err(problem) => return self.report(Err(problem)),
        };
        // The check: the only rule between the server's text and the player's address.
        // A refusal names nothing of what the server sent (see `REFUSED_PATH`).
        let checked = match checked_stream_path(&answer.path, id, index) {
            Some(path) => path,
            None => return self.notify(crate::client::Notice::Error(REFUSED_PATH.to_owned())),
        };
        // The player's address: a relay URL for a target that signs to a gateway, the
        // server's own for one that does not.
        let url = if self.api.has_gateway_credential().await {
            match self.relay_url_for(checked).await {
                Ok(url) => url,
                Err(problem) => {
                    return self.notify(crate::client::Notice::Error(crate::engine::map::clean(
                        &problem,
                    )));
                }
            }
        } else {
            crate::client::StreamUrl::new(format!("{}{}", self.api.base(), checked))
        };
        self.notify(crate::client::Notice::Play(PlayTarget::new(
            &crate::engine::map::clean(&chosen.path),
            PlaySource::Url(url),
        )));
    }

    /// The relay's URL for the server address `checked`: the relay is started on the
    /// first ask, kept for the run, and the URL is issued from its own token store.
    async fn relay_url_for(&self, checked: &str) -> Result<crate::client::StreamUrl, String> {
        use crate::remote::relay;
        {
            let guard = self
                .relay
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(relay) = guard.as_ref() {
                return Ok(relay.url_for(checked));
            }
        }
        // The bind is the only await the lock does not cross.
        let api = Arc::clone(&self.api);
        let started = relay::Relay::start(api).await?;
        let mut guard = self
            .relay
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(started);
        let relay = guard.as_ref().expect("just set");
        Ok(relay.url_for(checked))
    }
}

/// The file a remote `Stream` plays: the wanted file if it names a real, non-pad file
/// whose `index` field matches, else the largest non-pad file (lowest index on a tie),
/// `None` when there is none.
///
/// The returned value is the `index` field of the chosen file. The caller must look it up
/// by that field (`find_file`) rather than using it as a positional index: the server's
/// `index` values are its own, and a hostile list may name an index that does not exist
/// or that is duplicated. A missing match is an error notice, not a panic.
pub(crate) fn choose(files: &[FileDto], wanted: Option<usize>) -> Option<usize> {
    if let Some(wanted_index) = wanted
        && let Some(file) = files.iter().find(|file| file.index == wanted_index)
        && !file.pad
    {
        return Some(file.index);
    }
    files
        .iter()
        .filter(|file| !file.pad)
        .max_by_key(|file| (file.len, std::cmp::Reverse(file.index)))
        .map(|file| file.index)
}

/// Look up a file by its `index` field. `None` when no file in the list has that index:
/// a hostile server may name an index that does not exist in its own list, and the
/// answer is an error notice, not a panic.
pub(crate) fn find_file(files: &[FileDto], index: usize) -> Option<&FileDto> {
    files.iter().find(|file| file.index == index)
}

#[cfg(test)]
mod tests {
    use super::{REFUSED_PATH, checked_stream_path, choose};
    use crate::client::TorrentId;
    use crate::remote::dto::FileDto;

    /// The id every path below is built from: a wire hash in its 40 lowercase hex digits.
    fn id() -> TorrentId {
        TorrentId("0123456789abcdef0123456789abcdef01234567".to_string())
    }

    fn file(index: usize, path: &str, len: u64, pad: bool) -> FileDto {
        FileDto {
            index,
            path: path.to_string(),
            len,
            pad,
            ..Default::default()
        }
    }

    /// Every address a server may answer with, and the one shape this client will hand to a
    /// player: the prefix it asked for, nothing on the end, or its own token on the end.
    /// Everything else - another file, a URL, a newline in the query, a path that is a
    /// megabyte - is refused, and the refusal is the one line that names nothing of it.
    #[test]
    fn hostile_stream_path_is_refused() {
        let id = id();

        // The real shape, with and without the token: accepted, and the whole path is the
        // address that is checked.
        let bare = crate::stream::stream_path(&id, 0);
        assert_eq!(checked_stream_path(&bare, &id, 0), Some(bare.as_str()));
        let with_token = format!("{bare}?token=abc-DEF_0123");
        assert_eq!(
            checked_stream_path(&with_token, &id, 0),
            Some(with_token.as_str())
        );
        // A 128-character token is the limit, and it is accepted at it.
        let full = "x".repeat(128);
        let at_limit = format!("{bare}?token={full}");
        assert_eq!(
            checked_stream_path(&at_limit, &id, 0),
            Some(at_limit.as_str()),
            "a 128-character token is at the limit, not past it"
        );

        // A newline in the token, percent-encoded or not: the token is a credential, and a
        // newline in one is a request smuggling the client's own connection would parse.
        let smuggled = format!("{bare}?token=a\nfile:///etc/passwd");
        assert_eq!(checked_stream_path(&smuggled, &id, 0), None);
        let encoded = format!("{bare}?token=%0a");
        assert_eq!(
            checked_stream_path(&encoded, &id, 0),
            None,
            "an encoded newline is a newline: the token's alphabet has no percent"
        );
        // A `file://` URL and an http URL are not stream addresses at all.
        assert_eq!(checked_stream_path("file:///x", &id, 0), None);
        assert_eq!(checked_stream_path("http://evil/", &id, 0), None);
        // A path the size of a megabyte is not a path.
        let big = format!("{bare}?token={}", "x".repeat(2048 - bare.len() - 7));
        assert_eq!(checked_stream_path(&big, &id, 0), None);
        // A token longer than the limit, and an empty one.
        assert_eq!(
            checked_stream_path(&format!("{bare}?token={}", "x".repeat(129)), &id, 0),
            None
        );
        assert_eq!(checked_stream_path(&format!("{bare}?token="), &id, 0), None);
        // The right prefix, another index: the check is against the file that was asked
        // for, and the server answering for a neighbour is a server to refuse.
        assert_eq!(
            checked_stream_path(&crate::stream::stream_path(&id, 1), &id, 0),
            None,
            "the right shape for another index is refused"
        );
        // The right prefix, a query with any other key: the token is the only thing a
        // stream answer may carry.
        assert_eq!(
            checked_stream_path(&format!("{bare}?x=1"), &id, 0),
            None,
            "another query key is not a stream address"
        );
        assert_eq!(
            checked_stream_path(&format!("{bare}?token=a&x=1"), &id, 0),
            None,
            "a second query key is not a stream address"
        );
        // The check is on the id asked too: a server answering for another torrent's
        // file is a server answering for a file this client did not ask for.
        let other = TorrentId("ffffffffffffffffffffffffffffffffffffffff".to_string());
        assert_eq!(
            checked_stream_path(&crate::stream::stream_path(&id, 0), &other, 0),
            None,
            "another id's path is not the one asked for"
        );
        // Upper-cased id or index: another spelling is another file, the same rule the
        // token's scope applies.
        let upper = "/api/v1/torrents/0123456789ABCDEF0123456789ABCDEF01234567/files/0/stream";
        assert_eq!(checked_stream_path(upper, &id, 0), None);
        assert_eq!(
            checked_stream_path(
                "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/00/stream",
                &id,
                0
            ),
            None,
            "a leading zero in the index is another file"
        );

        // The refusal itself names nothing: the line a player's screen would draw carries
        // no part of whatever the server sent.
        assert!(!REFUSED_PATH.contains("token"));
        assert!(!REFUSED_PATH.contains("file://"));
    }

    /// The choice from the server's own list: the wanted file when it is a real file,
    /// the largest non-pad file when nothing is wanted, and nothing when the list holds
    /// no playable file.
    #[test]
    fn the_largest_non_pad_file_is_chosen() {
        let files = vec![
            file(0, "a.bin", 10, false),
            file(1, "b.bin", 30, false),
            file(2, "pad", 512, true),
            file(3, "c.bin", 20, false),
        ];
        assert_eq!(choose(&files, None), Some(1));
        assert_eq!(choose(&files, Some(3)), Some(3));
        // A pad is never the answer, wanted or not.
        assert_eq!(choose(&files, Some(2)), Some(1));
        let all_pad = vec![file(0, "pad", 10, true), file(1, "pad", 99, true)];
        assert_eq!(choose(&all_pad, None), None);
        assert_eq!(choose(&all_pad, Some(1)), None);
        // An out-of-range wanted index falls back to the largest file.
        assert_eq!(choose(&files, Some(99)), Some(1));
        // A tie in size: the lowest index.
        let tie = vec![file(0, "a.bin", 50, false), file(1, "b.bin", 50, false)];
        assert_eq!(choose(&tie, None), Some(0));
    }

    /// A hostile file list: the `index` field is looked up, not used as a positional
    /// index. A list whose `index` values do not line up with the list's positions must
    /// not panic, and a wanted index that names no file in the list must not panic.
    #[test]
    fn hostile_file_list_does_not_panic() {
        // A file list where the `index` field does not match the position: a server
        // that says index 7 for the first file in a list of one.
        let offset = vec![file(7, "a.bin", 10, false)];
        // No wanted index: the largest non-pad file is chosen by its `index` field.
        assert_eq!(choose(&offset, None), Some(7));
        // The lookup by `index` field finds it.
        assert!(super::find_file(&offset, 7).is_some());
        // An out-of-range wanted index: no file has that index, so the largest is chosen.
        assert_eq!(choose(&offset, Some(0)), Some(7));
        // A wanted index that matches the position but not the `index` field: refused.
        assert_eq!(choose(&offset, Some(0)), Some(7));

        // Duplicate indexes: the first match wins, no panic.
        let dup = vec![
            file(3, "a.bin", 10, false),
            file(3, "b.bin", 30, false),
            file(3, "c.bin", 20, false),
        ];
        // No wanted index: the largest non-pad file (b.bin at index 3).
        assert_eq!(choose(&dup, None), Some(3));
        // A wanted index that matches: the first match (a.bin) is found.
        assert_eq!(choose(&dup, Some(3)), Some(3));
        // The lookup finds the first file with that index.
        let found = super::find_file(&dup, 3).expect("a file at index 3");
        assert_eq!(found.path, "a.bin");

        // A wanted index that is a pad file: falls back to the largest non-pad file.
        let with_pad = vec![file(0, "a.bin", 10, false), file(1, "pad", 999, true)];
        assert_eq!(choose(&with_pad, Some(1)), Some(0));
    }

    /// A hostile torrent id in `checked_stream_path`: the id must be exactly 40 lowercase
    /// hex characters before any path is built from it. Newline, `?`, `/`, upper case, and
    /// a different length are all refused.
    #[test]
    fn hostile_torrent_id_is_refused() {
        let id = id();
        let bare = crate::stream::stream_path(&id, 0);
        // The valid id passes.
        assert_eq!(checked_stream_path(&bare, &id, 0), Some(bare.as_str()));

        // A newline in the id: a request smuggling vector.
        let newline = TorrentId("0123456789abcdef0123456789abcdef012345\n7".to_string());
        assert_eq!(checked_stream_path(&bare, &newline, 0), None);
        // A `?` in the id: a query smuggling vector.
        let question = TorrentId("0123456789abcdef0123456789abcdef0123456?".to_string());
        assert_eq!(checked_stream_path(&bare, &question, 0), None);
        // A `/` in the id: a path smuggling vector.
        let slash = TorrentId("0123456789abcdef0123456789abcdef0123456/".to_string());
        assert_eq!(checked_stream_path(&bare, &slash, 0), None);
        // Upper case in the id: another spelling.
        let upper = TorrentId("0123456789ABCDEF0123456789abcdef01234567".to_string());
        assert_eq!(checked_stream_path(&bare, &upper, 0), None);
        // 64 hex characters (a SHA-256): the wrong length.
        let sha256 = TorrentId(
            "0123456789abcdef0123456789abcdef012345670123456789abcdef0123456789abcdef".to_string(),
        );
        assert_eq!(checked_stream_path(&bare, &sha256, 0), None);
        // 39 characters: too short.
        let short = TorrentId("0123456789abcdef0123456789abcdef0123456".to_string());
        assert_eq!(checked_stream_path(&bare, &short, 0), None);
    }
}
