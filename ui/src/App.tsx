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
  resolveTypes,
  setGraph,
  snapshot as fetchSnapshot,
  type Graph,
  type HardwareInventory,
  type NodeDescriptor,
  type NodeInstance,
  type SnapshotDto,
  type ValidationError,
} from './api';
import {
  applyToGraph,
  edgeId,
  freshId,
  groupErrors,
  portKey,
  portsOf,
  toFlowEdges,
  toFlowNodes,
  toPortTypeMap,
  type PortTypeMap,
} from './graph';
import { placeNode } from './layout';
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
  // Selection is held here rather than read off the canvas, because rebuilding the
  // canvas discards React Flow's own selection flags — which would close the inspector
  // on every keystroke.
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // Inferred port types for the *staged* graph, so a port locks to a colour as soon as
  // a connection decides it rather than waiting for Apply.
  const [portTypes, setPortTypes] = useState<PortTypeMap>(new Map());

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
    setNodes(
      toFlowNodes(graph, catalogue, portTypes).map((node) =>
        node.id === selectedId ? { ...node, selected: true } : node,
      ),
    );
    setEdges(toFlowEdges(graph));
    // `selectedId` is deliberately not a dependency: re-selecting should not rebuild the
    // whole canvas, it only needs to survive a rebuild that happens for another reason.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [graph, catalogue, portTypes, setNodes, setEdges]);

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

  useEffect(() => {
    if (!graph || !IN_APP) return;
    let cancelled = false;
    resolveTypes(applyToGraph(graph, nodes, edges))
      .then((list) => {
        if (!cancelled) setPortTypes(toPortTypeMap(list));
      })
      .catch(() => {
        /* Inference is presentational; a failed call just leaves ports undecided. */
      });
    return () => {
      cancelled = true;
    };
    // Positions cannot affect types, so `nodes` is deliberately not a dependency —
    // including it would re-infer on every frame of a drag.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [graph, edges]);

  // --- Connection rules ----------------------------------------------------------------

  const quantityOf = useCallback(
    (nodeId: string | null, handleId: string | null | undefined, side: 'source' | 'target') => {
      if (!graph || !nodeId) return undefined;
      const instance = graph.nodes[nodeId];
      if (!instance) return undefined;
      if (handleId) return portTypes.get(portKey(nodeId, handleId)) ?? undefined;
      // React Flow allows a null handle when a node has exactly one port on that side.
      const { inputs, outputs } = portsOf(nodeId, instance, catalogue, portTypes);
      const ports = side === 'source' ? outputs : inputs;
      return ports[0]?.quantity ?? undefined;
    },
    [graph, catalogue, portTypes],
  );

  const isValidConnection = useCallback<IsValidConnection<Edge>>(
    (connection) => {
      const source = quantityOf(connection.source, connection.sourceHandle, 'source');
      const sink = quantityOf(connection.target, connection.targetHandle, 'target');
      // Undecided ports accept anything: connecting them is what decides them.
      return connects(source, sink);
    },
    [quantityOf],
  );

  const onConnect = useCallback(
    (connection: Connection) => {
      const source = quantityOf(connection.source, connection.sourceHandle, 'source');
      const sink = quantityOf(connection.target, connection.targetHandle, 'target');
      if (!connects(source, sink)) {
        // React Flow already refuses the edge; this exists so the refusal is *explained*
        // rather than the drag silently doing nothing.
        if (source && sink) setRejection(rejectionReason(source, sink));
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

      // Sources left, sinks right, transforms between, each column aligned. See layout.ts.
      const placed = placeNode(nodes, descriptor);
      const position: [number, number] = [placed.x, placed.y];

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

  /**
   * Replace one node in the staged document.
   *
   * Folds the canvas back in first, for the same reason adding does: editing from the
   * last applied document would silently undo every drag made since.
   */
  const updateNode = useCallback(
    (id: string, next: NodeInstance) => {
      if (!graph) return;
      const current = applyToGraph(graph, nodes, edges);
      const position = current.nodes[id]?.position ?? next.position;
      setLocalGraph({
        ...current,
        nodes: { ...current.nodes, [id]: { ...next, position } },
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
        onSelectionChange={({ nodes: picked }) => setSelectedId(picked[0]?.id ?? null)}
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
        selectedId={selectedId}
        selectedNode={selectedId ? (graph?.nodes[selectedId] ?? null) : null}
        selectedType={
          selectedId
            ? (portTypes.get(portKey(selectedId, 'in')) ??
              portTypes.get(portKey(selectedId, 'out')) ??
              null)
            : null
        }
        onChangeNode={updateNode}
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
