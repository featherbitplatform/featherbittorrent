//! The main window stays on the app: a navigation off the app's own origin is refused.
//!
//! The page is served from `tauri://localhost` (macOS and Linux) or `http://tauri.localhost` (Windows),
//! and in a dev build that has one, from the config's `devUrl`. A link to anywhere else, followed in the
//! main webview, would replace the whole app with that address: the title bar, the switcher and on
//! Windows and Linux even the window's only close button go with it. The page never means to do that (it
//! talks to engines with `fetch`, and the player's Download goes through the shell, [`crate::download`]),
//! so whatever tries is stopped here, whatever it was.
//!
//! The windows come from `tauri.conf.json`, so there is no `WebviewWindowBuilder` to put a handler on.
//! Tauri calls every plugin's `on_navigation` hook for every webview (`tauri::plugin::Builder::on_navigation`),
//! and returning `false` cancels the navigation; this module is that plugin.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Runtime, Url};

/// The plugin's name, as Tauri lists it.
const NAME: &str = "navigation-guard";

/// The origins the app's page is served from: the built-in one for this system, and `dev_url` when this
/// is a dev build that loads its page from there.
pub fn own_origins(dev_url: Option<&Url>) -> Vec<Url> {
    let mut origins =
        vec![Url::parse(crate::local::desktop_origin()).expect("the app's own origin parses")];
    if tauri::is_dev()
        && let Some(dev_url) = dev_url
    {
        origins.push(dev_url.clone());
    }
    origins
}

/// Whether the main window may go to `url`: only when it is on one of `own` origins, compared by scheme,
/// host and port. Compared by hand rather than with [`Url::origin`], which gives `tauri://localhost` an
/// opaque origin that equals nothing, itself included.
pub fn allows(url: &Url, own: &[Url]) -> bool {
    own.iter().any(|origin| {
        origin.scheme() == url.scheme()
            && origin.host_str().is_some()
            && origin.host_str() == url.host_str()
            && origin.port_or_known_default() == url.port_or_known_default()
    })
}

/// The plugin that refuses every navigation [`allows`] does not let through. A refusal is said on stderr
/// without the address, which may carry a token.
pub fn guard<R: Runtime>(own: Vec<Url>) -> TauriPlugin<R> {
    Builder::new(NAME)
        .on_navigation(move |_webview, url| {
            let allowed = allows(url, &own);
            if !allowed {
                eprintln!("A link tried to take the window off the app, and was refused.");
            }
            allowed
        })
        .build()
}

#[cfg(test)]
mod tests {
    use tauri::Url;

    use super::allows;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("a test URL")
    }

    #[test]
    fn the_apps_own_origins_are_allowed() {
        let own = [url("tauri://localhost"), url("http://tauri.localhost")];
        assert!(allows(&url("tauri://localhost"), &own));
        assert!(allows(&url("tauri://localhost/index.html#/settings"), &own));
        assert!(allows(&url("http://tauri.localhost/"), &own));
        assert!(allows(&url("http://tauri.localhost:80/index.html"), &own));

        let dev = [url("http://localhost:5173/")];
        assert!(allows(&url("http://localhost:5173/src/main.tsx"), &dev));
    }

    #[test]
    fn everything_else_is_refused() {
        let own = [url("tauri://localhost"), url("http://tauri.localhost")];
        for other in [
            "http://127.0.0.1:53211/api/v1/torrents/x/files/0/stream?token=t",
            "https://example.com/",
            "https://tauri.localhost/",
            "http://tauri.localhost:8080/",
            "http://tauri.localhost.example.com/",
            "tauri://localhost.example.com/",
            "tauri://otherhost/",
            "file:///etc/passwd",
            "about:blank",
            "data:text/html,hello",
            "blob:tauri://localhost/1234",
            "javascript:alert(1)",
        ] {
            assert!(!allows(&url(other), &own), "{other} was allowed");
        }
        assert!(!allows(
            &url("http://localhost:5174/"),
            &[url("http://localhost:5173/")]
        ));
        assert!(!allows(&url("tauri://localhost"), &[]));
    }
}
