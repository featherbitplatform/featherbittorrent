//! The proxy setting: what `proxy:` in `config.yaml` means, what it refuses, and the engine
//! settings it implies.
//!
//! A proxy is not only an address to send through; it is a promise that *everything* goes through
//! it. [`ProxySetting`] is the product's shape of the file's `proxy:` section, and it is the one
//! place that decides what the engine may run while a proxy is set:
//!
//! * [`ProxySetting::validate`] refuses a section a run cannot act on (no host, a port of 0, a
//!   credential longer than SOCKS5's 255 bytes, a type this product does not know), with the
//!   sentence the screens show ([`ProxyProblem::message`]).
//! * [`ProxySetting::to_engine`] hands the engine a proxy that forbids every bypass (`proxy_only`),
//!   so nothing leaves from the host's own address whatever the switches say.
//! * [`network_for`] and [`apply_to_settings`] derive the network switches a proxy implies
//!   (section 3.3 of the design): incoming connections, port forwarding and local peer discovery go
//!   off, and with an HTTP proxy - which carries TCP only - the DHT and outgoing uTP too.
//!
//! [`ProxyHealth`] is the engine's report on that proxy, in the product's own words, and
//! [`ProxyHealth::headline`] is the one line the screens show when it is not healthy.
//!
//! The one file that names fbt-lib's proxy surface beyond the three names above is
//! [`engine`], the adapter to be reconciled when that surface changes.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The adapter to fbt-lib's proxy surface: the only file that names its proxy items beyond
/// `ProxyConfig`, `ProxyKind` and `NetworkSettings`.
pub mod engine;

// The two questions a screen asks of a session, where a caller looks for the proxy's own words
// rather than the adapter's path: `fbtt_core::proxy::health(&session)`.
pub use engine::{apply_now, health, health_from_event};

/// The longest proxy host this product accepts, in characters: the DNS name limit.
const MAX_HOST_CHARS: usize = 253;
/// The longest user name or password SOCKS5 can carry (RFC 1929), in bytes.
const MAX_CREDENTIAL_BYTES: usize = 255;

/// Which proxy protocol a `proxy:` section names.
///
/// `none` means no proxy at all, which is the same as having no section. A word this product does
/// not know - `socks4`, say - is kept as [`ProxyType::Other`] rather than failing the whole parse,
/// so that it reaches [`ProxySetting::validate`] and is refused with a sentence a person can act on
/// instead of a YAML error that names a line and nothing else. It never reaches the engine: a
/// config carrying one is refused at load, and `ProxySetting::engine_kind` falls back to SOCKS5
/// so that even a bug that got that far sends the traffic to the named proxy rather than direct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyType {
    /// No proxy: connections are made directly.
    None,
    /// An HTTP proxy, used through `CONNECT` (TCP only).
    Http,
    /// A SOCKS5 proxy.
    Socks5,
    /// A word this version does not know, kept as it was written for the refusal to name it.
    Other(String),
}

impl Serialize for ProxyType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            ProxyType::None => "none",
            ProxyType::Http => "http",
            ProxyType::Socks5 => "socks5",
            // Written back exactly as it was read, so a save does not turn a typo into a different
            // typo and the person who fixes the line sees the word they wrote.
            ProxyType::Other(name) => name.as_str(),
        })
    }
}

impl<'de> Deserialize<'de> for ProxyType {
    /// Reads `type:` as the string it is: a plain string is matched case-insensitively against the
    /// three known words, and anything else becomes [`ProxyType::Other`] with its original spelling.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TypeVisitor;

        impl serde::de::Visitor<'_> for TypeVisitor {
            type Value = ProxyType;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a proxy type: none, http or socks5")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<ProxyType, E> {
                Ok(match value.to_ascii_lowercase().as_str() {
                    "none" => ProxyType::None,
                    "http" => ProxyType::Http,
                    "socks5" => ProxyType::Socks5,
                    _ => ProxyType::Other(value.to_string()),
                })
            }
        }

        deserializer.deserialize_str(TypeVisitor)
    }
}

