import { useEffect, useRef } from "react";
import {
  forceSimulation,
  forceLink,
  forceManyBody,
  forceCenter,
  type Simulation,
  type SimulationNodeDatum,
  type SimulationLinkDatum,
} from "d3-force";
import { useStore } from "@/store";

interface ForceNode extends SimulationNodeDatum {
  nodeId: string;
}

export function useForceLayout() {
  const topology = useStore((s) => s.topology);
  const setNodePositions = useStore((s) => s.setNodePositions);
  const simRef = useRef<Simulation<ForceNode, SimulationLinkDatum<ForceNode>> | null>(null);

  useEffect(() => {
    if (!topology || topology.nodes.length === 0) return;

    if (simRef.current) {
      simRef.current.stop();
      simRef.current = null;
    }

    // Build one combined node array: internal nodes first (so their indices
    // match topology.edges' numeric from/to), external nodes appended after.
    const externalNodes = topology.external_nodes ?? [];
    const externalEdges = topology.external_edges ?? [];
    const total = topology.nodes.length + externalNodes.length;

    const nodeArray: ForceNode[] = [
      ...topology.nodes.map((n, i) => ({
        nodeId: n.node_id,
        x: Math.cos((2 * Math.PI * i) / total) * 200,
        y: Math.sin((2 * Math.PI * i) / total) * 200,
      })),
      ...externalNodes.map((e, k) => {
        const i = topology.nodes.length + k;
        return {
          nodeId: e.id,
          // External nodes seed slightly further out so they ring the cluster.
          x: Math.cos((2 * Math.PI * i) / total) * 260,
          y: Math.sin((2 * Math.PI * i) / total) * 260,
        };
      }),
    ];

    const idToIndex = new Map<string, number>();
    nodeArray.forEach((n, i) => idToIndex.set(n.nodeId, i));

    // Internal links use numeric from/to (valid indices into the prefix);
    // external links resolve `from` (internal index) directly and `to`
    // (external id) through idToIndex.
    const internalLinks: SimulationLinkDatum<ForceNode>[] = topology.edges.map(
      (e) => ({
        source: e.from,
        target: e.to,
      }),
    );
    // Drop external edges whose `to` doesn't resolve to a known node rather
    // than silently linking them to internal node 0. Used everywhere below
    // (links, latency lookup) so positional indices stay aligned.
    const validExternalEdges = externalEdges.filter((e) => idToIndex.has(e.to));
    const externalLinks: SimulationLinkDatum<ForceNode>[] = validExternalEdges.map(
      (e) => ({
        source: e.from,
        target: idToIndex.get(e.to)!,
      }),
    );
    const linkArray = [...internalLinks, ...externalLinks];

    // Scale link distance by latency (min 80, max 300)
    const allLatencies = [
      ...topology.edges.map((e) => e.latency_ms),
      ...validExternalEdges.map((e) => e.latency_ms),
    ];
    const maxLatency = Math.max(...allLatencies, 1);

    const sim = forceSimulation<ForceNode>(nodeArray)
      .force(
        "link",
        forceLink<ForceNode, SimulationLinkDatum<ForceNode>>(linkArray)
          .distance((_, i) => {
            // linkArray is internal edges then external edges; index past the
            // internal edges into the external set, else fall back to default.
            const internalCount = topology.edges.length;
            const lat =
              i < internalCount
                ? topology.edges[i]?.latency_ms
                : validExternalEdges[i - internalCount]?.latency_ms;
            return lat != null ? 80 + (lat / maxLatency) * 220 : 150;
          })
          .strength(0.4),
      )
      .force("charge", forceManyBody().strength(-800))
      .force("center", forceCenter(0, 0))
      .alpha(1)
      .alphaDecay(0.05);

    // Throttle position updates: at most once per animation frame
    let dirty = false;
    let rafId: number | null = null;

    function publishPositions() {
      rafId = null;
      if (!dirty) return;
      dirty = false;
      const positions: Record<string, { x: number; y: number }> = {};
      for (const n of nodeArray) {
        positions[n.nodeId] = { x: n.x ?? 0, y: n.y ?? 0 };
      }
      setNodePositions(positions);
    }

    sim.on("tick", () => {
      dirty = true;
      if (rafId == null) {
        rafId = requestAnimationFrame(publishPositions);
      }
    });

    sim.on("end", () => {
      // Final publish to ensure we capture the settled positions
      dirty = true;
      publishPositions();
      simRef.current = null;
    });

    simRef.current = sim;

    return () => {
      sim.stop();
      if (rafId != null) cancelAnimationFrame(rafId);
    };
  }, [topology, setNodePositions]);
}
