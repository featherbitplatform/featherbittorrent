/**
 * The web UI skeleton: the featherbit mark and the word, on the Graphite canvas.
 *
 * This is the whole page for Task 4, and that is the point: it is what proves the built front end is
 * embedded in the server, routed beside the API and served with the headers a page needs. The sign-in
 * screen arrives in Task 5 and the torrent table in Task 6, which is where this markup stops being
 * the page and becomes the header it describes.
 *
 * The mark is `/favicon.png`, the one copy the server has: `web/public/` is copied into the built
 * page as it is, so this file and the tab icon are the same bytes and the same request. Its `alt` is
 * empty because the word beside it is the name, and a screen reader would otherwise hear it twice.
 *
 * @module App
 */
export default function App() {
  return (
    <main className="flex min-h-dvh flex-col items-center justify-center gap-4">
      <img className="h-16 w-auto" src="/favicon.png" alt="" />
      <h1 className="text-base font-medium tracking-wide text-strong">featherbittorrent</h1>
    </main>
  )
}
