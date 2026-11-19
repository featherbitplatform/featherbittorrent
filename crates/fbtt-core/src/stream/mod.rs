//! What playing a file needs: the address of one file's stream, and the tokens that
//! open it.
//!
//! A file is streamed through fbt-api's own route, and the only way a stranger gets to a
//! file's bytes is a token scoped to that one file's address. [`tokens::StreamTokens`] is
//! where a token is born, dies and is checked; [`stream_path`] is the one spelling every
//! such address takes, so the token and the URL it guards are the same string.

pub mod local;
pub mod tokens;

use crate::client::TorrentId;

/// The address of one file's stream, the one every stream token is scoped to.
///
/// `id` is already the wire hash in its 40 lowercase hex digits, and `index` is spelled
/// as a plain decimal with no leading zeros, so the address this spells is the one a
/// token was issued for: the compare on the other side is byte for byte, and another
/// spelling of the same file is another file to a token.
pub fn stream_path(id: &TorrentId, index: usize) -> String {
    format!("/api/v1/torrents/{}/files/{index}/stream", id.0)
}

#[cfg(test)]
mod tests {
    use super::stream_path;
    use crate::client::TorrentId;

    /// The address a token is scoped to: the wire hash as it is given, the index as a
    /// plain decimal, nothing else.
    #[test]
    fn stream_path_spells_the_route() {
        let id = TorrentId("0123456789abcdef0123456789abcdef01234567".to_string());
        assert_eq!(
            stream_path(&id, 0),
            "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/0/stream"
        );
        assert_eq!(
            stream_path(&id, 12),
            "/api/v1/torrents/0123456789abcdef0123456789abcdef01234567/files/12/stream"
        );
    }
}
