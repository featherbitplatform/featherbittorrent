# Task 9: browser fixes for the web player

Four bugs a real browser (Chrome, Playwright, a real server) found that the unit tests could not see,
because the fakes were more lenient than a browser. Branch `feature/web-ui-m2-player-web`, started at
`3749f5b`. One commit per fix, in order.

## B1 (Critical): the engine calls fetch unbound

`engine.ts:211` set `fetch: window.fetch` and `engine.ts:454` called `full.fetch(url, ...)`. In a
browser that throws `TypeError: Illegal invocation`; the engine took it for a network error and sat in
`waiting` forever, so the player showed `Waiting for data...` and never requested a stream.

- `web/src/player/engine.ts:211` (commit `4d4be26`): the default deps now wrap the call,
  `fetch: (input, init) => window.fetch(input, init)`, keeping `window` as the `this`.
- Test `the default fetch is called on the window` (`web/src/player/Player.test.tsx`): replaces
  `window.fetch` with a function that throws `TypeError('Illegal invocation')` when its `this` is not
  the `window` and otherwise answers like the recorder (a 200 with the stream's start header and a
  fresh `FakeBody` per call). The engine is made through the default deps path (no `fetch` handed in);
  it asserts a stream request is made and the state is not left in `waiting`.

## B2 (Critical): subtitles never show

`Player.tsx:456` assigned `(found as TextTrack & { id?: string }).id = ...`. `TextTrack.id` is
read-only in a browser; in a module (strict mode) the assignment throws, so `addCues` stopped before
any cue was added and the track stayed `hidden` with no cues.

- `web/src/player/Player.tsx:293` (commit `a2a90ab`): a `Map<string, TextTrack>` (`subtitleTracksRef`,
  subtitle key to track) is kept in a ref. `addCues` (line 486) and `applySubtitleModes` (line 515)
  read and write through it, and `id` is never written.
- Fake: `web/src/player/Player.test.tsx` (the `addTextTrack` shim) now defines the fake `TextTrack`'s
  `id` as a getter with no setter, so assigning it throws in strict mode, as in a browser.
- Test `a chosen subtitle shows its cues`: chooses a subtitle, delivers two cues, and asserts the
  track is `showing` and holds the cues. The existing subtitle tests pass under the strict fake.

## B3 (Important): the player opens paused

Pressing Play in the Files tab opened the player but nothing called `play()`, so the viewer pressed
Play a second time. Ruling: opening the player starts playback.

- `web/src/player/Player.tsx` (commit `f701a35`): an `autoPlayedRef` (one ask per open). The remux
  effect (line 424) adds a `canplay` listener that calls `video.play()` once; the native WebM path
  (line 361) has the same effect, armed once the token's `src` is set. A rejected promise (autoplay
  policy) is caught and ignored, leaving the Play control showing.
- Test `opening the player starts playback`: asserts `play()` was called once after the first data,
  and not again on later data or a later `canplay`.

## B4 (Minor): the player does not fit a phone

At 390 px wide the dialog sat at the left edge, about 340 px wide, with the page showing on its right,
and the controls wrapped onto a second row. No unit test; the controller checks it in a browser.

- `web/src/player/Player.tsx` (commit `d1a219d`), Tailwind responsive classes only:
  - the `<dialog>` (line 754): `max-[640px]:h-dvh max-[640px]:w-full max-[640px]:rounded-none
    max-[640px]:border-0`, so at widths up to 640 px it fills the viewport (full width and height, no
    outer margin, no rounded outer border). Above 640 px the original look is kept.
  - the remux video (line 805): `h-full w-full object-contain` for a flexible video area.
  - the seek bar (line 831): `min-w-0` instead of `min-w-40`, so it shrinks instead of forcing a wrap.
  - the volume bar (line 867): `w-20 min-[420px]:w-24`, so the controls wrap tidily on a narrow screen.

## Prove-test results (each on a clean tree)

Command shape: `bash ~/.claude/skills/local-implementer/scripts/prove-test.sh <patch> npm --prefix web
test -- Player -t "<test>"`. Each patch restores the bug on the fixed tree; the test passes without it
and fails with it.

| patch | test | result | mutated-run failure |
| --- | --- | --- | --- |
| `mutations/task9-the-default-fetch-is-called-on-the-window.patch` | the default fetch is called on the window | PROVED | `AssertionError: the default path never asked for a stream (an unbound fetch throws instead)` |
| `mutations/task9-a-chosen-subtitle-shows-its-cues.patch` | a chosen subtitle shows its cues | PROVED | `AssertionError: a chosen subtitle did not show its track: expected false to be true` |
| `mutations/task9-opening-the-player-starts-playback.patch` | opening the player starts playback | PROVED | `AssertionError: the open did not start playback: expected +0 to be 1` |

Each prove-test's last line was `prove-test: PROVED the test fails with the bug (<patch>)`.

## Gate results (run once each, after all four fixes)

- lint (`npm --prefix web run lint`): clean, no errors.
- typecheck (`npm --prefix web run typecheck`): clean, no errors.
- test (`npm --prefix web test`): 17 test files, 152 tests, all passed.
- build (`npm --prefix web run build`): built in ~278 ms; `index-*.js` 304.52 kB (gzip 93.53 kB),
  `index-*.css` 29.81 kB (gzip 6.48 kB).
- size (`npm --prefix web run size`): 99.0 KB gzipped total of 300.0 KB budget, 201.0 KB left.

## Player tests run alone

All 42 Player tests were each run alone with `npm --prefix web test -- Player -t "<name>"` and all
passed (1 passed, 63 skipped per run). No failure.
