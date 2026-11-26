//! The adapter to fbt-lib's proxy surface: the only file in this crate that names its proxy items
//! beyond `ProxyConfig`, `ProxyKind` and `NetworkSettings` (`crate::proxy`'s own types name those
//! three). When that surface changes in a later release, this is the file to reconcile, and nothing
//! outside it has to move.
//!
//! What it hides from the rest of the product:
//!
//! * the runtime call that changes a live session's proxy and switches
//!   ([`fbt::Session::set_network_settings`], through `apply_now`);
//! * the engine's own proxy state as this product's [`ProxyHealth`], both the value read from the
//!   session ([`health`]) and the one an event carries ([`health_from_event`]).
//!
//! `ProxyState` is `#[non_exhaustive]`: a state a later fbt-lib adds is not one this product knows,
//! and an unknown state maps to [`ProxyHealth::Unreachable`] rather than to the healthy one, so a
//! user is told something is off rather than shown a green state that may not hold.

use fbt::{Event, ProxyState, Session, Settings};

use super::{ProxyHealth, ProxySetting, network_for};

/// Applies `proxy` to a running session at once, over the switches `base` describes.
///
/// `base` is the session's settings without the proxy: the network switches
/// [`network_for`] takes as today's values and overrides with what the proxy implies. Equal to what
/// the session runs already, this is a no-op. A change closes the connections made under the old
/// proxy and installs the new stack on the same addresses; no connection is made directly in
/// between. `Err` is the engine's own one line, display-safe: a proxy or switch combination it
/// refuses, a new network stack that would not build (a listen address that will not bind again,
/// say), or a session that has shut down. After an `Err` the session is still on the setting it had
/// before, so a caller that already wrote the new one down has to put the previous one back.
///
/// What the server's proxy route and the local backend's own save call, after the file has the new
/// setting and inside the same serialized save, so the engine always ends on the last one written.
pub async fn apply_now(
    session: &Session,
    proxy: Option<&ProxySetting>,
    base: &Settings,
) -> Result<(), String> {
    session
        .set_network_settings(network_for(proxy, base))
        .await
        // Display-safe, because this one line goes to a screen and into a response as it is.
        .map_err(|problem| fbt::display_safe(&problem.to_string(), fbt::MAX_DISPLAY_CHARS))
}

/// The session's proxy health as this product holds it: `Off` without a proxy, and otherwise what
/// the engine last reported. This never probes anything itself; the engine does that on its own and
/// re-tries with a backoff while the proxy is down.
pub fn health(session: &Session) -> ProxyHealth {
    from_state(session.proxy_state())
}

/// The health an event carries, when it is a proxy state change, and `None` for every other event:
/// how a screen keeps [`health`] current without polling.
pub fn health_from_event(event: &Event) -> Option<ProxyHealth> {
    match event {
        Event::ProxyStateChanged { state } => Some(from_state(state.clone())),
        _ => None,
    }
}

/// The engine's own wording for a proxy it could not reach. The reason fbt hands over already starts
/// with it (`ProxyError::Unreachable`), and this product's one line adds its own
/// "Can't reach the proxy: ", so the prefix is dropped here to keep the line from saying it twice.
const ENGINE_UNREACHABLE_PREFIX: &str = "cannot reach the proxy: ";

/// `reason` without the engine's own unreachable wording at its front, when it is there: the rest,
/// unchanged, otherwise. The comparison is ASCII case-insensitive, and the slice is taken only at a
/// character boundary, so a reason that is nothing like this is returned as it is.
fn without_engine_prefix(reason: &str) -> &str {
    if reason.is_char_boundary(ENGINE_UNREACHABLE_PREFIX.len())
        && reason[..ENGINE_UNREACHABLE_PREFIX.len()].eq_ignore_ascii_case(ENGINE_UNREACHABLE_PREFIX)
    {
        &reason[ENGINE_UNREACHABLE_PREFIX.len()..]
    } else {
        reason
    }
}