/// What is wrong with a `proxy:` section, in the words the screens show.
///
/// Every sentence is product copy (the plan's Global Constraints): no emoji, no em-dashes, and one
/// line each. [`message`](Self::message) is the only place they are written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyProblem {
    /// The host is missing, empty, has whitespace in it, or is spelled as a URL (`http://host`).
    NoHost,
    /// The port is 0: a proxy needs a port to dial.
    BadPort,
    /// The user name or the password is longer than SOCKS5 can carry (255 bytes).
    TooLong,
    /// The type is a word this product does not know, kept as written.
    UnknownType(String),
    /// A SOCKS5 proxy named only one half of a login: SOCKS5 has both or neither.
    IncompleteLogin,
    /// A password was given with an empty or missing user name: a login, HTTP or SOCKS5, names
    /// its user.
    LoginNeedsUser,
    /// An HTTP user name carries a `:`: HTTP Basic splits on it, so it cannot.
    UsernameColon,
    /// A top-level key spells `proxy` in another case (`Proxy:`, `PROXY:`): the parser keeps it as
    /// an unknown key, so the run would otherwise go without the proxy it names.
    KeyCase,
    /// A file that mentions a proxy has a top-level YAML merge (`<<`): the parser does not apply
    /// merges, so a proxy carried by one would be dropped.
    MergeKey,
    /// The engine refuses the proxy this product built, with fbt's own words. The last word, for a
    /// case this product's own checks do not cover.
    Engine(String),
}

impl ProxyProblem {
    /// The one line to show the person who wrote this setting.
    pub fn message(&self) -> String {
        match self {
            ProxyProblem::NoHost => "Enter the proxy's host name or address.".to_string(),
            ProxyProblem::BadPort => "The port must be a number from 1 to 65535.".to_string(),
            ProxyProblem::TooLong => {
                "The user name and password can be at most 255 bytes each.".to_string()
            }
            ProxyProblem::UnknownType(name) => {
                // The word is quoted so an empty one is visible as `""` rather than a gap, and it is
                // made display-safe first: it came from a file.
                format!(
                    "Unknown proxy type \"{}\"; use none, http or socks5.",
                    fbt::display_safe(name, fbt::MAX_DISPLAY_CHARS)
                )
            }
            ProxyProblem::IncompleteLogin => {
                "A SOCKS5 login needs both a user name and a password.".to_string()
            }
            ProxyProblem::LoginNeedsUser => "A proxy login needs a user name.".to_string(),
            ProxyProblem::UsernameColon => "The user name cannot contain a colon.".to_string(),
            ProxyProblem::KeyCase => "Write the proxy setting as proxy: in lower case.".to_string(),
            ProxyProblem::MergeKey => {
                "Write the proxy: section out in full; a YAML merge (<<) cannot carry it."
                    .to_string()
            }
            ProxyProblem::Engine(problem) => {
                // The engine's own sentence, made display-safe in case its wording ever carries a
                // character a screen should not draw.
                format!(
                    "The proxy setting is not valid: {}",
                    fbt::display_safe(problem, fbt::MAX_DISPLAY_CHARS)
                )
            }
        }
    }
}

