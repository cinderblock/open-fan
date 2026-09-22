/**
 * Translation between the backend's document and React Flow's view of it.
 *
 * The backend owns the graph. These functions convert it for display and convert edits
 * back — they never invent structure. In particular, a node's ports come from the
 * catalogue descriptor for its kind, not from anything the editor decides, so the editor
 * cannot offer a connection the backend would reject.
 */
import type { Edge as FlowEdge } from '@xyflow/react';

import type { Graph, NodeDescriptor, NodeInstance, PortDto } from './api';
import type { PortTypeDto } from './bindings/PortTypeDto';
import type { Quantity } from './bindings/Quantity';
import type { TypedNodeType } from './nodes/TypedNode';

/** The serde tag identifying a node's kind. */
export function kindTag(node: NodeInstance): string {
  return (node.kind as { kind: string }).kind;
}

/**
 * The resolved type of every port, keyed `nodeId.port`.
 *
 * `null` means the port is generic and inference has not decided it. Computed by the
 * backend so the editor does not carry a second implementation of unification.
 */
export type PortTypeMap = Map<string, Quantity | null>;

export function portKey(nodeId: string, port: string): string {
  return `${nodeId}.${port}`;
}

export function toPortTypeMap(list: PortTypeDto[]): PortTypeMap {
  return new Map(list.map((t) => [portKey(t.nodeId, t.port), t.quantity]));
}

/**
 * Ports for a node: the shape comes from the catalogue, the types from inference.
 *
 * A descriptor's own port types are those of its *template*, which for a generic node
 * means "undecided". Overlaying the resolved types is what makes a Hold show degrees
 * once it is wired to a temperature.
 */
export function portsOf(
  nodeId: string,
  node: NodeInstance,
  catalogue: NodeDescriptor[],
  types: PortTypeMap,
): { inputs: PortDto[]; outputs: PortDto[] } {
  const descriptor = catalogue.find((d) => d.kind === kindTag(node));
  if (!descriptor) return { inputs: [], outputs: [] };

  const resolve = (port: PortDto): PortDto => {
    const key = portKey(nodeId, port.key);
    return types.has(key) ? { ...port, quantity: types.get(key) ?? null } : port;
  };

  return {
    inputs: descriptor.inputs.map(resolve),
    outputs: descriptor.outputs.map(resolve),
  };
}

/** A short subtitle for a node: its kind, plus the thing it is bound to. */
function subtitleOf(node: NodeInstance, descriptor?: NodeDescriptor): string {
  const params = node.kind as unknown as Record<string, unknown>;
  const label = descriptor?.label ?? kindTag(node);
  if (typeof params.sensor_id === 'string' && params.sensor_id) return params.sensor_id;
  if (typeof params.channel === 'string' && params.channel) return params.channel;
  return label;
}

/**
 * Convert the backend document into React Flow nodes.
 *
 * Structure only — no readouts and no errors. Those are merged into existing nodes by the
 * editor, because rebuilding from the document resets every node to its *stored* position
 * and would throw away drags that have not been applied yet.
 */
export function toFlowNodes(
  graph: Graph,
  catalogue: NodeDescriptor[],
  types: PortTypeMap,
): TypedNodeType[] {
  return Object.entries(graph.nodes).map(([id, node]) => {
    const tag = kindTag(node);
    const descriptor = catalogue.find((d) => d.kind === tag);
    const { inputs, outputs } = portsOf(id, node, catalogue, types);

    return {
      id,
      type: 'typed' as const,
      position: { x: node.position[0], y: node.position[1] },
      data: {
        title: node.label || (descriptor?.label ?? tag),
        subtitle: subtitleOf(node, descriptor),
        inputs,
        outputs,
      },
    };
  });
}

/** Convert the backend document's edges into React Flow edges. */
export function toFlowEdges(graph: Graph): FlowEdge[] {
  return graph.edges.map((edge) => ({
    id: edgeId(edge.from.node, edge.from.port, edge.to.node, edge.to.port),
    source: edge.from.node,
    sourceHandle: edge.from.port,
    target: edge.to.node,
    targetHandle: edge.to.port,
  }));
}

export function edgeId(
  source: string,
  sourceHandle: string,
  target: string,
  targetHandle: string,
): string {
  return `${source}.${sourceHandle}->${target}.${targetHandle}`;
}

/**
 * Fold React Flow's view back into the backend document.
 *
 * Only positions and edges are taken from the view; node parameters stay as the backend
 * has them, because the canvas has no way to express them and must not be able to clobber
 * them by omission.
 */
export function applyToGraph(
  graph: Graph,
  flowNodes: TypedNodeType[],
  flowEdges: FlowEdge[],
): Graph {
  const positions = new Map(flowNodes.map((n) => [n.id, n.position]));

  const nodes: Graph['nodes'] = {};
  for (const [id, node] of Object.entries(graph.nodes)) {
    const position = positions.get(id);
    nodes[id] = position ? { ...node, position: [position.x, position.y] } : node;
  }

  return {
    nodes,
    edges: flowEdges
      // An edge needs both handles to be meaningful; React Flow allows them to be null
      // when a node has a single port, which the document format does not.
      .filter((e) => e.sourceHandle && e.targetHandle)
      .map((e) => ({
        from: { node: e.source, port: e.sourceHandle as string },
        to: { node: e.target, port: e.targetHandle as string },
      })),
  };
}

/** A unique node id that does not collide with anything already in the graph. */
export function freshId(graph: Graph, tag: string): string {
  let n = 1;
  while (Object.hasOwn(graph.nodes, `${tag}-${n}`)) n += 1;
  return `${tag}-${n}`;
}

/** Group validation errors by the node they point at. */
export function groupErrors(errors: { message: string; nodeId: string | null }[]) {
  const byNode = new Map<string, string[]>();
  for (const error of errors) {
    if (!error.nodeId) continue;
    byNode.set(error.nodeId, [...(byNode.get(error.nodeId) ?? []), error.message]);
  }
  return byNode;
}
