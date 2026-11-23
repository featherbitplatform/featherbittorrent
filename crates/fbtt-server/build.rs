//! Change tracking for the front end `rust-embed` folds into this crate's binary.
//!
//! `rust-embed`'s derive reads `web/dist` when this crate is compiled, so the files it wants are not
//! in this crate and cargo would not think to look at them: this is the line that tells it to. It is
//! conditional because the headless build has no static routes and no embed at all, and asking it to
//! rebuild on a folder it never reads would make `web/dist` part of a build that does not use it.
//!
//! `OUT_DIR` is watched unconditionally. It changes whenever the feature set or the profile does, and
//! this is the only place that decides whether the folder line is in effect, so re-running on it is
//! what keeps the two builds' answers separate - a build that gains `web-ui` re-runs this script and
//! picks the line up, and one that drops it stops rebuilding on `web/dist`.

fn main() {
    println!("cargo:rerun-if-env-changed=OUT_DIR");

    // `CARGO_FEATURE_<NAME>` is how a build script sees its own features: cargo sets one per enabled
    // feature, uppercased and with `-` spelled as `_`.
    if std::env::var_os("CARGO_FEATURE_WEB_UI").is_none() {
        return;
    }

    println!("cargo:rerun-if-changed=../../web/dist");

    // Say it here rather than letting the derive say it. The derive also fails on a missing folder,
    // but what it says is that a folder does not exist, which is not what is wrong: the front end has
    // not been built. `index.html` is what is looked for rather than the folder, because a folder that
    // is there and empty - `web/dist` left over from a build that was interrupted, or a checkout that
    // made the directory and nothing in it - is the same problem wearing a different face, and it is
    // the file the page route asks for by name.
    let index = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/dist/index.html");
    if !std::path::Path::new(&index).is_file() {
        panic!(
            "the web-ui feature embeds web/dist, which is missing: run npm --prefix web ci && npm --prefix web run build"
        );
    }
}
