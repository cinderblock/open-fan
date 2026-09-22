/**
 * The parameter editor for the selected node.
 *
 * Controls are generated from the schema the backend publishes alongside each node kind,
 * not hand-written per variant. Adding a node kind therefore needs no change here, and
 * the editor cannot offer a field the document does not have — a spec's `key` is the
 * serde field name, so applying an edit is `{ ...kind, [key]: value }` and nothing more.
 *
 * Editing is local until Apply, like the rest of the canvas. Writing straight through
 * would mean a half-typed setpoint briefly became the one the engine was running.
 */
import { useId } from 'react';

import type { HardwareInventory, NodeDescriptor, NodeInstance, NodeKind } from './api';
import type { ChoiceOption } from './bindings/ChoiceOption';
import type { CurvePoint } from './bindings/CurvePoint';
import type { ParamSpec } from './bindings/ParamSpec';
import type { ParamUnit } from './bindings/ParamUnit';
import type { Quantity } from './bindings/Quantity';
import { ALL_QUANTITIES, styleOf } from './quantities';

/** A node kind as a plain bag of fields, which is what editing manipulates. */
type KindRecord = Record<string, unknown>;

interface Props {
  nodeId: string;
  node: NodeInstance;
  descriptor: NodeDescriptor | undefined;
  inventory: HardwareInventory | null;
  /**
   * What this node turned out to be carrying, from inference. `null` while it is still
   * generic — units then read as bare numbers, which is the honest rendering.
   */
  nodeType: Quantity | null;
  /**
   * The tachometer belonging to this node's channel, when it has one.
   *
   * A header and its tachometer are one device, but the tach is a measurement rather
   * than a return value, so it lives in its own node. Saying so here is what stops that
   * being a thing you have to already know.
   */
  device: DeviceHint | null;
  onChange: (next: NodeInstance) => void;
  onAddSensor: (sensorId: string) => void;
}

export interface DeviceHint {
  sensorId: string;
  label: string;
  /** Whether a sensor node already reads it. */
  present: boolean;
}

/**
 * Resolve what a numeric parameter is measured in.
 *
 * Quantity-relative units read the node's own type field, so a Hold band shows degrees on
 * a temperature and percent on a duty without the backend having to guess.
 */
function unitSymbol(unit: ParamUnit, nodeType: Quantity | null): string {
  switch (unit.unit) {
    case 'none':
      return '';
    case 'fixed':
      return unit.symbol;
    case 'quantity':
    case 'quantity-rate': {
      // Undecided types have no honest symbol, so show none rather than guessing.
      const symbol = nodeType ? styleOf(nodeType).symbol : '';
      if (unit.unit === 'quantity-rate') return symbol ? `${symbol}/s` : '/s';
      return symbol;
    }
  }
}

function Field({
  label,
  help,
  children,
}: {
  label: string;
  help?: string | null;
  children: React.ReactNode;
}) {
  return (
    <label className="field">
      <span className="field__label">{label}</span>
      {children}
      {/* Help is inline, never a tooltip: hover text is invisible on touch and hides the
          thing that makes a parameter understandable. */}
      {help && <span className="field__help">{help}</span>}
    </label>
  );
}

function NumberInput({
  value,
  min,
  max,
  step,
  suffix,
  onChange,
}: {
  value: number;
  min?: number | null;
  max?: number | null;
  step: number;
  suffix: string;
  onChange: (n: number) => void;
}) {
  return (
    <span className="field__control">
      <input
        type="number"
        className="input"
        value={Number.isFinite(value) ? value : 0}
        min={min ?? undefined}
        max={max ?? undefined}
        step={step}
        onChange={(e) => {
          const next = e.currentTarget.valueAsNumber;
          // An empty or unparseable box must not write NaN into the document; leave the
          // last good value until something valid is typed.
          if (Number.isFinite(next)) onChange(next);
        }}
      />
      {suffix && <span className="field__suffix">{suffix}</span>}
    </span>
  );
}

