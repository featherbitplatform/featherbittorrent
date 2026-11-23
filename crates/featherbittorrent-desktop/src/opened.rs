//! The `magnet:` links and `.torrent` files the system opened the app with, handed to the page.
//!
//! They arrive four ways: the first launch's own arguments, a second launch's arguments (the
//! single-instance plugin forwards them to this copy, which shows its window, and the second copy exits
//! without starting anything), and on macOS the system's open-URL event, which is how a magnet clicked
//! in a browser and a `.torrent` double-clicked in the Finder reach an app that is already running (the
//! deep-link plugin turns it into `deep-link://new-url`). Each way ends in [`OpenedState::deliver`].
//!
//! What the arguments name is read by `fbtt_desktop_core::handoff` (magnets and existing `.torrent`
//! files only, a fixed bound on both). Here a file becomes its bytes: a `.torrent` of at most
//! [`MAX_TORRENT_FILE_BYTES`] goes to the page as base64, and a larger one, or one that cannot be read,
//! goes as a refusal the page shows, never silently dropped.
//!
//! The page may not be listening yet: the first launch's items exist before it has loaded, and a second
//! launch can arrive while it starts. So the shell keeps what arrives until the page asks for it with
//! [`take_opened`], and only after that emits [`OPENED_EVENT`] as items arrive. The page listens first and
//! asks second, so nothing falls between the two.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use fbtt_desktop_core::{Opened, opened_from_args};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

use crate::lifecycle;

/// The event the page gets for each item once it has asked for the ones that came first.
pub const OPENED_EVENT: &str = "opened";

/// The largest `.torrent` the shell reads and hands over: the same bound the Add page puts on a file it
/// uploads, since a file this size is an archive or a video, not metainfo.
pub const MAX_TORRENT_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// The most items kept while the page is not listening: sixteen launches' worth of the handoff's own
/// bound. A launcher that keeps firing past it loses the newest, said on stderr, rather than growing
/// the shell without bound.
const MAX_PENDING: usize = 256;

/// The scheme prefixes an argument may carry. An argument with one is a link, never a relative path.
const URL_PREFIXES: [&str; 2] = ["magnet:", "file:"];

/// One thing to open, as the page gets it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum OpenedItem {
    /// A `magnet:` link, as it was handed over.
    Magnet { uri: String },
    /// A `.torrent` file: its name and its bytes, base64.
    File { name: String, bytes_b64: String },
    /// A `.torrent` the shell would not hand over, and why, for the page to show.
    Refused { name: String, message: String },
}

/// Where the items go: the page, and the window it is in. The app's own is [`AppPage`]; the tests use a
/// fake that records.
pub trait Page {
    /// Hands one item to the listening page.
    fn emit(&self, item: &OpenedItem) -> Result<(), String>;
    /// Shows and focuses the window, which may be hidden in the tray.
    fn show(&self);
}

/// What the shell keeps: whether the page has asked yet, and what arrived before it did.
#[derive(Debug, Default)]
pub struct OpenedState {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    page_listening: bool,
    pending: VecDeque<OpenedItem>,
}

impl OpenedState {
    /// Nothing opened yet, and a page that has not asked.
    pub fn new() -> Self {
        OpenedState::default()
    }

    /// Hands `items` to the page if it is listening, or keeps them for [`OpenedState::take`]. An item the
    /// page could not be handed is kept too, and handed over again, ahead of the new ones, the next time
    /// something arrives or the page asks.
    pub fn deliver(&self, items: Vec<OpenedItem>, page: &impl Page) {
        if items.is_empty() {
            return;
        }
        let (listening, earlier) = {
            let mut inner = self.lock();
            let earlier: Vec<OpenedItem> = if inner.page_listening {
                inner.pending.drain(..).collect()
            } else {
                Vec::new()
            };
            (inner.page_listening, earlier)
        };
        let mut missed = Vec::new();
        if listening {
            for item in earlier.into_iter().chain(items) {
                if let Err(problem) = page.emit(&item) {
                    eprintln!("The page could not be handed what was opened: {problem}");
                    missed.push(item);
                }
            }
        } else {
            missed = items;
        }
        let mut inner = self.lock();
        for item in missed {
            if inner.pending.len() == MAX_PENDING {
                eprintln!(
                    "Too many links and files are waiting for the page; the newest are left out."
                );
                break;
            }
            inner.pending.push_back(item);
        }
    }

