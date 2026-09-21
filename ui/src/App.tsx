/**
 * The graph editor.
 *
 * Phase 1 scaffold: the canvas, the typed-port node renderer and the connection rule are
 * real and enforced. The graph is still local demo state — wiring it to the backend's
 * document, live readouts and the node palette is Phase 2.
 */
import { useCallback, useMemo, useState } from 'react';
import {
  Background,
  Controls,
  MiniMap,
  ReactFlow,
  addEdge,
  useEdgesState,
  useNodesState,
  type Connection,
  type Edge,
  type IsValidConnection,
} from '@xyflow/react';
import '@xyflow/react/dist/style.css';

import TypedNode, { type TypedNodeType } from './nodes/TypedNode';
import { ALL_QUANTITIES, connects, rejectionReason, styleOf, type Quantity } from './quantities';

const nodeTypes = { typed: TypedNode };

const initialNodes: TypedNodeType[] = [
  {
    id: 'cpu',
    type: 'typed',
    position: { x: 0, y: 40 },
    data: {
      title: 'CPU package',
      subtitle: 'Sensor',
      inputs: [],
      outputs: [{ key: 'out', label: 'Reading', quantity: 'temperature' }],
      readouts: { out: 54.3 },
    },
  },
  {
    id: 'gpu',
    type: 'typed',
    position: { x: 0, y: 190 },
    data: {
      title: 'GPU load',
      subtitle: 'Sensor',
      inputs: [],
      outputs: [{ key: 'out', label: 'Reading', quantity: 'load' }],
      readouts: { out: 71 },
    },
  },
  {
    id: 'curve',
    type: 'typed',
    position: { x: 280, y: 40 },
    data: {
      title: 'Quiet curve',
      subtitle: 'Curve',
      inputs: [{ key: 'in', label: 'Input', quantity: 'temperature' }],
      outputs: [{ key: 'out', label: 'Duty', quantity: 'duty' }],
      readouts: { out: 38 },
    },
  },
  {
    id: 'fan',
    type: 'typed',
    position: { x: 560, y: 60 },
    data: {
      title: 'Front intake',
      subtitle: 'Fan output',
      inputs: [{ key: 'duty', label: 'Duty', quantity: 'duty' }],
      outputs: [],
    },
  },
];

const initialEdges: Edge[] = [
  { id: 'cpu->curve', source: 'cpu', sourceHandle: 'out', target: 'curve', targetHandle: 'in' },
  { id: 'curve->fan', source: 'curve', sourceHandle: 'out', target: 'fan', targetHandle: 'duty' },
];

export default function App() {
  const [nodes, , onNodesChange] = useNodesState<TypedNodeType>(initialNodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>(initialEdges);
  const [rejection, setRejection] = useState<string | null>(null);

  /** Look up the declared quantity of a specific handle on a specific node. */
  const quantityOf = useCallback(
    (nodeId: string | null, handleId: string | null | undefined, side: 'source' | 'target') => {
      const node = nodes.find((n) => n.id === nodeId);
      if (!node) return undefined;
      const ports = side === 'source' ? node.data.outputs : node.data.inputs;
      // A node with exactly one port on that side may omit the handle id.
      const port = handleId ? ports.find((p) => p.key === handleId) : ports[0];
      return port?.quantity;
    },
    [nodes],
  );

  const isValidConnection = useCallback<IsValidConnection<Edge>>(
    (connection) => {
      const source = quantityOf(connection.source, connection.sourceHandle, 'source');
      const sink = quantityOf(connection.target, connection.targetHandle, 'target');
      if (!source || !sink) return false;
      return connects(source, sink);
    },
    [quantityOf],
  );

  const onConnect = useCallback(
    (connection: Connection) => {
      const source = quantityOf(connection.source, connection.sourceHandle, 'source');
      const sink = quantityOf(connection.target, connection.targetHandle, 'target');
      if (!source || !sink) return;

      if (!connects(source, sink)) {
        // React Flow already refuses the edge via isValidConnection; this exists so the
        // refusal is *explained* rather than the drag silently doing nothing.
        setRejection(rejectionReason(source, sink));
        return;
      }

      setRejection(null);
      setEdges((eds) =>
        addEdge({ ...connection, style: { stroke: styleOf(source).color, strokeWidth: 2 } }, eds),
      );
    },
    [quantityOf, setEdges],
  );

  // Colour existing edges by the quantity they carry.
  const styledEdges = useMemo(
    () =>
      edges.map((edge) => {
        const q = quantityOf(edge.source, edge.sourceHandle, 'source');
        return q
          ? { ...edge, style: { stroke: styleOf(q).color, strokeWidth: 2, ...edge.style } }
          : edge;
      }),
    [edges, quantityOf],
  );

  return (
    <div className="app">
      <ReactFlow
        nodes={nodes}
        edges={styledEdges}
        nodeTypes={nodeTypes}
        onNodesChange={onNodesChange}
        onEdgesChange={onEdgesChange}
        onConnect={onConnect}
        isValidConnection={isValidConnection}
        colorMode="dark"
        fitView
        proOptions={{ hideAttribution: false }}
      >
        <Background gap={20} />
        <Controls />
        <MiniMap pannable zoomable />
      </ReactFlow>

      <aside className="legend">
        <h2 className="legend__title">Connection types</h2>
        <p className="legend__hint">
          A connection is only legal between identical types. Converting between them is
          something a node does, explicitly.
        </p>
        <ul className="legend__list">
          {ALL_QUANTITIES.map((q: Quantity) => {
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
      </aside>

      {rejection && (
        <div className="rejection" role="status">
          <span>{rejection}</span>
          <button type="button" onClick={() => setRejection(null)} aria-label="Dismiss">
            ×
          </button>
        </div>
      )}
    </div>
  );
}