/// The `proxy:` section of `config.yaml`, as this product holds it.
///
/// Absent, or `type: none`, means no proxy. `host` and `port` are the address; `username` and
/// `password` are optional and travel with the proxy (SOCKS5 username/password authentication, or
/// HTTP `Proxy-Authorization: Basic`). The password is a secret: this type's [`Debug`] prints it as
/// `<redacted>`, so a config logged, dumped in an error or shown in a panic message carries no
/// password (fbt-lib's own `ProxyConfig` hides it too, as `***`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySetting {
    /// Which protocol, as `type:` in the file.
    #[serde(rename = "type")]
    pub kind: ProxyType,
    /// The proxy's host name or address, as written. A missing key reads as the empty string, which
    /// [`validate`](Self::validate) refuses: the file that means "no proxy" says `type: none`.
    #[serde(default)]
    pub host: String,
    /// The proxy's port, as written. A missing key reads as 0, which
    /// [`validate`](Self::validate) refuses for the same reason.
    #[serde(default)]
    pub port: u16,
    /// The user name to authenticate with, when the proxy wants one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// The password to authenticate with. Written in clear text to `config.yaml` (the file already
    /// holds other secrets) and printed nowhere: see the type's `Debug`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl ProxySetting {
    /// Whether this section names a proxy at all: `type: none` (or an absent section) does not, and
    /// neither does an empty type with no host, which is an empty section.
    pub fn is_set(&self) -> bool {
        match &self.kind {
            ProxyType::None => false,
            ProxyType::Other(name) => !(name.is_empty() && self.host.is_empty()),
            ProxyType::Http | ProxyType::Socks5 => true,
        }
    }

    /// Checks that a run can act on this section: the type is one this product knows, the host is a
    /// name or an address (1 to 253 characters, no whitespace, not spelled as a URL), the port is
    /// not 0, and the credentials can be sent (at most 255 bytes each; a password needs a user name;
    /// a SOCKS5 login is both halves or neither; an HTTP user name carries no `:`). The engine's own check runs last, so a case
    /// this product does not cover is still refused - with fbt's sentence rather than engine-worded
    /// text at start.
    pub fn validate(&self) -> Result<(), ProxyProblem> {
        match &self.kind {
            // No proxy: nothing else in the section is used, so nothing else can be wrong with it.
            ProxyType::None => return Ok(()),
            // An empty type naming no host is an empty section: no proxy, nothing to refuse.
            ProxyType::Other(name) if name.is_empty() && self.host.is_empty() => return Ok(()),
            ProxyType::Other(name) => return Err(ProxyProblem::UnknownType(name.clone())),
            ProxyType::Http | ProxyType::Socks5 => {}
        }
        // The host is checked and dialled without one surrounding pair of brackets, so an IPv6
        // literal spelled for a URI (`[::1]`) is a host this product accepts; the stored setting
        // keeps the brackets as typed.
        let host = unbracketed(&self.host);
        if host.is_empty()
            || host.chars().count() > MAX_HOST_CHARS
            || host.contains("://")
            || host.chars().any(char::is_whitespace)
        {
            // A host that is empty, too long, a URL, or has a space in it is not a host to dial; the
            // one sentence says what to type instead.
            return Err(ProxyProblem::NoHost);
        }
        if self.port == 0 {
            return Err(ProxyProblem::BadPort);
        }
        // The length check comes before the login's shape, so a lone over-long credential is the
        // length that is wrong with it, not the half-login.
        if self
            .username
            .as_ref()
            .is_some_and(|user| user.len() > MAX_CREDENTIAL_BYTES)
            || self
                .password
                .as_ref()
                .is_some_and(|pass| pass.len() > MAX_CREDENTIAL_BYTES)
        {
            return Err(ProxyProblem::TooLong);
        }
        let user = self.username.as_deref().filter(|user| !user.is_empty());
        let pass = self.password.as_deref().filter(|pass| !pass.is_empty());
        // A password names a login, and a login names its user, for HTTP as much as SOCKS5: an
        // empty user name would otherwise reach the engine as `Some("")`.
        if pass.is_some() && user.is_none() {
            return Err(ProxyProblem::LoginNeedsUser);
        }
        // A SOCKS5 login is both halves or neither: a user name without a password is a login the
        // engine would refuse at start.
        if matches!(self.kind, ProxyType::Socks5) && user.is_some() != pass.is_some() {
            return Err(ProxyProblem::IncompleteLogin);
        }
        // HTTP Basic splits the user name on `:`, so a user name that carries one cannot be sent.
        if matches!(self.kind, ProxyType::Http) && user.is_some_and(|user| user.contains(':')) {
            return Err(ProxyProblem::UsernameColon);
        }
        // The engine's own check, last: whatever this product's checks did not cover, fbt's does, so
        // a config never passes loading and then fails at start with engine-worded text.
        if let Err(problem) = self.to_engine().validate() {
            return Err(ProxyProblem::Engine(problem));
        }
        Ok(())
    }

    /// The engine's proxy config for this setting: the address and credentials as written, all three
    /// routing switches on, and `proxy_only` set so the engine treats every bypass as off. A value
    /// here is only ever reached after [`validate`](Self::validate) accepted it (a config that names
    /// an unknown type is refused at load), and an unknown type falls back to SOCKS5 so that nothing
    /// can fall through to a direct connection.
    ///
    /// The host loses one surrounding pair of brackets (an IPv6 literal spelled for a URI), and an
    /// empty login becomes no login: empty credentials are not sent, the way the engine expects.
    pub fn to_engine(&self) -> fbt::ProxyConfig {
        let mut config = fbt::ProxyConfig::default();
        config.kind = self.engine_kind();
        config.host = unbracketed(&self.host).to_string();
        config.port = self.port;
        let user = self.username.as_deref().filter(|user| !user.is_empty());
        let pass = self.password.as_deref().filter(|pass| !pass.is_empty());
        if user.is_none() && pass.is_none() {
            // Both empty (or absent) is no login at all; the engine must not be handed a blank one.
            config.username = None;
            config.password = None;
        } else {
            config.username = self.username.clone();
            config.password = self.password.clone();
        }
        config.proxy_peers = true;
        config.proxy_trackers = true;
        config.proxy_hostnames = true;
        config.proxy_only = true;
        config
    }

    /// This setting's [`ProxyType`] as the engine's kind. `Other` never comes from a config a run
    /// acts on (see [`ProxyType`]): an empty one is no proxy, and any other maps to SOCKS5 so the
    /// fallback is still a proxy rather than nothing.
    fn engine_kind(&self) -> fbt::ProxyKind {
        match &self.kind {
            ProxyType::None => fbt::ProxyKind::None,
            ProxyType::Http => fbt::ProxyKind::Http,
            ProxyType::Other(name) if name.is_empty() => fbt::ProxyKind::None,
            ProxyType::Socks5 | ProxyType::Other(_) => fbt::ProxyKind::Socks5,
        }
    }
}

