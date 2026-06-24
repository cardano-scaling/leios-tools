import { memo } from "react";
import {
  BaseEdge,
  EdgeLabelRenderer,
  getStraightPath,
  type EdgeProps,
} from "@xyflow/react";
import { useStore } from "@/store";

interface TopologyEdgeData {
  latency_ms: number;
  selected: boolean;
  flash?: "connected" | "disconnected" | null;
  status?: "connected" | "disconnected" | null;
  external?: boolean;
  // For external edges: the relay's address, used to find the measured RTT
  // in the connecting node's peer stats.
  relayAddress?: string;
}

type Props = EdgeProps & { data: TopologyEdgeData };

function TopologyEdgeInner({
  id,
  source,
  sourceX,
  sourceY,
  targetX,
  targetY,
  data,
}: Props) {
  const [edgePath, labelX, labelY] = getStraightPath({
    sourceX,
    sourceY,
    targetX,
    targetY,
  });

  const selected = data?.selected ?? false;
  const flash = data?.flash ?? null;
  const status = data?.status ?? null;
  const external = data?.external ?? false;

  // Label: internal edges show their (simulated) latency; external edges
  // show the measured RTT to the relay, or nothing when it isn't known yet
  // (the configured 0ms delay would be misleading for a real link).
  const sourceStats = useStore((s) => s.latestStats[source]);
  let label: string | null;
  if (external) {
    const rtt = sourceStats?.peers?.find(
      (p) => p.address === data?.relayAddress,
    )?.rtt_ms;
    label = rtt != null ? `${rtt.toFixed(0)}ms` : null;
  } else {
    label = `${data?.latency_ms ?? 0}ms`;
  }

  // Flash takes priority (event animation), then selection, then
  // steady-state status, then default.  Connected edges render
  // light green; disconnected edges render pink; unknown (no
  // events yet) stays gray — except external (Blue-team) edges, which
  // default to blue so a relay link is visible before any connect event.
  const stroke =
    flash === "connected"
      ? "#4caf50"
      : flash === "disconnected"
        ? "#e53935"
        : selected
          ? "#90caf9"
          : status === "disconnected"
            ? "#f48fb1"
            : status === "connected"
              ? "#a5d6a7"
              : external
                ? "#1976d2"
                : "#555";
  const strokeWidth = flash ? 3 : selected ? 2 : 1;

  return (
    <>
      <BaseEdge
        id={id}
        path={edgePath}
        style={{
          stroke,
          strokeWidth,
          // Dash external (relay) edges so they read distinctly from
          // internal links even when carrying live connect status.
          ...(external ? { strokeDasharray: "6 4" } : {}),
        }}
      />
      {label != null && (
        <EdgeLabelRenderer>
          <div
            style={{
              position: "absolute",
              transform: `translate(-50%, -50%) translate(${labelX}px,${labelY}px)`,
              fontSize: 10,
              color: selected ? "#90caf9" : "#888",
              pointerEvents: "all",
              background: "#121212cc",
              padding: "0 3px",
              borderRadius: 2,
            }}
          >
            {label}
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  );
}

export const TopologyEdge = memo(TopologyEdgeInner);
