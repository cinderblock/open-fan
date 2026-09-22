/**
 * Presentation metadata for the port type system.
 *
 * The `Quantity` union itself is generated from the Rust definition in `of-units`
 * (`bun run bindings`), so the editor cannot invent a port type the backend has never
 * heard of. This module only adds what the backend has no opinion about: colour and
 * display text.
 *
 * The connection rule below deliberately mirrors `Quantity::connects_to` in Rust. The
 * backend remains authoritative — it re-validates every graph on load — but the editor
 * check is what stops an illegal edge from ever being drawn.
 */
import type { Quantity } from './bindings/Quantity';

export type { Quantity };

export interface QuantityStyle {
  /** Display name, matching `Quantity::label`. */
  label: string;
  /** Unit symbol, matching `Quantity::symbol`. Empty for dimensionless quantities. */
  symbol: string;
  /** Port and edge colour. */
  color: string;
}

/**
 * Colours are chosen to stay distinguishable for the most common forms of colour vision
 * deficiency, and shape/label carry the type too — colour is never the only cue.
 */
export const QUANTITY_STYLE: Record<Quantity, QuantityStyle> = {
  temperature: { label: 'Temperature', symbol: '°C', color: '#e8663d' },
  duty: { label: 'Duty', symbol: '%', color: '#3d9ae8' },
  rpm: { label: 'Speed', symbol: 'RPM', color: '#7a6ff0' },
  load: { label: 'Load', symbol: '%', color: '#d9a03c' },
  power: { label: 'Power', symbol: 'W', color: '#c94f9c' },
  voltage: { label: 'Voltage', symbol: 'V', color: '#4bbf9a' },
  current: { label: 'Current', symbol: 'A', color: '#2f8f7a' },
  frequency: { label: 'Frequency', symbol: 'Hz', color: '#8a8f98' },
  throughput: { label: 'Throughput', symbol: 'B/s', color: '#5f7d95' },
  ratio: { label: 'Ratio', symbol: '', color: '#9aa4ad' },
  boolean: { label: 'Boolean', symbol: '', color: '#6f7f8f' },
  time: { label: 'Time', symbol: 's', color: '#b07f4f' },
};

export const ALL_QUANTITIES = Object.keys(QUANTITY_STYLE) as Quantity[];

/**
 * How an undecided port is drawn.
 *
 * Generic nodes carry whatever they are given, so until a connection decides them there
 * is no honest colour to use. White reads as "anything", and the port locks to a real
 * colour the moment inference resolves it.
 */
export const GENERIC_STYLE: QuantityStyle = {
  label: 'Any type',
  symbol: '',
  color: '#e6e9ee',
};

/** The style for a port type, falling back to neutral when nothing has decided it. */
export function styleOf(q: Quantity | null | undefined): QuantityStyle {
  return q ? QUANTITY_STYLE[q] : GENERIC_STYLE;
}

/**
 * Whether a value of `source` may flow into a port expecting `sink`.
 *
 * Exact equality, with no implicit coercion — including between quantities that share a
 * unit. `load` and `duty` are both percentages and still do not interchange, because
 * "the CPU is 70 % busy" and "drive this fan at 70 %" are different claims.
 *
 * `null` means the port is generic and nothing has decided it yet, so it accepts
 * anything: connecting it is precisely what decides it. The backend re-runs full
 * inference on apply and is the authority — this check exists so an obviously illegal
 * edge cannot be drawn in the first place.
 */
export function connects(
  source: Quantity | null | undefined,
  sink: Quantity | null | undefined,
): boolean {
  if (!source || !sink) return true;
  return source === sink;
}

/** The message to show when a connection is refused. */
export function rejectionReason(source: Quantity, sink: Quantity): string {
  const a = styleOf(source);
  const b = styleOf(sink);
  return `${a.label} cannot drive ${b.label} — add a conversion node in between.`;
}

/** Format a value for a readout, e.g. `54.3 °C`. */
export function formatValue(q: Quantity | null | undefined, scalar: number): string {
  if (!Number.isFinite(scalar)) return '—';
  const { symbol } = styleOf(q);
  const rounded = Math.abs(scalar) >= 100 ? scalar.toFixed(0) : scalar.toFixed(1);
  return symbol ? `${rounded} ${symbol}` : rounded;
}
