/**
 * The order instances are listed in, in the switcher and in Instances alike.
 *
 * @module desktop/instanceOrder
 */
import { LOCAL_ID } from './bridge'
import type { InstanceItem } from './bridge'

/** This computer first, the rest in the order they were saved. */
export function inSwitcherOrder(instances: InstanceItem[]): InstanceItem[] {
  return [
    ...instances.filter((item) => item.id === LOCAL_ID),
    ...instances.filter((item) => item.id !== LOCAL_ID),
  ]
}
