/**
 * The graph editor.
 *
 * The backend owns the document and every control decision. This component reads that
 * document, lets it be edited, and hands edits back to be validated — nothing here
 * affects a fan directly, and closing this window changes nothing about cooling.
 *
 * Edits are staged and applied explicitly rather than written through on every drag. A
 * half-finished graph is a normal intermediate state while wiring something up, and it
 * should not be a state the engine is asked to run.
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
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

import {
  IN_APP,
  getGraph,
  inventory,
  nodeCatalogue,
  setGraph,
  snapshot as fetchSnapshot,
  type Graph,
  type HardwareInventory,
  type NodeDescriptor,
  type SnapshotDto,
  type ValidationError,
} from './api';
import {
  applyToGraph,
  edgeId,
  freshId,
  groupErrors,
  portsOf,
  toFlowEdges,
  toFlowNodes,
} from './graph';
import TypedNode, { type TypedNodeType } from './nodes/TypedNode';
import { connects, rejectionReason, styleOf } from './quantities';
import Sidebar from './Sidebar';

const nodeTypes = { typed: TypedNode };

/** How often to ask the engine what it last did. */
const SNAPSHOT_INTERVAL_MS = 250;

export default function App() {
  const [graph, setLocalGraph] = useState<Graph | null>(null);
  const [catalogue, setCatalogue] = useState<NodeDescriptor[]>([]);
  const [hardware, setHardware] = useState<HardwareInventory | null>(null);
  const [snapshot, setSnapshot] = useState<SnapshotDto | null>(null);
  const [errors, setErrors] = useState<ValidationError[]>([]);
  const [rejection, setRejection] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  const [nodes, setNodes, onNodesChange] = useNodesState<TypedNodeType>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);

  // --- Load ---------------------------------------------------------------------------

  useEffect(() => {
    if (!IN_APP) {
      setLoadError(
        'Not running inside the OpenFan app, so there is no backend to talk to. Start it with `bun run tauri dev`.',
      );
      return;
    }
    Promise.all([nodeCatalogue(), getGraph(), inventory()])
      .then(([cat, doc, inv]) => {
        setCatalogue(cat);
        setLocalGraph(doc);
        setHardware(inv);
      })
      .catch((err) => setLoadError(String(err)));
  }, []);

  useEffect(() => {
    if (!IN_APP) return;
    let cancelled = false;
    const poll = () => {
      fetchSnapshot()
        .then((s) => {
          if (!cancelled) setSnapshot(s);
        })
        .catch(() => {
          /* A missed poll is not worth surfacing; the next one will land. */
        });
    };
    poll();
    const timer = setInterval(poll, SNAPSHOT_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  // --- Document -> canvas --------------------------------------------------------------

  const errorsByNode = useMemo(() => groupErrors(errors), [errors]);

  useEffect(() => {
    if (!graph) return;
    // Rebuild structure only when the document or catalogue changes. Readouts and
    // validation errors are merged in below instead of being rebuilt from, because a
    // rebuild resets every node to its *document* position — discarding drags that have
    // not been applied yet.
    setNodes(toFlowNodes(graph, catalogue));
    setEdges(toFlowEdges(graph));
  }, [graph, catalogue, setNodes, setEdges]);

  useEffect(() => {
    setNodes((current) =>
      current.map((node) => {
        const next = errorsByNode.get(node.id);
        if (next === node.data.errors) return node;
        return { ...node, data: { ...node.data, errors: next } };
      }),
    );
  }, [errorsByNode, setNodes]);

  useEffect(() => {
    if (!snapshot) return;
    const readouts = new Map<string, Record<string, number>>();
    for (const wire of snapshot.wires) {
      if (wire.value === null) continue;
      const existing = readouts.get(wire.nodeId) ?? {};
      existing[wire.port] = wire.value;
      readouts.set(wire.nodeId, existing);
    }
    setNodes((current) =>
      current.map((node) => {
        const next = readouts.get(node.id);
        if (next === node.data.readouts) return node;
        return { ...node, data: { ...node.data, readouts: next } };
      }),
    );
  }, [snapshot, setNodes]);

  // --- Connection rules ----------------------------------------------------------------

  const quantityOf = useCallback(
    (nodeId: string | null, handleId: string | null | undefined, side: 'source' | 'target') => {
      if (!graph || !nodeId) return undefined;
      const instance = graph.nodes[nodeId];
      if (!instance) return undefined;
      const { inputs, outputs } = portsOf(instance, catalogue);
      const ports = side === 'source' ? outputs : inputs;
      const port = handleId ? ports.find((p) => p.key === handleId) : ports[0];
      return port?.quantity;
    },
    [graph, catalogue],
  );

  const isValidConnection = useCallback<IsValidConnection<Edge>>(
    (connection) => {
      const source = quantityOf(connection.source, connection.sourceHandle, 'source');
      const sink = quantityOf(connection.target, connection.targetHandle, 'target');
      return !!source && !!sink && connects(source, sink);
    },
    [quantityOf],
  );

  const onConnect = useCallback(
    (connection: Connection) => {
      const source = quantityOf(connection.source, connection.sourceHandle, 'source');
      const sink = quantityOf(connection.target, connection.targetHandle, 'target');
      if (!source || !sink) return;

      if (!connects(source, sink)) {
        // React Flow already refuses the edge; this exists so the refusal is *explained*
        // rather than the drag silently doing nothing.
        setRejection(rejectionReason(source, sink));
        return;
      }
      setRejection(null);
      setDirty(true);
      setEdges((eds) =>
        addEdge(
          {
            ...connection,
            id: edgeId(
              connection.source,
              connection.sourceHandle ?? '',
              connection.target,
              connection.targetHandle ?? '',
            ),
            style: { stroke: styleOf(source).color, strokeWidth: 2 },
          },
          eds,
        ),
      );
    },
    [quantityOf, setEdges],
  );

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

  // --- Editing -------------------------------------------------------------------------

  const addNode = useCallback(
    (descriptor: NodeDescriptor) => {
      if (!graph) return;
      // Fold the canvas back in first. Editing from the last *applied* document would
      // discard every drag made since, snapping the whole graph back into place.
      const current = applyToGraph(graph, nodes, edges);
      const id = freshId(current, descriptor.kind);

      // Place the new node clear of the existing ones rather than on top of them.
      const right = nodes.reduce((max, n) => Math.max(max, n.position.x), -Infinity);
      const position: [number, number] = Number.isFinite(right)
        ? [right + 240, 80 + (Object.keys(current.nodes).length % 4) * 130]
        : [80, 80];

      setLocalGraph({
        ...current,
        nodes: {
          ...current.nodes,
          [id]: { kind: structuredClone(descriptor.template), label: '', position },
        },
      });
      setDirty(true);
    },
    [graph, nodes, edges],
  );

  const deleteSelected = useCallback(() => {
    if (!graph) return;
    const doomed = new Set(nodes.filter((n) => n.selected).map((n) => n.id));
    if (doomed.size === 0) return;

    // As with adding: start from the canvas so unapplied positions survive.
    const current = applyToGraph(graph, nodes, edges);
    const remaining: Graph['nodes'] = {};
    for (const [id, node] of Object.entries(current.nodes)) {
      if (!doomed.has(id)) remaining[id] = node;
    }
    setLocalGraph({
      nodes: remaining,
      // Edges touching a deleted node would dangle; the backend rejects those, so drop
      // them here rather than making the user discover it on apply.
      edges: current.edges.filter((e) => !doomed.has(e.from.node) && !doomed.has(e.to.node)),
    });
    setDirty(true);
  }, [graph, nodes, edges]);

  const apply = useCallback(async () => {
    if (!graph) return;
    const next = applyToGraph(graph, nodes, edges);
    const result = await setGraph(next);
    if (result.ok) {
      setErrors([]);
      setDirty(false);
      // Re-read rather than assuming: the backend is the authority on what is running.
      setLocalGraph(await getGraph());
    } else {
      setErrors(result.errors);
    }
  }, [graph, nodes, edges]);

  const revert = useCallback(async () => {
    setErrors([]);
    setRejection(null);
    setDirty(false);
    setLocalGraph(await getGraph());
  }, []);

  // --- Render --------------------------------------------------------------------------

  if (loadError) {
    return (
      <div className="app app--message">
        <div className="message">
          <h1>OpenFan</h1>
          <p>{loadError}</p>
        </div>
      </div>
    );
  }

  return (
    <div className="app">
      <ReactFlow
        nodes={nodes}
        edges={styledEdges}
        nodeTypes={nodeTypes}
        onNodesChange={(changes) => {
          if (changes.some((c) => c.type === 'position' || c.type === 'remove')) {
            setDirty(true);
          }
          onNodesChange(changes);
        }}
        onEdgesChange={(changes) => {
          if (changes.some((c) => c.type === 'remove')) setDirty(true);
          onEdgesChange(changes);
        }}
        onConnect={onConnect}
        isValidConnection={isValidConnection}
        colorMode="dark"
        fitView
      >
        <Background gap={20} />
        <Controls />
        <MiniMap pannable zoomable />
      </ReactFlow>

      <Sidebar
        catalogue={catalogue}
        hardware={hardware}
        snapshot={snapshot}
        errors={errors}
        dirty={dirty}
        onAdd={addNode}
        onApply={apply}
        onRevert={revert}
        onDelete={deleteSelected}
        hasSelection={nodes.some((n) => n.selected)}
      />

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
