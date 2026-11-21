import js from '@eslint/js'
import globals from 'globals'
import reactHooks from 'eslint-plugin-react-hooks'
import reactRefresh from 'eslint-plugin-react-refresh'
import tseslint from 'typescript-eslint'
import { defineConfig, globalIgnores } from 'eslint/config'

/**
 * The lint rules for the front end, in the gateway's shape.
 *
 * `dist` is ignored because it is what the build writes, and `src/api/schema.ts` because
 * `npm run gen:api` writes it from `src/api/openapi.json`: a rule that fires there is fixed by
 * changing the generator or the API document, not by editing a file that is rewritten on the next
 * `gen:api`. The CI job that lints also proves that file is current, which is the check that
 * generated code actually needs.
 */
export default defineConfig([
  globalIgnores(['dist', 'src/api/schema.ts']),
  {
    files: ['**/*.{ts,tsx}'],
    extends: [
      js.configs.recommended,
      tseslint.configs.recommended,
      reactHooks.configs.flat.recommended,
      reactRefresh.configs.vite,
    ],
    languageOptions: {
      ecmaVersion: 2023,
      globals: globals.browser,
    },
  },
  {
    // This file and the build scripts are node code rather than part of the page: different globals,
    // no React, and `import` of packages that do not exist in a browser.
    files: ['**/*.{js,mjs}'],
    extends: [js.configs.recommended],
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: 'module',
      globals: globals.node,
    },
  },
])
