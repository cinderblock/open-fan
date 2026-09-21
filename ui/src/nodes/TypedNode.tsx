/**
 * A graph node rendered from its typed port signature.
 *
 * Every node in OpenFan is drawn by this one component: a node's ports come from the
 * backend's catalogue, so the editor never hard-codes what a node looks like and a new
 * node kind needs no new React.
 *
 * The port type is communicated three ways — colour, the unit symbol on the handle, and
 * the port's label — so the graph stays readable without relying on colour alone.
 */
import { Handle, Position, type Node, type NodeProps } from '@xyflow/react';

import type { PortDto } from '../api';
import { formatValue, styleOf } from '../quantities';

export interface TypedNodeData extends Record<string, unknown> {
  title: string;
  subtitle?: string;
  inputs: PortDto[];
  outputs: PortDto[];
  /** Live value per output port key, from the engine's most recent tick. */
  readouts?: Record<string, number>;
  /** Validation messages the backend attributed to this node. */
  errors?: string[];
}

export type TypedNodeType = Node<TypedNodeData, 'typed'>;

function PortRow({
  port,
  side,
  readout,
}: {
  port: PortDto;
  side: 'input' | 'output';
  readout?: number;
}) {
  const style = styleOf(port.quantity);
  const isInput = side === 'input';

  return (
    <div className={`port-row port-row--${side}`}>
      <Handle
        type={isInput ? 'target' : 'source'}
        position={isInput ? Position.Left : Position.Right}
        id={port.key}
        className={`port-handle${port.variadic ? ' port-handle--variadic' : ''}`}
        style={{ background: style.color, borderColor: style.color }}
      />
      <span className="port-label">{port.label}</span>
      <span className="port-type" style={{ color: style.color }}>
        {style.symbol || style.label}
      </span>
      {readout !== undefined && (
        <span className="port-readout">{formatValue(port.quantity, readout)}</span>
      )}
    </div>
  );
}

export default function TypedNode({ data, selected }: NodeProps<TypedNodeType>) {
  const faulted = data.errors && data.errors.length > 0;

  return (
    <div
      className={[
        'typed-node',
        selected ? 'typed-node--selected' : '',
        faulted ? 'typed-node--error' : '',
      ]
        .filter(Boolean)
        .join(' ')}
    >
      <header className="typed-node__header">
        <span className="typed-node__title">{data.title}</span>
        {data.subtitle && <span className="typed-node__subtitle">{data.subtitle}</span>}
      </header>

      <div className="typed-node__ports">
        {data.inputs.map((port) => (
          <PortRow key={port.key} port={port} side="input" />
        ))}
        {data.outputs.map((port) => (
          <PortRow
            key={port.key}
            port={port}
            side="output"
            readout={data.readouts?.[port.key]}
          />
        ))}
      </div>

      {/* Errors are shown inline rather than in a tooltip: the whole point of reporting
          every problem at once is that they are all visible at once. */}
      {faulted && (
        <ul className="typed-node__errors">
          {data.errors?.map((message) => (
            <li key={message}>{message}</li>
          ))}
        </ul>
      )}
    </div>
  );
}