/// A host as written without one surrounding pair of brackets: `[::1]` becomes `::1`, so an IPv6
/// literal spelled the way a URI spells it is accepted and dialled bare. A string that is not a
/// bracketed pair - including one with only one bracket - is returned as it is.
fn unbracketed(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

impl fmt::Debug for ProxySetting {
    /// Prints the password as `<redacted>`: a config that reaches a log line, an error or a panic
    /// message must not carry the password out with it.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProxySetting")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// The engine's network settings for `proxy`, derived from `base`.
///
/// With no proxy, or a section whose `type` is `none`, this is `base`'s switches unchanged and the
/// engine's default proxy (not whatever `base` carried): a run without a proxy behaves exactly as it
/// did before this setting existed. With a proxy set, the switches
/// section 3.3 of the design turns off go off - incoming TCP and uTP, UPnP, NAT-PMP and local peer
/// discovery, which cannot go through a proxy - and with an HTTP proxy, which carries TCP only, the
/// DHT and outgoing uTP as well. A SOCKS5 proxy keeps `base`'s DHT and uTP switches: the engine
/// relays them when the proxy supports UDP and turns them off itself when it does not, so this
/// setting does not have to guess.
pub fn network_for(proxy: Option<&ProxySetting>, base: &fbt::Settings) -> fbt::NetworkSettings {
    let mut network = base.network();
    let Some(setting) = proxy.filter(|setting| setting.is_set()) else {
        // No proxy: the engine's default, whatever `base` carried. A base that names a proxy of its
        // own must not leak it into a run that has none.
        network.proxy = fbt::ProxyConfig::default();
        return network;
    };
    network.proxy = setting.to_engine();
    network.enable_incoming_tcp = false;
    network.enable_incoming_utp = false;
    network.enable_upnp = false;
    network.enable_natpmp = false;
    network.enable_lsd = false;
    if matches!(setting.kind, ProxyType::Http) {
        network.dht_enabled = false;
        network.enable_outgoing_utp = false;
    }
    network
}

/// Puts the network settings `proxy` implies onto `settings`, in place: what
/// [`Config::session_settings`](crate::Config::session_settings) calls, and what a runtime change
/// applies to a live session through `engine::apply_now`.
pub fn apply_to_settings(settings: &mut fbt::Settings, proxy: Option<&ProxySetting>) {
    let network = network_for(proxy, settings);
    settings.set_network(network);
}

/// The health of the session's proxy, in this product's own words.
///
/// The engine reports a session-level proxy state (fbt-lib's `ProxyState`); this is that state as
/// the screens hold it: `Off` means no proxy is set, `Ok` means the proxy answered, and the other
/// three are the ways it is not working. The `reason` is display-safe and bounded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProxyHealth {
    /// No proxy is set: everything goes direct, as it does without this setting at all.
    Off,
    /// The proxy answered.
    Ok,
    /// The proxy could not be reached, for the reason given (display-safe).
    Unreachable {
        /// Why, fit to draw.
        reason: String,
    },
    /// The proxy refused the credentials, or wanted some and none were set.
    LoginRefused,
    /// The proxy answers TCP but will not relay UDP, so the DHT, uTP and UDP trackers are off.
    UdpNotRelayed,
}

impl ProxyHealth {
    /// The one line to show when the proxy is not working, or `None` when there is nothing to say:
    /// `Off` and `Ok` are both quiet, and `UdpNotRelayed` has its own explanation beside the proxy
    /// fields rather than in the status line.
    pub fn headline(&self) -> Option<String> {
        match self {
            ProxyHealth::Unreachable { reason } if !reason.is_empty() => {
                Some(format!("Can't reach the proxy: {reason}"))
            }
            ProxyHealth::Unreachable { .. } => Some("Can't reach the proxy".to_string()),
            ProxyHealth::LoginRefused => {
                Some("Can't reach the proxy: it refused the login".to_string())
            }
            ProxyHealth::Off | ProxyHealth::Ok | ProxyHealth::UdpNotRelayed => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProxyHealth, ProxyProblem, ProxySetting, ProxyType, apply_to_settings, network_for,
    };

    /// A SOCKS5 proxy at `host:port`, with credentials when they are given.
    fn socks5(host: &str, port: u16) -> ProxySetting {
        ProxySetting {
            kind: ProxyType::Socks5,
            host: host.to_string(),
            port,
            username: None,
            password: None,
        }
    }

    /// The type reads from the string the file has it as, whatever case it is written in, and a
    /// word this version does not know is kept rather than failing the parse - so `socks4` reaches
    /// validation and is refused with the sentence that names it.
    #[test]
    fn proxy_type_reads_any_word_from_a_string() {
        let read = |text: &str| -> ProxySetting {
            serde_norway::from_str(text).expect("a proxy section reads")
        };
        let none: ProxySetting =
            serde_norway::from_str("type: none\n").expect("the word none reads");
        assert_eq!(none.kind, ProxyType::None);
        assert_eq!(read("type: http\n").kind, ProxyType::Http);
        assert_eq!(read("type: SOCKS5\n").kind, ProxyType::Socks5);
        assert_eq!(
            read("type: socks4\n").kind,
            ProxyType::Other("socks4".to_string()),
            "a type this version does not know is kept for the refusal to name it"
        );
        // It is written back as it was read: a save does not turn one typo into another.
        let written =
            serde_norway::to_string(&read("type: socks4\n")).expect("a proxy section writes");
        assert!(written.contains("socks4"), "{written}");
    }

    /// A section that names no host or no port reads (the keys are optional), and validation is
    /// what refuses it: a file that means "no proxy" says `type: none`.
    #[test]
    fn a_partial_section_reads_and_validation_refuses_it() {
        let section: ProxySetting =
            serde_norway::from_str("type: socks5\n").expect("the type reads");
        assert_eq!(section.host, "");
        assert_eq!(section.port, 0);
        assert_eq!(section.validate(), Err(ProxyProblem::NoHost));
        let none: ProxySetting =
            serde_norway::from_str("type: none\n").expect("the word none reads alone");
        assert_eq!(none.validate(), Ok(()), "no proxy is nothing to validate");
        assert!(!none.is_set());
    }

    /// Every sentence a refusal can be, and the `Debug` that hides the one secret in this type.
    #[test]
    fn validate_table() {
        assert_eq!(socks5("", 1080).validate(), Err(ProxyProblem::NoHost));
        assert_eq!(
            socks5("http://h", 1080).validate(),
            Err(ProxyProblem::NoHost),
            "a URL is not a host"
        );
        assert_eq!(
            socks5("two words", 1080).validate(),
            Err(ProxyProblem::NoHost)
        );
        assert_eq!(
            socks5(&"a".repeat(254), 1080).validate(),
            Err(ProxyProblem::NoHost),
            "a host longer than a DNS name is not one"
        );
        assert_eq!(socks5("h", 0).validate(), Err(ProxyProblem::BadPort));
        assert_eq!(socks5("h", 65535).validate(), Ok(()));
        assert_eq!(
            ProxySetting {
                password: Some("p".repeat(256)),
                ..socks5("h", 1080)
            }
            .validate(),
            Err(ProxyProblem::TooLong)
        );
        assert_eq!(
            ProxySetting {
                username: Some("u".repeat(256)),
                ..socks5("h", 1080)
            }
            .validate(),
            Err(ProxyProblem::TooLong)
        );
        assert_eq!(
            ProxySetting {
                kind: ProxyType::Other("socks4".to_string()),
                ..socks5("h", 1080)
            }
            .validate(),
            Err(ProxyProblem::UnknownType("socks4".to_string()))
        );
        // An empty type with no host is an empty section: no proxy, nothing to refuse; with a host
        // it is a type this product does not know, named as the empty word it is.
        let empty_type = ProxySetting {
            kind: ProxyType::Other(String::new()),
            ..socks5("", 1080)
        };
        assert_eq!(empty_type.validate(), Ok(()));
        assert!(!empty_type.is_set());
        assert_eq!(
            ProxySetting {
                kind: ProxyType::Other(String::new()),
                ..socks5("h", 1080)
            }
            .validate(),
            Err(ProxyProblem::UnknownType(String::new()))
        );
        // `type: none` accepts anything else in the section: none of it is used.
        assert_eq!(
            ProxySetting {
                kind: ProxyType::None,
                host: String::new(),
                port: 0,
                username: Some("u".repeat(300)),
                password: Some("p".repeat(300)),
            }
            .validate(),
            Ok(())
        );

        let message = |problem: ProxyProblem| problem.message();
        assert_eq!(
            message(ProxyProblem::NoHost),
            "Enter the proxy's host name or address."
        );
        assert_eq!(
            message(ProxyProblem::BadPort),
            "The port must be a number from 1 to 65535."
        );
        assert_eq!(
            message(ProxyProblem::TooLong),
            "The user name and password can be at most 255 bytes each."
        );
        assert_eq!(
            message(ProxyProblem::UnknownType("socks4".to_string())),
            "Unknown proxy type \"socks4\"; use none, http or socks5."
        );
        assert_eq!(
            message(ProxyProblem::UnknownType(String::new())),
            "Unknown proxy type \"\"; use none, http or socks5."
        );
        assert_eq!(
            message(ProxyProblem::IncompleteLogin),
            "A SOCKS5 login needs both a user name and a password."
        );
        assert_eq!(
            message(ProxyProblem::UsernameColon),
            "The user name cannot contain a colon."
        );
        assert_eq!(
            message(ProxyProblem::Engine(
                "proxy host contains brackets".to_string()
            )),
            "The proxy setting is not valid: proxy host contains brackets"
        );
    }

    /// This product's validation agrees with the engine's: the cases where they could drift - a
    /// bracketed IPv6 literal, an empty login, a SOCKS5 half login and an HTTP user name with a
    /// colon - are each pinned, and every shape this product accepts the engine accepts too.
    #[test]
    fn proxy_validation_matches_the_engine() {
        // A bracketed IPv6 literal is a host this product accepts, and the engine gets it bare.
        let bracketed = socks5("[2001:db8::1]", 1080);
        assert_eq!(
            bracketed.validate(),
            Ok(()),
            "a bracketed IPv6 host is accepted"
        );
        assert_eq!(
            bracketed.to_engine().host,
            "2001:db8::1",
            "the brackets come off"
        );
        assert_eq!(bracketed.to_engine().validate(), Ok(()));
        let loopback = socks5("[::1]", 1080);
        assert_eq!(loopback.validate(), Ok(()));
        assert_eq!(loopback.to_engine().host, "::1");
        assert_eq!(loopback.to_engine().validate(), Ok(()));

        // An empty login is no login: accepted, and the engine is handed no credentials.
        let empty_login = ProxySetting {
            username: Some(String::new()),
            password: Some(String::new()),
            ..socks5("proxy.example", 1080)
        };
        assert_eq!(empty_login.validate(), Ok(()), "an empty login is no login");
        assert_eq!(empty_login.to_engine().username, None);
        assert_eq!(empty_login.to_engine().password, None);

        // A SOCKS5 login is both halves or neither, with this product's own sentence.
        let half_login = ProxySetting {
            username: Some("alice".to_string()),
            password: None,
            ..socks5("proxy.example", 1080)
        };
        assert_eq!(half_login.validate(), Err(ProxyProblem::IncompleteLogin));
        assert_eq!(
            half_login.validate().unwrap_err().message(),
            "A SOCKS5 login needs both a user name and a password."
        );

        // A password with an empty user name is refused for both types, with its own sentence: the
        // engine would otherwise be handed an empty user name.
        for kind in [ProxyType::Http, ProxyType::Socks5] {
            for username in [None, Some(String::new())] {
                let no_user = ProxySetting {
                    kind: kind.clone(),
                    username: username.clone(),
                    password: Some("hunter2".to_string()),
                    ..socks5("proxy.example", 1080)
                };
                assert_eq!(
                    no_user.validate(),
                    Err(ProxyProblem::LoginNeedsUser),
                    "{kind:?} with user name {username:?}"
                );
                assert_eq!(
                    no_user.validate().unwrap_err().message(),
                    "A proxy login needs a user name."
                );
            }
        }

        // An HTTP user name cannot carry a colon, with this product's own sentence.
        let user_colon = ProxySetting {
            kind: ProxyType::Http,
            username: Some("a:b".to_string()),
            password: Some("pw".to_string()),
            ..socks5("proxy.example", 3128)
        };
        assert_eq!(user_colon.validate(), Err(ProxyProblem::UsernameColon));
        assert_eq!(
            user_colon.validate().unwrap_err().message(),
            "The user name cannot contain a colon."
        );

        // Everything this product accepts, the engine accepts: no drift between the two checks.
        let accepted = [
            socks5("proxy.example", 1080),
            socks5("[2001:db8::1]", 1080),
            socks5("[::1]", 1080),
            empty_login,
            ProxySetting {
                username: Some("alice".to_string()),
                password: Some("hunter2".to_string()),
                ..socks5("proxy.example", 1080)
            },
            ProxySetting {
                kind: ProxyType::Http,
                username: Some("alice".to_string()),
                password: Some("hunter2".to_string()),
                ..socks5("proxy.example", 3128)
            },
        ];
        for setting in accepted {
            assert_eq!(setting.validate(), Ok(()), "{setting:?}");
            assert_eq!(
                setting.to_engine().validate(),
                Ok(()),
                "the engine accepts what this product does: {setting:?}"
            );
        }
    }

    /// A base that already carries a proxy does not leak it into a run that has none: `network_for`
    /// with no proxy is the engine's default proxy, whatever the base said.
    #[test]
    fn no_proxy_drops_a_base_proxy() {
        let mut base = fbt::Settings::default();
        let mut carried = base.network();
        carried.proxy = socks5("proxy.example", 1080).to_engine();
        assert_ne!(
            carried.proxy,
            fbt::ProxyConfig::default(),
            "the base carries a proxy"
        );
        base.set_network(carried);

        let derived = network_for(None, &base);
        assert_eq!(
            derived.proxy,
            fbt::ProxyConfig::default(),
            "no proxy means no proxy, whatever the base carried"
        );
    }

    /// The engine never sees the password in a log line: this type's `Debug` prints `<redacted>`,
    /// and so does a whole config's, because the config's own `Debug` goes through this one.
    #[test]
    fn debug_hides_the_password() {
        let setting = ProxySetting {
            username: Some("alice".to_string()),
            password: Some("hunter2".to_string()),
            ..socks5("proxy.example", 1080)
        };
        let shown = format!("{setting:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
        assert!(
            shown.contains("alice"),
            "the user name is not a secret: {shown}"
        );

        let config = crate::Config {
            proxy: Some(setting),
            ..crate::Config::default()
        };
        let dumped = format!("{config:?}");
        assert!(!dumped.contains("hunter2"), "{dumped}");
        assert!(dumped.contains("<redacted>"), "{dumped}");
    }

    /// The engine config carries the address and credentials, and forbids every bypass: the three
    /// routing switches on and `proxy_only` set.
    #[test]
    fn to_engine_forces_every_connection_through_the_proxy() {
        let setting = ProxySetting {
            kind: ProxyType::Http,
            host: "proxy.example".to_string(),
            port: 3128,
            username: Some("alice".to_string()),
            password: Some("hunter2".to_string()),
        };
        let engine = setting.to_engine();
        assert_eq!(engine.kind, fbt::ProxyKind::Http);
        assert_eq!(engine.host, "proxy.example");
        assert_eq!(engine.port, 3128);
        assert_eq!(engine.username.as_deref(), Some("alice"));
        assert_eq!(engine.password.as_deref(), Some("hunter2"));
        assert!(engine.proxy_peers, "peer connections go through the proxy");
        assert!(engine.proxy_trackers, "tracker requests go through it");
        assert!(engine.proxy_hostnames, "names are resolved by the proxy");
        assert!(engine.proxy_only, "nothing leaves from this machine");
    }

    /// With no proxy - or `type: none` - the derived settings are the base's, field for field: a run
    /// without a proxy is today's run.
    #[test]
    fn no_proxy_changes_nothing() {
        let mut base = fbt::Settings::default();
        base.dht_enabled = true;
        base.enable_lsd = true;
        base.enable_upnp = true;
        base.enable_natpmp = true;
        base.enable_incoming_tcp = true;
        base.enable_incoming_utp = true;
        base.enable_outgoing_utp = true;
        let today = base.network();
        assert_eq!(network_for(None, &base), today);
        let none = ProxySetting {
            kind: ProxyType::None,
            host: String::new(),
            port: 0,
            username: None,
            password: None,
        };
        assert_eq!(network_for(Some(&none), &base), today);
        // And it went through `set_network` unchanged, so the settings a run starts on are untouched.
        let mut applied = base.clone();
        apply_to_settings(&mut applied, Some(&none));
        assert_eq!(applied.network(), today);
    }

    /// A SOCKS5 proxy turns off what cannot go through it but keeps the DHT and uTP switches: the
    /// engine relays them when the proxy carries UDP and turns them off itself when it does not.
    #[test]
    fn socks5_turns_off_incoming_and_discovery() {
        let base = fbt::Settings::default();
        let network = network_for(Some(&socks5("proxy.example", 1080)), &base);
        assert!(!network.enable_incoming_tcp);
        assert!(!network.enable_incoming_utp);
        assert!(!network.enable_lsd);
        assert!(!network.enable_upnp);
        assert!(!network.enable_natpmp);
        assert!(network.dht_enabled, "a SOCKS5 proxy keeps the DHT switch");
        assert!(
            network.enable_outgoing_utp,
            "a SOCKS5 proxy keeps the outgoing uTP switch"
        );
        assert_eq!(network.proxy.kind, fbt::ProxyKind::Socks5);
    }

    /// An HTTP proxy carries TCP only, so the DHT and outgoing uTP go off with the rest.
    #[test]
    fn http_also_turns_off_dht_and_utp() {
        let setting = ProxySetting {
            kind: ProxyType::Http,
            ..socks5("proxy.example", 3128)
        };
        let network = network_for(Some(&setting), &fbt::Settings::default());
        assert!(!network.dht_enabled);
        assert!(!network.enable_outgoing_utp);
        assert_eq!(network.proxy.kind, fbt::ProxyKind::Http);
    }

    /// The headline is the two "Can't reach the proxy" lines or nothing: the quiet states say
    /// nothing, and a UDP-less proxy has its own sentence elsewhere.
    #[test]
    fn the_headline_only_speaks_when_the_proxy_is_not_working() {
        assert_eq!(ProxyHealth::Off.headline(), None);
        assert_eq!(ProxyHealth::Ok.headline(), None);
        assert_eq!(ProxyHealth::UdpNotRelayed.headline(), None);
        assert_eq!(
            ProxyHealth::Unreachable {
                reason: "connection refused".to_string()
            }
            .headline()
            .as_deref(),
            Some("Can't reach the proxy: connection refused")
        );
        assert_eq!(
            ProxyHealth::Unreachable {
                reason: String::new()
            }
            .headline()
            .as_deref(),
            Some("Can't reach the proxy")
        );
        assert_eq!(
            ProxyHealth::LoginRefused.headline().as_deref(),
            Some("Can't reach the proxy: it refused the login")
        );
    }

    /// The health a screen holds is the shape the API sends: a tag and a reason.
    #[test]
    fn proxy_health_serialises_with_its_state_tag() {
        let off = serde_json::to_string(&ProxyHealth::Off).expect("health writes");
        assert_eq!(off, r#"{"state":"off"}"#);
        let unreachable = serde_json::to_string(&ProxyHealth::Unreachable {
            reason: "refused".to_string(),
        })
        .expect("health writes");
        assert_eq!(unreachable, r#"{"state":"unreachable","reason":"refused"}"#);
        let back: ProxyHealth =
            serde_json::from_str(&unreachable).expect("health reads back as it was written");
        assert_eq!(
            back,
            ProxyHealth::Unreachable {
                reason: "refused".to_string()
            }
        );
    }
}
