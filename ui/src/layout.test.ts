import { describe, expect, test } from 'bun:test';

import {
  COLUMNS_BETWEEN,
  COLUMN_WIDTH,
  ORIGIN,
  ROW_HEIGHT,
  SINK_COLUMN,
  columnOf,
  columnX,
  placeNode,
  roleOf,
  type Placed,
} from './layout';

/** Minimal stand-ins for the port lists placement actually reads. */
const port = { key: 'p', label: 'p', quantity: 'duty', required: true, variadic: false } as const;
const SOURCE = { inputs: [], outputs: [port] };
const TRANSFORM = { inputs: [port], outputs: [port] };
const SINK = { inputs: [port], outputs: [] };

const at = (x: number, y: number): Placed => ({ position: { x, y } });

describe('roles', () => {
  test('are derived from the port signature alone', () => {
    expect(roleOf(SOURCE)).toBe('source');
    expect(roleOf(TRANSFORM)).toBe('transform');
    expect(roleOf(SINK)).toBe('sink');
  });
});

describe('columns', () => {
  test('sources are left, sinks are right', () => {
    expect(columnOf(SOURCE)).toBe(0);
    expect(columnOf(SINK)).toBe(SINK_COLUMN);
    expect(columnOf(SOURCE)).toBeLessThan(columnOf(SINK));
  });

  test('two columns are left free between sources and sinks', () => {
    expect(columnOf(SINK) - columnOf(SOURCE) - 1).toBe(COLUMNS_BETWEEN);
  });

  test('transforms start in the first free column, not on top of the sources', () => {
    expect(columnOf(TRANSFORM)).toBe(1);
    expect(columnX(columnOf(TRANSFORM))).toBeGreaterThan(columnX(columnOf(SOURCE)));
    expect(columnX(columnOf(TRANSFORM))).toBeLessThan(columnX(columnOf(SINK)));
  });
});

describe('placement', () => {
  test('the first node of each kind starts at the top of its column', () => {
    expect(placeNode([], SOURCE)).toEqual({ x: columnX(0), y: ORIGIN.y });
    expect(placeNode([], SINK)).toEqual({ x: columnX(SINK_COLUMN), y: ORIGIN.y });
  });

  test('sources stack vertically, sharing one x', () => {
    const first = placeNode([], SOURCE);
    const second = placeNode([at(first.x, first.y)], SOURCE);
    const third = placeNode([at(first.x, first.y), at(second.x, second.y)], SOURCE);

    expect(second.x).toBe(first.x);
    expect(third.x).toBe(first.x);
    expect(second.y).toBe(first.y + ROW_HEIGHT);
    expect(third.y).toBe(second.y + ROW_HEIGHT);
  });

  test('sinks stack in their own column, unaffected by the sources', () => {
    const sources = [at(columnX(0), ORIGIN.y), at(columnX(0), ORIGIN.y + ROW_HEIGHT)];
    const sink = placeNode(sources, SINK);

    expect(sink.x).toBe(columnX(SINK_COLUMN));
    // Three sources in the left column must not push the first sink down.
    expect(sink.y).toBe(ORIGIN.y);
  });

  test('a new node never lands on top of an existing one in its column', () => {
    const existing = [at(columnX(0), ORIGIN.y), at(columnX(0), ORIGIN.y + ROW_HEIGHT * 3)];
    const next = placeNode(existing, SOURCE);

    // Below the lowest, not merely below the first.
    expect(next.y).toBe(ORIGIN.y + ROW_HEIGHT * 4);
    for (const node of existing) {
      expect(next.y).not.toBe(node.position.y);
    }
  });

  test('a node the user dragged into a column counts as occupying it', () => {
    // Placement matches by proximity rather than by role, so hand-arranged layouts are
    // respected instead of being stacked through.
    const dragged = at(columnX(0) + 30, ORIGIN.y + 400);
    expect(placeNode([dragged], SOURCE).y).toBe(dragged.position.y + ROW_HEIGHT);
  });

  test('a node well clear of a column does not affect it', () => {
    const faraway = at(columnX(0) + COLUMN_WIDTH, ORIGIN.y + 999);
    expect(placeNode([faraway], SOURCE)).toEqual({ x: columnX(0), y: ORIGIN.y });
  });

  test('a whole sensor-to-fan chain lays out left to right', () => {
    const placements: Placed[] = [];
    const add = (ports: Parameters<typeof placeNode>[1]) => {
      const p = placeNode(placements, ports);
      placements.push(at(p.x, p.y));
      return p;
    };

    const sensor = add(SOURCE);
    const curve = add(TRANSFORM);
    const fan = add(SINK);

    expect(sensor.x).toBeLessThan(curve.x);
    expect(curve.x).toBeLessThan(fan.x);
    // Nothing overlaps, and the chain reads along one row.
    expect(new Set([sensor.y, curve.y, fan.y]).size).toBe(1);
  });
});
