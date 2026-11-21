/**
 * The bundle budget: the built JavaScript and CSS, gzipped, at most 300 KB between them.
 *
 * Gzipped because that is what crosses the network, and why the budget is not a byte count of the
 * files on disk. Measured with the default deflate level, which is the level Vite's own build report
 * quotes, so this and `vite build` agree about what a file costs. KB here is 1000 bytes: the unit the
 * plan states the budget in, and the unit `vite build` divides its sizes by when it prints them
 * (it labels that unit `kB`).
 *
 * Only `dist/assets/*.js` and `dist/assets/*.css` are counted, which is the plan's rule: fonts and
 * images are payload the page asks for on its own terms and are not what makes a first paint slow to
 * start, and they are served with a year of `immutable` caching rather than on every load.
 *
 * Run it with `npm run size`, after `npm run build`: it reads what the build wrote rather than
 * rebuilding, so the number it prints is the number the binary will embed.
 *
 * @module scripts/check-bundle-size
 */
import { readdir, readFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { gzipSync } from 'node:zlib'

// The budget, in bytes: 300 KB of gzipped JavaScript and CSS together, KB being 1000 bytes. That is
// the unit the plan states the budget in, and the unit `vite build` divides a byte count by when it
// prints one (that report labels the unit `kB`), so the two numbers are about the same quantity.
const LIMIT = 300 * 1000

const here = dirname(fileURLToPath(import.meta.url))
const assetsDir = join(here, '..', 'dist', 'assets')

// A byte count spelled the way a person reads one.
function kb(bytes) {
  return `${(bytes / 1000).toFixed(1)} KB`
}

async function main() {
  let names
  try {
    names = await readdir(assetsDir)
  } catch {
    console.error(`no ${assetsDir}: run npm --prefix web run build before npm --prefix web run size`)
    process.exit(1)
  }

  // Sorted so the report reads the same on two machines: a directory's order is its own, and a check
  // that prints its rows in an arbitrary order is a check whose output cannot be compared by eye.
  const counted = names.filter((name) => name.endsWith('.js') || name.endsWith('.css')).sort()
  if (counted.length === 0) {
    console.error(`${assetsDir} holds no JavaScript or CSS: the build did not build`)
    process.exit(1)
  }

  let total = 0
  for (const name of counted) {
    const bytes = await readFile(join(assetsDir, name))
    const gzipped = gzipSync(bytes).length
    total += gzipped
    console.log(`  ${kb(gzipped).padStart(10)}  ${name}`)
  }

  const remaining = LIMIT - total
  console.log(`  ${kb(total).padStart(10)}  total gzipped, of ${kb(LIMIT)} budget`)
  if (total > LIMIT) {
    console.error(`the bundle is ${kb(total)} gzipped, ${kb(total - LIMIT)} over the budget`)
    process.exit(1)
  }
  console.log(`${kb(remaining)} left`)
}

main().catch((problem) => {
  console.error(`could not measure the bundle: ${problem.message ?? problem}`)
  process.exit(1)
})
