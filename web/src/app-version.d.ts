/**
 * featherbittorrent's own version, as built. Vite injects it at build time from the
 * `[workspace.package]` version in the workspace root's `Cargo.toml` (see `vite.config.ts`), so
 * the header states the version of the app in front of the reader, not the engine library's.
 * Vitest inherits the same `define`, so the header test asserts against the constant itself.
 *
 * @module app-version
 */
export {}

declare global {
  const __APP_VERSION__: string
}
