/**
 * A graph node rendered from its typed port signature.
 *
 * Every node in OpenFan is drawn by this one component: the node's ports come from the
 * backend's `NodeKind::spec()`, so the editor never hard-codes what a node looks like and
 * a new node kind needs no new React.
 *
 * The port type is communicated three ways — colour, the unit symbol on the handle, and
 * the port's label — so the graph stays readable without relying on colour alone.
 */
import { Handle, Position, type NodeProps, type Node } from '@xyflow/react';
import { formatValue, styleOf, type Quantity } from '../quantities';

export interface TypedPort {
  key: string;
  label: string;
  quantity: Quantity;
  /** Variadic inputs accept any number of incoming connections. */
  variadic?: boolean;
}

export interface TypedNodeData extends Record<string, unknown> {
  title: string;
  subtitle?: string;
  inputs: TypedPort[];
  outputs: TypedPort[];
  /** Live value per output port key, streamed from the backend's tick.  */
  readouts?: Record<string, number>;
}

export type TypedNodeType = Node<TypedNodeData, 'typed'>;

function PortRow({
  port,
  side,
  readout,
}: {
  port: TypedPort;
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
  return (
    <div className={`typed-node${selected ? ' typed-node--selected' : ''}`}>
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
    </div>
  );
}