/// The engine's proxy state as this product's health.
fn from_state(state: ProxyState) -> ProxyHealth {
    match state {
        ProxyState::NotSet => ProxyHealth::Off,
        ProxyState::Reachable => ProxyHealth::Ok,
        ProxyState::UdpNotRelayed => ProxyHealth::UdpNotRelayed,
        ProxyState::LoginRefused => ProxyHealth::LoginRefused,
        ProxyState::Unreachable { reason } => ProxyHealth::Unreachable {
            // The engine's own "cannot reach the proxy: " is dropped, because this product's
            // headline adds its own "Can't reach the proxy: " and the line must not say it twice.
            // The engine already made the rest display-safe and bounded; running it through the same
            // rule again costs nothing and keeps this type's promise true whatever the reason was.
            reason: fbt::display_safe(without_engine_prefix(&reason), fbt::MAX_DISPLAY_CHARS),
        },
        // `ProxyState` is `#[non_exhaustive]`: a state a later fbt-lib adds is not one this product
        // knows, and saying so is better than a green state that may be lying.
        _ => ProxyHealth::Unreachable {
            reason: "unknown proxy state".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{from_state, health_from_event};
    use crate::proxy::ProxyHealth;
    use fbt::ProxyState;

    /// Every state this version of the engine reports has its own answer. A state a later fbt-lib
    /// adds cannot be built here (`ProxyState` is `#[non_exhaustive]`); the wildcard arm maps it to
    /// `Unreachable`, which is what a state nobody understands deserves.
    #[test]
    fn every_engine_state_maps_to_this_products_health() {
        assert_eq!(from_state(ProxyState::NotSet), ProxyHealth::Off);
        assert_eq!(from_state(ProxyState::Reachable), ProxyHealth::Ok);
        assert_eq!(
            from_state(ProxyState::UdpNotRelayed),
            ProxyHealth::UdpNotRelayed
        );
        assert_eq!(
            from_state(ProxyState::LoginRefused),
            ProxyHealth::LoginRefused
        );
        assert_eq!(
            from_state(ProxyState::Unreachable {
                reason: "connection refused".to_string()
            }),
            ProxyHealth::Unreachable {
                reason: "connection refused".to_string()
            }
        );
    }

    /// The reason is display-safe on the way through: control and bidi characters are dropped and
    /// the text is bounded, so a proxy's own error text cannot reach a screen as it is. The engine's
    /// own "cannot reach the proxy: " wording is dropped too, so the one line this product draws does
    /// not say it twice.
    #[test]
    fn an_unreachable_reason_is_made_display_safe() {
        let ProxyHealth::Unreachable { reason } = from_state(ProxyState::Unreachable {
            reason: "refused\u{202e}evoba\u{7}".to_string(),
        }) else {
            panic!("an unreachable state maps to unreachable health");
        };
        assert!(!reason.contains('\u{202e}'), "{reason:?}");
        assert!(!reason.contains('\u{7}'), "{reason:?}");

        // The engine's own wording, as fbt hands it over, is dropped before the line is drawn.
        let health = from_state(ProxyState::Unreachable {
            reason: "cannot reach the proxy: Connection refused (os error 61)".to_string(),
        });
        assert_eq!(
            health,
            ProxyHealth::Unreachable {
                reason: "Connection refused (os error 61)".to_string()
            },
            "the engine's own wording is dropped, not repeated"
        );
        let headline = health.headline().expect("a line for an unreachable proxy");
        assert_eq!(
            headline, "Can't reach the proxy: Connection refused (os error 61)",
            "the one line says it once"
        );
        assert_eq!(
            headline.matches("reach the proxy").count(),
            1,
            "the phrase is not repeated: {headline}"
        );
    }

    /// Only the proxy state's own event carries health; every other event says nothing about it.
    #[test]
    fn only_a_proxy_state_change_carries_health() {
        assert_eq!(
            health_from_event(&fbt::Event::ProxyStateChanged {
                state: ProxyState::Reachable
            }),
            Some(ProxyHealth::Ok)
        );
        assert_eq!(
            health_from_event(&fbt::Event::Lagged { missed: 1 }),
            None,
            "an unrelated event is not a proxy state"
        );
    }
}
