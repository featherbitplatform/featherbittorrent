// @vitest-environment jsdom
/**
 * The switcher lists This computer first whatever order the list came in, marks the connected
 * instance, and says what was picked.
 *
 * @module desktop/InstanceSwitcher.test
 */
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'

vi.mock('./bridge', () => ({ LOCAL_ID: 'local' }))

import { InstanceSwitcher } from './InstanceSwitcher'

afterEach(cleanup)

const ITEMS = [
  { id: 'nas', name: 'NAS', url: 'https://nas.example', saved_password: true },
  { id: 'local', name: 'This computer', url: '', saved_password: false },
  { id: 'pi', name: 'Pi at home', url: 'http://pi.local:8090', saved_password: false },
]

it('switcher_lists_this_computer_first', () => {
  const onSwitch = vi.fn()
  const onManage = vi.fn()
  render(<InstanceSwitcher instances={ITEMS} current="pi" onSwitch={onSwitch} onManage={onManage} />)
  expect(screen.queryByRole('menu')).toBeNull()
  fireEvent.click(screen.getByRole('button', { name: 'Instance: Pi at home' }))
  const rows = screen.getAllByRole('menuitemradio')
  expect(rows.map((row) => row.textContent)).toEqual([
    'This computer',
    'NAShttps://nas.example',
    'Pi at homehttp://pi.local:8090',
  ])
  expect(rows.map((row) => row.getAttribute('aria-checked'))).toEqual(['false', 'false', 'true'])

  fireEvent.click(rows[1]!)
  expect(onSwitch).toHaveBeenCalledWith('nas')
  expect(screen.queryByRole('menu'), 'the menu stayed open after a pick').toBeNull()

  fireEvent.click(screen.getByRole('button', { name: 'Instance: Pi at home' }))
  fireEvent.click(screen.getByRole('menuitem', { name: 'Manage instances' }))
  expect(onManage).toHaveBeenCalledTimes(1)
})

it('escape_closes_the_menu', () => {
  render(<InstanceSwitcher instances={ITEMS} current="local" onSwitch={vi.fn()} onManage={vi.fn()} />)
  fireEvent.click(screen.getByRole('button', { name: 'Instance: This computer' }))
  expect(screen.getByRole('menu')).toBeTruthy()
  fireEvent.keyDown(document, { key: 'Escape' })
  expect(screen.queryByRole('menu')).toBeNull()
})
