/**
 * The side panel: engine status, the node palette, and the apply controls.
 *
 * Status is deliberately prominent. "Is anything actually controlling my fans right now"
 * is the question this application exists to answer, and it should never require clicking
 * into anything to find out.
 */
import { useMemo } from 'react';

import type {
  HardwareInventory,
  NodeDescriptor,
  NodeInstance,
  SnapshotDto,
  ValidationError,
} from './api';
import NodeInspector, { type DeviceHint } from './NodeInspector';
import TakeoverPanel from './TakeoverPanel';
import UpdatePanel from './UpdatePanel';
import { kindTag } from './graph';
import type { NodeCategory } from './bindings/NodeCategory';
import type { Quantity } from './bindings/Quantity';
import { ALL_QUANTITIES, GENERIC_STYLE, styleOf } from './quantities';

const CATEGORY_ORDER: NodeCategory[] = ['source', 'transform', 'stateful', 'logic', 'sink'];

const CATEGORY_LABEL: Record<NodeCategory, string> = {
  source: 'Sources',
  transform: 'Transforms',
  stateful: 'Filters & controllers',
  logic: 'Logic',
  sink: 'Outputs',
};

interface Props {
  catalogue: NodeDescriptor[];
  hardware: HardwareInventory | null;
  snapshot: SnapshotDto | null;
  errors: ValidationError[];
  dirty: boolean;
  selectedId: string | null;
  selectedNode: NodeInstance | null;
  selectedType: Quantity | null;
  selectedDevice: DeviceHint | null;
  onAdd: (descriptor: NodeDescriptor) => void;
  onApply: () => void;
  onRevert: () => void;
  onDelete: () => void;
  onChangeNode: (id: string, next: NodeInstance) => void;
  onAddSensor: (sensorId: string) => void;
}

function EngineStatus({
  snapshot,
  hardware,
}: {
  snapshot: SnapshotDto | null;
  hardware: HardwareInventory | null;
}) {
  if (!snapshot) {
    return <p className="status status--unknown">Waiting for the engine…</p>;
  }

  const commanded = Object.entries(snapshot.commanded);
  const state = snapshot.degraded ? 'degraded' : commanded.length > 0 ? 'ok' : 'idle';

  return (
    <div className={`status status--${state}`}>
      <div className="status__headline">
        {state === 'degraded' && 'Failsafe — channels handed back'}
        {state === 'ok' && `Controlling ${commanded.length} channel${commanded.length === 1 ? '' : 's'}`}
        {state === 'idle' && 'Running, nothing to control'}
      </div>

      {snapshot.sensorError && <div className="status__detail">{snapshot.sensorError}</div>}

      {commanded.length > 0 && (
        <ul className="status__channels">
          {commanded.map(([channel, duty]) => (
            <li key={channel}>
              <span className="status__channel">{channel}</span>
              <span className="status__duty">{duty.toFixed(0)} %</span>
            </li>
          ))}
        </ul>
      )}

      {snapshot.failsafed.length > 0 && (
        <div className="status__detail">
          Failsafed: {snapshot.failsafed.join(', ')}
        </div>
      )}

      <div className="status__meta">
        {hardware && <span>{hardware.backend}</span>}
        <span>{(1 / Math.max(snapshot.dt, 1e-6)).toFixed(0)} Hz</span>
        <span>{snapshot.tickDurationMs.toFixed(1)} ms/tick</span>
        {snapshot.overruns > 0 && (
          <span className="status__warn">{String(snapshot.overruns)} overruns</span>
        )}
      </div>
    </div>
  );
}

export default function Sidebar({
  catalogue,
  hardware,
  snapshot,
  errors,
  dirty,
  selectedId,
  selectedNode,
  selectedType,
  selectedDevice,
  onAdd,
  onApply,
  onRevert,
  onDelete,
  onChangeNode,
  onAddSensor,
}: Props) {
  const grouped = useMemo(() => {
    const byCategory = new Map<NodeCategory, NodeDescriptor[]>();
    for (const descriptor of catalogue) {
      byCategory.set(descriptor.category, [
        ...(byCategory.get(descriptor.category) ?? []),
        descriptor,
      ]);
    }
    return CATEGORY_ORDER.map((c) => [c, byCategory.get(c) ?? []] as const).filter(
      ([, items]) => items.length > 0,
    );
  }, [catalogue]);

  return (
    <aside className="sidebar">
      <header className="sidebar__header">
        <h1 className="sidebar__title">OpenFan</h1>
        {hardware && !hardware.driverPresent && (
          <p className="sidebar__driver">{hardware.driverSummary}</p>
        )}
      </header>

      <EngineStatus snapshot={snapshot} hardware={hardware} />

      <TakeoverPanel />

      <UpdatePanel />

      <div className="sidebar__actions">
        <button
          type="button"
          className="button button--primary"
          onClick={onApply}
          disabled={!dirty}
        >
          {dirty ? 'Apply changes' : 'No changes'}
        </button>
        <button type="button" className="button" onClick={onRevert} disabled={!dirty}>
          Revert
        </button>
        <button type="button" className="button" onClick={onDelete} disabled={!selectedId}>
          Delete node
        </button>
      </div>

      {errors.length > 0 && (
        <div className="errors">
          <h2 className="errors__title">
            Not applied — {errors.length} problem{errors.length === 1 ? '' : 's'}
          </h2>
          <ul className="errors__list">
            {errors.map((error, i) => (
              <li key={`${error.nodeId ?? ''}-${i}`}>{error.message}</li>
            ))}
          </ul>
        </div>
      )}

      {selectedId && selectedNode && (
        <NodeInspector
          nodeId={selectedId}
          node={selectedNode}
          descriptor={catalogue.find((d) => d.kind === kindTag(selectedNode))}
          inventory={hardware}
          nodeType={selectedType}
          device={selectedDevice}
          onChange={(next) => onChangeNode(selectedId, next)}
          onAddSensor={onAddSensor}
        />
      )}

      <section className="palette">
        <h2 className="sidebar__heading">Add a node</h2>
        {grouped.map(([category, items]) => (
          <div key={category} className="palette__group">
            <h3 className="palette__category">{CATEGORY_LABEL[category]}</h3>
            {items.map((descriptor) => (
              <button
                key={descriptor.kind}
                type="button"
                className="palette__item"
                onClick={() => onAdd(descriptor)}
              >
                <span className="palette__label">{descriptor.label}</span>
                {/* The description is inline rather than a tooltip: hover text is
                    invisible on touch and hides the thing that makes a node choosable. */}
                <span className="palette__description">{descriptor.description}</span>
              </button>
            ))}
          </div>
        ))}
      </section>

      <section className="legend">
        <h2 className="sidebar__heading">Connection types</h2>
        <p className="legend__hint">
          A connection is only legal between identical types. Converting between them is
          something a node does, explicitly. White ports carry whatever they are given
          and lock to a colour once a connection decides them.
        </p>
        <ul className="legend__list">
          <li className="legend__item">
            <span className="legend__swatch legend__swatch--generic" />
            <span className="legend__label">{GENERIC_STYLE.label}</span>
            <span className="legend__symbol">any</span>
          </li>
          {ALL_QUANTITIES.map((q) => {
            const s = styleOf(q);
            return (
              <li key={q} className="legend__item">
                <span className="legend__swatch" style={{ background: s.color }} />
                <span className="legend__label">{s.label}</span>
                <span className="legend__symbol">{s.symbol}</span>
              </li>
            );
          })}
        </ul>
      </section>
    </aside>
  );
}
