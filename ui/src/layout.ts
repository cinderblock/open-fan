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

/** A node's role, from its port signature alone. */
export type Role = 'source' | 'transform' | 'sink';

export function roleOf(ports: Pick<NodeDescriptor, 'inputs' | 'outputs'>): Role {
  if (ports.inputs.length === 0) return 'source';
  if (ports.outputs.length === 0) return 'sink';
  return 'transform';
}

/** Which column a node of this shape belongs in. */
export function columnOf(ports: Pick<NodeDescriptor, 'inputs' | 'outputs'>): number {
  switch (roleOf(ports)) {
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
  ports: Pick<NodeDescriptor, 'inputs' | 'outputs'>,
): { x: number; y: number } {
  const x = columnX(columnOf(ports));
  const inColumn = existing.filter(
    (node) => Math.abs(node.position.x - x) < COLUMN_WIDTH / 2,
  );

  if (inColumn.length === 0) return { x, y: ORIGIN.y };

  const lowest = Math.max(...inColumn.map((node) => node.position.y));
  return { x, y: lowest + ROW_HEIGHT };
}
