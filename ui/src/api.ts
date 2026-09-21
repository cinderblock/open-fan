/**
 * Typed wrappers over the backend commands.
 *
 * Every type here is generated from Rust (`bun run bindings`), so the editor cannot
 * quietly disagree with the backend about the protocol.
 *
 * The editor **polls** rather than subscribing. That is deliberate: a subscription makes
 * the control loop responsible for pushing to a consumer that may be slow, paused by the
 * OS, or gone. Polling keeps the dependency pointing the right way — the UI asks for what
 * the engine has already published, and a stalled window costs the engine nothing.
 */
import { invoke } from '@tauri-apps/api/core';

import type { Graph } from './bindings/Graph';
import type { HardwareInventory } from './bindings/HardwareInventory';
import type { NodeDescriptor } from './bindings/NodeDescriptor';
import type { SnapshotDto } from './bindings/SnapshotDto';
import type { ValidationError } from './bindings/ValidationError';

export type { Graph, HardwareInventory, NodeDescriptor, SnapshotDto, ValidationError };
export type { NodeInstance } from './bindings/NodeInstance';
export type { NodeKind } from './bindings/NodeKind';
export type { PortDto } from './bindings/PortDto';
export type { WireValue } from './bindings/WireValue';

/** True when running inside the Tauri shell rather than a bare browser. */
export const IN_APP = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

export const nodeCatalogue = () => invoke<NodeDescriptor[]>('node_catalogue');
export const inventory = () => invoke<HardwareInventory>('inventory');
export const getGraph = () => invoke<Graph>('get_graph');
export const snapshot = () => invoke<SnapshotDto>('snapshot');
export const rescan = () => invoke<void>('rescan');

/** Outcome of an attempted graph install. */
export type ApplyResult = { ok: true } | { ok: false; errors: ValidationError[] };

/**
 * Install a graph.
 *
 * Validation failures come back as a list rather than an exception, because they are an
 * expected outcome of editing — not an error condition. A rejected graph leaves the
 * running configuration untouched.
 */
export async function setGraph(graph: Graph): Promise<ApplyResult> {
  try {
    await invoke<void>('set_graph', { graph });
    return { ok: true };
  } catch (raw) {
    if (Array.isArray(raw)) return { ok: false, errors: raw as ValidationError[] };
    // Anything else is a genuine transport or backend failure; surface it as one entry
    // rather than swallowing it into a silent no-op.
    return { ok: false, errors: [{ message: String(raw), nodeId: null }] };
  }
}
