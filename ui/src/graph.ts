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
import type { TypedNodeType } from './nodes/TypedNode';

/** The serde tag identifying a node's kind. */
export function kindTag(node: NodeInstance): string {
  return (node.kind as { kind: string }).kind;
}

/** Ports for a node, looked up from the catalogue. */
export function portsOf(
  node: NodeInstance,
  catalogue: NodeDescriptor[],
): { inputs: PortDto[]; outputs: PortDto[] } {
  const tag = kindTag(node);
  const descriptor = catalogue.find((d) => d.kind === tag);
  if (!descriptor) return { inputs: [], outputs: [] };

  // The catalogue's descriptor is built from a *template*, whose quantity parameters may
  // differ from this instance's. Re-derive the quantities from the instance so a Mix set
  // to Temperature does not render Duty-coloured ports.
  const params = node.kind as unknown as Record<string, unknown>;
  const retype = (port: PortDto): PortDto => {
    const candidate =
      port.key === 'out' && typeof params.to === 'string'
        ? params.to
        : port.key === 'in' && typeof params.from === 'string'
          ? params.from
          : typeof params.quantity === 'string'
            ? params.quantity
            : typeof params.input === 'string' && port.key === 'in'
              ? params.input
              : undefined;
    return candidate ? { ...port, quantity: candidate as PortDto['quantity'] } : port;
  };

  // A Curve always emits a duty and a Comparator always emits a boolean, whatever their
  // input is; only re-type ports the instance actually parameterises.
  const fixedOutput = tag === 'curve' || tag === 'comparator' || tag === 'pid';
  return {
    inputs: descriptor.inputs.map(retype),
    outputs: fixedOutput ? descriptor.outputs : descriptor.outputs.map(retype),
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
export function toFlowNodes(graph: Graph, catalogue: NodeDescriptor[]): TypedNodeType[] {
  return Object.entries(graph.nodes).map(([id, node]) => {
    const tag = kindTag(node);
    const descriptor = catalogue.find((d) => d.kind === tag);
    const { inputs, outputs } = portsOf(node, catalogue);

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