    /// The page started loading (a first load or a reload): it is not listening until it asks again, so
    /// what arrives meanwhile is kept for that [`OpenedState::take`] rather than emitted into nothing.
    pub fn page_gone(&self) {
        self.lock().page_listening = false;
    }

    /// Everything kept so far, oldest first, and from now on items go to the page as they arrive. Every
    /// page load asks once.
    pub fn take(&self) -> Vec<OpenedItem> {
        let mut inner = self.lock();
        inner.page_listening = true;
        inner.pending.drain(..).collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A `.torrent` file as the page gets it: its bytes when it is at most [`MAX_TORRENT_FILE_BYTES`], a
/// refusal saying why when it is larger or cannot be read. The size is checked before anything is read
/// and the read itself stops one byte past the bound, so a file that grows in between is refused too.
pub fn read_torrent(path: &Path) -> OpenedItem {
    let raw_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = fbt::display_safe(&raw_name, fbt::MAX_DISPLAY_CHARS);
    let too_large = || OpenedItem::Refused {
        name: name.clone(),
        message: format!("{name} is too large to be a torrent."),
    };
    let unreadable = || OpenedItem::Refused {
        name: name.clone(),
        message: format!("{name} could not be read."),
    };
    let Ok(file) = std::fs::File::open(path) else {
        return unreadable();
    };
    match file.metadata() {
        Ok(meta) if meta.len() > MAX_TORRENT_FILE_BYTES => return too_large(),
        Ok(_) => {}
        Err(_) => return unreadable(),
    }
    let mut bytes = Vec::new();
    if file
        .take(MAX_TORRENT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return unreadable();
    }
    if bytes.len() as u64 > MAX_TORRENT_FILE_BYTES {
        return too_large();
    }
    OpenedItem::File {
        name: raw_name,
        bytes_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

/// What the handoff found, as the page gets it.
pub fn items_from(opened: Vec<Opened>) -> Vec<OpenedItem> {
    opened
        .into_iter()
        .map(|item| match item {
            Opened::Magnet(uri) => OpenedItem::Magnet { uri },
            Opened::TorrentFile(path) => read_torrent(&path),
        })
        .collect()
}

/// A second launch's arguments as this copy reads them: a relative path is the second launch's, so it is
/// joined onto that launch's working directory. Flags and links are left as they are.
pub fn forwarded_args(argv: Vec<String>, cwd: &str) -> Vec<OsString> {
    let cwd = PathBuf::from(cwd);
    argv.into_iter()
        .enumerate()
        .map(|(index, arg)| {
            let link = URL_PREFIXES.iter().any(|prefix| {
                arg.trim()
                    .get(..prefix.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
            });
            if index == 0 || link || arg.starts_with('-') || Path::new(&arg).is_absolute() {
                OsString::from(arg)
            } else {
                cwd.join(arg).into_os_string()
            }
        })
        .collect()
}

/// A second launch: the window comes up, and what it was opened with goes to this copy's page. Nothing
/// is started here; the second copy exits once the plugin has forwarded its arguments.
pub fn on_second_launch(state: &OpenedState, argv: Vec<String>, cwd: &str, page: &impl Page) {
    page.show();
    let items = items_from(opened_from_args(&forwarded_args(argv, cwd)));
    state.deliver(items, page);
}

/// The URLs the system opened the running app with (macOS): magnets, and `file://` URLs for `.torrent`
/// files. Read by the same handoff as arguments, so the same bounds and the same checks apply.
pub fn on_open_urls(state: &OpenedState, urls: Vec<String>, page: &impl Page) {
    let args: Vec<OsString> = std::iter::once(OsString::new())
        .chain(urls.into_iter().map(OsString::from))
        .collect();
    let items = items_from(opened_from_args(&args));
    if items.is_empty() {
        return;
    }
    page.show();
    state.deliver(items, page);
}

/// The app's page: the main window, which [`lifecycle::show_window`] brings up.
pub struct AppPage<'a, R: Runtime>(pub &'a AppHandle<R>);

impl<R: Runtime> Page for AppPage<'_, R> {
    fn emit(&self, item: &OpenedItem) -> Result<(), String> {
        self.0
            .emit_to(lifecycle::MAIN_WINDOW, OPENED_EVENT, item)
            .map_err(|problem| problem.to_string())
    }

    fn show(&self) {
        lifecycle::show_window(self.0);
    }
}

/// The first launch's own arguments, kept for the page, and on macOS the open-URL events from now on.
pub fn install<R: Runtime>(app: &AppHandle<R>, state: &OpenedState) {
    let args: Vec<OsString> = std::env::args_os().collect();
    state.deliver(items_from(opened_from_args(&args)), &AppPage(app));
    #[cfg(target_os = "macos")]
    {
        use tauri::Manager;
        use tauri_plugin_deep_link::DeepLinkExt;
        let handle = app.clone();
        app.deep_link().on_open_url(move |event| {
            let urls = event.urls().into_iter().map(String::from).collect();
            on_open_urls(&handle.state::<OpenedState>(), urls, &AppPage(&handle));
        });
        // A cold start to open a file gets its open event before the app is ready, so before the
        // listener above existed; the plugin keeps the last one it saw. Both run on the main thread,
        // so nothing arrives between the listener and this read.
        if let Ok(Some(urls)) = app.deep_link().get_current() {
            let urls = urls.into_iter().map(String::from).collect();
            on_open_urls(state, urls, &AppPage(app));
        }
    }
}

/// What arrived before the page asked, oldest first. From this call on, items reach the page as
/// [`OPENED_EVENT`].
#[tauri::command]
pub fn take_opened(state: tauri::State<'_, OpenedState>) -> Vec<OpenedItem> {
    state.take()
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use base64::Engine;
    use fbtt_core::scratch::ScratchDir;

    use super::{
        MAX_TORRENT_FILE_BYTES, OpenedItem, OpenedState, Page, on_open_urls, on_second_launch,
        read_torrent,
    };

    /// A page that records what it was handed and how often the window was shown.
    #[derive(Default)]
    struct FakePage {
        emitted: RefCell<Vec<OpenedItem>>,
        shown: Cell<usize>,
    }

    impl Page for FakePage {
        fn emit(&self, item: &OpenedItem) -> Result<(), String> {
            self.emitted.borrow_mut().push(item.clone());
            Ok(())
        }

        fn show(&self) {
            self.shown.set(self.shown.get() + 1);
        }
    }

    /// A second launch's links and files go to the copy that runs, whose window comes up: kept while its
    /// page has not asked, emitted once it has. A relative path is the second launch's, read from its
    /// working directory. A launch with nothing to open still brings the window up.
    #[test]
    fn second_launch_args_are_forwarded_not_started() {
        let dir = ScratchDir::new("desktop-opened-second-launch");
        std::fs::write(
            dir.path().join("Sintel.torrent"),
            b"d4:infod4:name6:Sintelee",
        )
        .expect("write a torrent file");
        let cwd = dir.path().to_string_lossy().into_owned();
        let state = OpenedState::new();
        let page = FakePage::default();

        on_second_launch(
            &state,
            vec![
                "featherbittorrent-desktop".to_string(),
                "Sintel.torrent".to_string(),
                "magnet:?xt=urn:btih:aaa".to_string(),
                "--flag".to_string(),
            ],
            &cwd,
            &page,
        );
        assert_eq!(page.shown.get(), 1, "the running copy's window comes up");
        assert!(
            page.emitted.borrow().is_empty(),
            "a page that has not asked is not emitted to"
        );

        let file = OpenedItem::File {
            name: "Sintel.torrent".to_string(),
            bytes_b64: base64::engine::general_purpose::STANDARD
                .encode(b"d4:infod4:name6:Sintelee"),
        };
        let magnet = OpenedItem::Magnet {
            uri: "magnet:?xt=urn:btih:aaa".to_string(),
        };
        assert_eq!(state.take(), vec![file, magnet.clone()]);
        assert!(state.take().is_empty(), "taken once");

        on_second_launch(
            &state,
            vec![
                "featherbittorrent-desktop".to_string(),
                "magnet:?xt=urn:btih:aaa".to_string(),
            ],
            &cwd,
            &page,
        );
        assert_eq!(page.shown.get(), 2);
        assert_eq!(
            *page.emitted.borrow(),
            vec![magnet],
            "once the page has asked, items are emitted as they arrive"
        );
        assert!(state.take().is_empty(), "and not kept as well");

        on_second_launch(
            &state,
            vec!["featherbittorrent-desktop".to_string()],
            &cwd,
            &page,
        );
        assert_eq!(
            page.shown.get(),
            3,
            "a bare second launch still shows the window"
        );
        assert_eq!(page.emitted.borrow().len(), 1, "and opens nothing");

        // A reload: the page is not listening until it asks again, and what arrives meanwhile is kept
        // for that ask rather than emitted into nothing.
        state.page_gone();
        on_second_launch(
            &state,
            vec![
                "featherbittorrent-desktop".to_string(),
                "magnet:?xt=urn:btih:bbb".to_string(),
            ],
            &cwd,
            &page,
        );
        assert_eq!(
            page.emitted.borrow().len(),
            1,
            "not emitted to a page that is loading"
        );
        assert_eq!(
            state.take(),
            vec![OpenedItem::Magnet {
                uri: "magnet:?xt=urn:btih:bbb".to_string()
            }]
        );

        // An open-URL event with nothing this app opens shows nothing and hands over nothing.
        on_open_urls(&state, vec!["https://example.com/".to_string()], &page);
        assert_eq!(page.shown.get(), 4);
        assert_eq!(page.emitted.borrow().len(), 1);
    }

    /// An item the page could not be handed is kept, and handed over first the next time something
    /// arrives.
    #[test]
    fn a_missed_item_is_handed_over_again() {
        struct Flaky(Cell<bool>, RefCell<Vec<OpenedItem>>);
        impl Page for Flaky {
            fn emit(&self, item: &OpenedItem) -> Result<(), String> {
                if self.0.replace(false) {
                    return Err("the page is gone".to_string());
                }
                self.1.borrow_mut().push(item.clone());
                Ok(())
            }
            fn show(&self) {}
        }
        let magnet = |hash: &str| OpenedItem::Magnet {
            uri: format!("magnet:?xt=urn:btih:{hash}"),
        };
        let state = OpenedState::new();
        assert!(state.take().is_empty());
        let page = Flaky(Cell::new(true), RefCell::new(Vec::new()));
        state.deliver(vec![magnet("a")], &page);
        assert!(page.1.borrow().is_empty());
        state.deliver(vec![magnet("b")], &page);
        assert_eq!(*page.1.borrow(), vec![magnet("a"), magnet("b")]);
    }

    /// A `.torrent` over the bound goes to the page as a refusal naming it, not as bytes; one at the
    /// bound is handed over.
    #[test]
    fn oversized_torrent_is_refused() {
        let dir = ScratchDir::new("desktop-opened-oversized");
        let big = dir.path().join("Huge.torrent");
        std::fs::File::create(&big)
            .and_then(|file| file.set_len(MAX_TORRENT_FILE_BYTES + 1))
            .expect("make a file one byte over the bound");
        assert_eq!(
            read_torrent(&big),
            OpenedItem::Refused {
                name: "Huge.torrent".to_string(),
                message: "Huge.torrent is too large to be a torrent.".to_string(),
            }
        );

        let edge = dir.path().join("Edge.torrent");
        std::fs::File::create(&edge)
            .and_then(|file| file.set_len(MAX_TORRENT_FILE_BYTES))
            .expect("make a file at the bound");
        assert!(matches!(
            read_torrent(&edge),
            OpenedItem::File { ref name, .. } if name == "Edge.torrent"
        ));

        let gone = dir.path().join("Gone.torrent");
        assert!(matches!(
            read_torrent(&gone),
            OpenedItem::Refused { ref message, .. } if message == "Gone.torrent could not be read."
        ));

        let json = serde_json::to_value(read_torrent(&big)).expect("serializes");
        assert_eq!(json["kind"], "refused");
    }

    /// The base config registers `magnet:` and `.torrent` with the system; the platform files, which
    /// replace only the window list, carry neither.
    #[test]
    fn the_config_registers_magnet_links_and_torrent_files() {
        let base: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("parses");
        assert_eq!(
            base["plugins"]["deep-link"]["desktop"]["schemes"],
            serde_json::json!(["magnet"])
        );
        let association = &base["bundle"]["fileAssociations"][0];
        assert_eq!(association["ext"], serde_json::json!(["torrent"]));
        assert_eq!(association["mimeType"], "application/x-bittorrent");
        for platform in [
            include_str!("../tauri.windows.conf.json"),
            include_str!("../tauri.linux.conf.json"),
        ] {
            let conf: serde_json::Value = serde_json::from_str(platform).expect("parses");
            assert!(conf.get("plugins").is_none() && conf.get("bundle").is_none());
        }
    }
}
