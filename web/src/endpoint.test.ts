/**
 * The instance base address: what the shell or a person typed, made into the one spelling every URL is
 * built on.
 *
 * @module endpoint.test
 */
import { expect, it } from 'vitest'
import { normaliseBase } from './endpoint'

it('normalise_base_drops_trailing_slashes_and_space', () => {
  expect(normaliseBase('http://127.0.0.1:53211/')).toBe('http://127.0.0.1:53211')
  expect(normaliseBase(' https://seedbox.example/fbtt// ')).toBe('https://seedbox.example/fbtt')
  expect(normaliseBase('http://127.0.0.1:53211')).toBe('http://127.0.0.1:53211')
})
