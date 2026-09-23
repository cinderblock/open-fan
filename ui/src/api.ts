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
import type { PortTypeDto } from './bindings/PortTypeDto';
import type { SnapshotDto } from './bindings/SnapshotDto';
import type { ContentionReport } from './bindings/ContentionReport';
import type { TakeoverResult } from './bindings/TakeoverResult';
import type { ValidationError } from './bindings/ValidationError';

export type { Graph, HardwareInventory, NodeDescriptor, SnapshotDto, ValidationError };
export type { NodeInstance } from './bindings/NodeInstance';
export type { NodeKind } from './bindings/NodeKind';
export type { PortDto } from './bindings/PortDto';
export type { PortTypeDto } from './bindings/PortTypeDto';
export type { WireValue } from './bindings/WireValue';

/** True when running inside the Tauri shell rather than a bare browser. */
export const IN_APP = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

export const nodeCatalogue = () => invoke<NodeDescriptor[]>('node_catalogue');
export const inventory = () => invoke<HardwareInventory>('inventory');
export const getGraph = () => invoke<Graph>('get_graph');
export const snapshot = () => invoke<SnapshotDto>('snapshot');
export const rescan = () => invoke<void>('rescan');

/**
 * Infer the type of every port in a candidate graph.
 *
 * Called on each structural edit so generic ports can lock to a colour as soon as a
 * connection decides them. Unification lives in the backend; the editor only renders
 * the answer.
 */
export const resolveTypes = (graph: Graph) => invoke<PortTypeDto[]>('resolve_types', { graph });

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


export type { ContentionReport, TakeoverResult };
export type { ChannelControlDto } from './bindings/ChannelControlDto';
export type { ChannelControlEntry } from './bindings/ChannelControlEntry';
export type { ContendingAppDto } from './bindings/ContendingAppDto';

/** What currently stands between OpenFan and control of this machine's fans. */
export const contentionReport = () => invoke<ContentionReport>('contention_report');

/**
 * Stand rival controllers down and return abandoned channels to the board firmware.
 *
 * `force` permits terminating an application that will not exit politely. Left off by
 * default and worth keeping off: a terminated program runs no shutdown code, so it
 * restores nothing it was controlling.
 */
export const takeOver = (force: boolean) => invoke<TakeoverResult>('take_over', { force });

/** What the editor knows about the background service. */
export type ServiceStatus = {
  running: boolean;
  version: string | null;
  compatible: boolean;
  summary: string;
};

/**
 * Is the service there, and does it speak our protocol?
 *
 * Hand-written rather than generated, because it describes the *connection* rather than
 * anything the backend models — it has to be answerable when there is no backend to ask.
 */
export const serviceStatus = () => invoke<ServiceStatus>('service_status');

/**
 * What the service knows about updates.
 *
 * Hand-written rather than generated: the service passes this through as opaque JSON
 * because it owns the meaning, and the window only renders it.
 */
export type UpdateStatus = {
  currentVersion: string;
  available: string | null;
  notes: string | null;
  rejected: string | null;
  /** The service installs updates itself, with no prompt. */
  automatic: boolean;
  /** False when this build has no signing key, in which case nothing can be installed. */
  verifiable: boolean;
  error: string | null;
};

export const updateStatus = () => invoke<UpdateStatus>('update_status');
export const checkForUpdate = () => invoke<UpdateStatus>('check_for_update');
export const setAutoUpdate = (enabled: boolean) =>
  invoke<UpdateStatus>('set_auto_update', { enabled });

/** Install as the service: no prompt. Only offered once the user has opted in. */
export const applyUpdateSilently = () => invoke<void>('apply_update_silently');

/** Install as the user: one administrator prompt. The service still verifies the file. */
export const applyUpdatePrompted = () => invoke<string>('apply_update_prompted');

/**
 * Fetch the PawnIO hardware module.
 *
 * PawnIO installs a driver and no modules, so a machine can have it working and OpenFan
 * still see nothing at all. The service downloads it — it is the one with somewhere
 * machine-wide to put the file, and the one that will use it.
 */
export const fetchHardwareModule = () => invoke<string>('fetch_hardware_module');

/**
 * Whether the OpenFan *window* opens at sign-in.
 *
 * Only the window. The service starts at boot regardless, which is what keeps fans
 * managed before anyone logs in — this is about whether the tray icon is there.
 */
export const autostartEnabled = () => invoke<boolean>('autostart_enabled');
export const setAutostart = (enabled: boolean) => invoke<boolean>('set_autostart', { enabled });
