// Copies the repository's CHANGELOG.md to a generated page, src/pages/changelog.md, so the docs
// site has a Changelog page. Runs before `npm start` and `npm run build` (see package.json); the
// copy is git-ignored. CHANGELOG.md stays the only place to edit. The page lives outside docs/
// because docs/ is read in place and also rendered on GitHub.
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';

const website = dirname(dirname(fileURLToPath(import.meta.url)));
const repo = 'https://github.com/featherbitplatform/featherbittorrent';
let text = readFileSync(join(website, '..', 'CHANGELOG.md'), 'utf8');

// Links that point into the repository (docs/x.md, crates/..., ../...) would break on the site;
// make them absolute GitHub links. Absolute links, anchors and mailto: stay as they are.
text = text.replace(/\]\((?!https?:|#|mailto:)([^)\s]+)\)/g, (_, target) => {
  const path = target.replace(/^(\.\.?\/)+/, '');
  return `](${repo}/blob/develop/${path})`;
});

mkdirSync(join(website, 'src', 'pages'), {recursive: true});
writeFileSync(join(website, 'src', 'pages', 'changelog.md'), text);