function CurveEditor({
  points,
  inputQuantity,
  onChange,
}: {
  points: CurvePoint[];
  inputQuantity: Quantity | null;
  onChange: (next: CurvePoint[]) => void;
}) {
  const sorted = [...points].sort((a, b) => a.x - b.x);
  const inputStyle = styleOf(inputQuantity);
  const dutyStyle = styleOf('duty');

  // Plot over the curve's own domain, padded so the endpoints are not on the frame.
  const xs = sorted.map((p) => p.x);
  const lo = xs.length > 0 ? Math.min(...xs) : 0;
  const hi = xs.length > 0 ? Math.max(...xs) : 100;
  const span = hi - lo || 1;
  const W = 250;
  const H = 96;
  const px = (x: number) => ((x - lo) / span) * (W - 16) + 8;
  const py = (y: number) => H - 8 - (Math.max(0, Math.min(100, y)) / 100) * (H - 16);

  const update = (i: number, patch: Partial<CurvePoint>) =>
    onChange(sorted.map((p, j) => (i === j ? { ...p, ...patch } : p)));

  return (
    <div className="curve">
      <svg className="curve__plot" viewBox={`0 0 ${W} ${H}`} role="img" aria-label="Transfer curve">
        {[0, 50, 100].map((duty) => (
          <line
            key={duty}
            x1={0}
            x2={W}
            y1={py(duty)}
            y2={py(duty)}
            className="curve__grid"
          />
        ))}
        {sorted.length > 1 && (
          <polyline
            points={sorted.map((p) => `${px(p.x)},${py(p.y)}`).join(' ')}
            fill="none"
            stroke={dutyStyle.color}
            strokeWidth={2}
          />
        )}
        {sorted.map((p, i) => (
          <circle key={i} cx={px(p.x)} cy={py(p.y)} r={3} fill={inputStyle.color} />
        ))}
      </svg>

      <table className="curve__table">
        <thead>
          <tr>
            <th>{inputStyle.symbol || inputStyle.label}</th>
            <th>{dutyStyle.symbol}</th>
            <th aria-label="Remove" />
          </tr>
        </thead>
        <tbody>
          {sorted.map((point, i) => (
            <tr key={i}>
              <td>
                <input
                  type="number"
                  className="input input--tight"
                  value={point.x}
                  step={1}
                  onChange={(e) => {
                    const x = e.currentTarget.valueAsNumber;
                    if (Number.isFinite(x)) update(i, { x });
                  }}
                />
              </td>
              <td>
                <input
                  type="number"
                  className="input input--tight"
                  value={point.y}
                  min={0}
                  max={100}
                  step={1}
                  onChange={(e) => {
                    const y = e.currentTarget.valueAsNumber;
                    if (Number.isFinite(y)) update(i, { y });
                  }}
                />
              </td>
              <td>
                <button
                  type="button"
                  className="curve__remove"
                  // A curve with no points evaluates to a fault, so refuse to empty it.
                  disabled={sorted.length <= 2}
                  onClick={() => onChange(sorted.filter((_, j) => j !== i))}
                >
                  ×
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      <button
        type="button"
        className="button button--small"
        onClick={() => {
          const last = sorted[sorted.length - 1];
          onChange([...sorted, { x: (last?.x ?? 0) + 10, y: Math.min(100, (last?.y ?? 0) + 10) }]);
        }}
      >
        Add point
      </button>
    </div>
  );
}

function Control({
  spec,
  kind,
  inventory,
  nodeType,
  onSet,
}: {
  spec: ParamSpec;
  kind: KindRecord;
  inventory: HardwareInventory | null;
  nodeType: Quantity | null;
  onSet: (patch: KindRecord) => void;
}) {
  const value = kind[spec.key];
  const set = (v: unknown) => onSet({ [spec.key]: v });

  switch (spec.kind.type) {
    case 'number':
      return (
        <NumberInput
          value={typeof value === 'number' ? value : 0}
          min={spec.kind.min}
          max={spec.kind.max}
          step={spec.kind.step}
          suffix={unitSymbol(spec.kind.unit, nodeType)}
          onChange={set}
        />
      );

    case 'integer':
      return (
        <NumberInput
          value={typeof value === 'number' ? value : 0}
          min={spec.kind.min}
          max={spec.kind.max}
          step={1}
          suffix=""
          onChange={(n) => set(Math.round(n))}
        />
      );

    case 'quantity':
      return (
        <select
          className="input"
          value={String(value ?? 'ratio')}
          onChange={(e) => set(e.currentTarget.value)}
        >
          {ALL_QUANTITIES.map((q) => {
            const s = styleOf(q);
            return (
              <option key={q} value={q}>
                {s.label}
                {s.symbol ? ` (${s.symbol})` : ''}
              </option>
            );
          })}
        </select>
      );

    case 'choice':
      return (
        <select
          className="input"
          value={String(value ?? '')}
          onChange={(e) => set(e.currentTarget.value)}
        >
          {spec.kind.options.map((option: ChoiceOption) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
      );

    case 'sensor': {
      const sensors = inventory?.sensors ?? [];
      return (
        <select
          className="input"
          value={String(value ?? '')}
          onChange={(e) => {
            const id = e.currentTarget.value;
            const sensor = sensors.find((s) => s.id === id);
            // Set the declared quantity alongside the id. A sensor read as the wrong
            // quantity faults, so leaving the two to be set separately would make a
            // dead node the normal outcome of picking from this list.
            onSet(sensor ? { [spec.key]: id, quantity: sensor.quantity } : { [spec.key]: id });
          }}
        >
          <option value="">— choose a sensor —</option>
          {sensors.map((sensor) => (
            <option key={sensor.id} value={sensor.id}>
              {sensor.label} · {sensor.id}
            </option>
          ))}
        </select>
      );
    }

    case 'channel': {
      const channels = inventory?.channels ?? [];
      return (
        <select
          className="input"
          value={String(value ?? '')}
          onChange={(e) => set(e.currentTarget.value)}
        >
          <option value="">— choose a channel —</option>
          {channels.map((channel) => (
            <option key={channel.id} value={channel.id}>
              {channel.label} · {channel.id}
            </option>
          ))}
        </select>
      );
    }

    case 'curve':
      return (
        <CurveEditor
          points={Array.isArray(value) ? (value as CurvePoint[]) : []}
          inputQuantity={nodeType}
          onChange={set}
        />
      );
  }
}

export default function NodeInspector({
  nodeId,
  node,
  descriptor,
  inventory,
  nodeType,
  device,
  onChange,
  onAddSensor,
}: Props) {
  const labelId = useId();
  const kind = node.kind as unknown as KindRecord;

  const patch = (fields: KindRecord) =>
    onChange({ ...node, kind: { ...kind, ...fields } as unknown as NodeKind });

  return (
    <section className="inspector">
      <h2 className="sidebar__heading">
        {descriptor?.label ?? String(kind.kind)} · {nodeId}
      </h2>

      <label className="field" htmlFor={labelId}>
        <span className="field__label">Name</span>
        <input
          id={labelId}
          type="text"
          className="input"
          value={node.label}
          placeholder={descriptor?.label ?? ''}
          onChange={(e) => onChange({ ...node, label: e.currentTarget.value })}
        />
      </label>

      {descriptor?.params.map((spec) => (
        <Field key={spec.key} label={spec.label} help={spec.help}>
          <Control
            spec={spec}
            kind={kind}
            inventory={inventory}
            nodeType={nodeType}
            onSet={patch}
          />
        </Field>
      ))}

      {device && (
        <div className="device-hint">
          <span className="device-hint__text">
            Speed is read by <strong>{device.label}</strong>. It is a separate sensor
            node, not an output of this one — the reading reflects an earlier duty.
          </span>
          {device.present ? (
            <span className="device-hint__linked">Linked on the canvas</span>
          ) : (
            <button
              type="button"
              className="button button--small"
              onClick={() => onAddSensor(device.sensorId)}
            >
              Add its sensor node
            </button>
          )}
        </div>
      )}

      {!descriptor && (
        <p className="field__help">
          This node kind is not in the catalogue, so it cannot be edited here.
        </p>
      )}
    </section>
  );
}
