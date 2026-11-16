//! The routes this client calls, and the escaping a server's text needs to stay inside one of them.
//!
//! Paths are written out at their call sites rather than built from parts, so that a reader of either crate
//! can match a request to the route it reaches. The one thing that does get assembled is the `{id}` segment:
//! a torrent id is a server's text, and this client puts it in a path, which is where the escaping comes in.

use crate::client::DetailTab;

/// The pane's tab as the tail of the route that answers it: `/files`, `/peers`, and so on, ready to hand to
/// [`torrent_route`].
pub(crate) fn tab_path(tab: DetailTab) -> &'static str {
    match tab {
        DetailTab::Files => "/files",
        DetailTab::Peers => "/peers",
        DetailTab::Trackers => "/trackers",
        DetailTab::Pieces => "/pieces",
    }
}

/// Percent-encodes one path or query component, so a device name with a space in it or a folder path with
/// a slash reaches the route that has to match it exactly. Everything outside the unreserved set becomes a
/// `%` escape, which is what a server decodes back to the byte it started as.
pub(crate) fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            // Spaces are the common case and every other byte needs no judgement either: this is one table.
            _ => {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("%{byte:02X}"));
            }
        }
    }
    out
}

/// `/api/v1/torrents/{id}` plus `tail`, with the id escaped.
///
/// The id came from a server, and an unescaped slash in it would move the request to a different route:
/// `x/../../auth/session` becomes one `%2F`-escaped segment here, so it can only ever name a torrent -
/// which is what it decodes back to on the other side. `tail` is the part after the id, leading slash and
/// all (`""`, `"/pause"`, `"/files"`).
pub(crate) fn torrent_route(id: &str, tail: &str) -> String {
    format!("/api/v1/torrents/{}{tail}", encode(id))
}

#[cfg(test)]
mod tests {
    use super::{encode, torrent_route};

    /// The unreserved set and nothing more: `:` and `/` in an OAuth2 assertion type become escapes a
    /// gateway has to undo, which is why the fake one in `tests/remote.rs` decodes percent pairs rather
    /// than spaces alone.
    #[test]
    fn encode_leaves_only_the_unreserved_characters_bare() {
        assert_eq!(encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode("a b"), "a%20b");
        assert_eq!(encode("a/b"), "a%2Fb");
        assert_eq!(
            encode("urn:ietf:params:oauth:client-assertion-type:jwt-bearer"),
            "urn%3Aietf%3Aparams%3Aoauth%3Aclient-assertion-type%3Ajwt-bearer"
        );
    }

    /// A torrent id is a server's text, and a hostile one can contain path separators. Escaped, it stays
    /// inside the `{id}` segment of the route it is put in rather than becoming other routes' segments -
    /// dots survive because they are unreserved, slashes do not, so this cannot climb out.
    #[test]
    fn a_torrent_id_cannot_reach_another_route() {
        assert_eq!(
            torrent_route("aabb", "/pause"),
            "/api/v1/torrents/aabb/pause"
        );
        assert_eq!(
            torrent_route("x/../../auth/session", ""),
            "/api/v1/torrents/x%2F..%2F..%2Fauth%2Fsession"
        );
        // The query goes after the escaped path, so it cannot be reached from inside the id either.
        assert_eq!(
            format!("{}?delete_files={}", torrent_route("a/b", ""), true),
            "/api/v1/torrents/a%2Fb?delete_files=true"
        );
    }
}
