/**
 * Default placement for newly added nodes.
 *
 * A fan graph reads left to right: what the machine is doing, what to do about it, and
 * which header to drive. Placement follows that, so a graph built purely by clicking the
 * palette comes out readable instead of needing to be untangled first.
 *
 * Sources land in one column and sinks in another, each aligned vertically, with room for
 * two columns of transforms between them — enough for the common
 * `sensor → mix → curve → fan` shape without anything having to be moved.
 *
 * ```text
 *   col 0        col 1         col 2         col 3
 *   sources      transforms    transforms    sinks
 *   ┌──────┐     ┌──────┐                    ┌──────┐
 *   │ CPU  │────▶│ mix  │───────────────────▶│ fan  │
 *   └──────┘     └──────┘                    └──────┘
 *   ┌──────┐        ▲
 *   │ GPU  │────────┘
 *   └──────┘
 * ```
 *
 * These are *defaults*. Anything the user drags stays where they put it; placement only
 * decides where a node first appears.
 */
import type { NodeDescriptor } from './api';

/** Horizontal pitch between columns. Comfortably wider than the widest node. */
export const COLUMN_WIDTH = 280;

/** Vertical pitch between nodes in a column. Tall enough for a node with several ports. */
export const ROW_HEIGHT = 150;

/** Columns left free between sources and sinks. */
export const COLUMNS_BETWEEN = 2;

/** Where the first node in the first column goes. */
export const ORIGIN = { x: 60, y: 60 } as const;

/** The column a sink occupies: sources, then the gap, then sinks. */
export const SINK_COLUMN = COLUMNS_BETWEEN + 1;

export interface Placed {
  position: { x: number; y: number };
}

/** A node's role for layout purposes. */
export type Role = 'source' | 'transform' | 'sink';

/**
 * Which end of the graph a node belongs to.
 *
 * Taken from the catalogue's own category rather than inferred from port shape. A fan
 * output has a speed output as well as a duty input — it is still the end of the chain,
 * and counting ports would put it in the middle.
 */
export function roleOf(node: Pick<NodeDescriptor, 'category'>): Role {
  switch (node.category) {
    case 'source':
      return 'source';
    case 'sink':
      return 'sink';
    default:
      return 'transform';
  }
}

/** Which column a node of this kind belongs in. */
export function columnOf(node: Pick<NodeDescriptor, 'category'>): number {
  switch (roleOf(node)) {
    case 'source':
      return 0;
    case 'sink':
      return SINK_COLUMN;
    // Transforms start in the first free column; the second is left for whatever the
    // user wires in after them.
    case 'transform':
      return 1;
  }
}

export function columnX(column: number): number {
  return ORIGIN.x + column * COLUMN_WIDTH;
}

/**
 * Pick a position for a new node.
 *
 * Nodes are matched to a column by proximity rather than by role, so a node the user has
 * dragged into a column counts as occupying it. Stacking continues below whatever is
 * lowest there, which keeps a column aligned without ever placing a node on top of an
 * existing one.
 */
export function placeNode(
  existing: readonly Placed[],
  node: Pick<NodeDescriptor, 'category'>,
): { x: number; y: number } {
  const x = columnX(columnOf(node));
  const inColumn = existing.filter(
    (node) => Math.abs(node.position.x - x) < COLUMN_WIDTH / 2,
  );

  if (inColumn.length === 0) return { x, y: ORIGIN.y };

  const lowest = Math.max(...inColumn.map((node) => node.position.y));
  return { x, y: lowest + ROW_HEIGHT };
}
