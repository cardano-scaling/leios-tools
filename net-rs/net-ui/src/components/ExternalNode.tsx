import { memo } from "react";
import { Handle, Position, type NodeProps } from "@xyflow/react";
import { Box, Typography } from "@mui/material";

// External ("Blue team") relay node. Unlike TopologyNode it reports no
// telemetry (we don't control it), so it shows its address instead of a tip.
// Rendered as a blue diamond to contrast with our red-accented circles.
interface ExternalNodeData {
  label: string;
  address: string;
  selected: boolean;
}

type Props = NodeProps & { data: ExternalNodeData };

const handleStyle = {
  opacity: 0,
  top: "50%",
  left: "50%",
  transform: "translate(-50%, -50%)",
  pointerEvents: "none" as const,
};

function ExternalNodeInner({ data }: Props) {
  const { label, address, selected } = data;

  return (
    <Box
      sx={{
        width: 60,
        height: 60,
        // Diamond: a rotated square. Inner content is counter-rotated.
        transform: "rotate(45deg)",
        bgcolor: selected ? "#1565c0" : "#0d47a1",
        border: selected ? 4 : 2,
        borderColor: selected ? "#90caf9" : "#1976d2",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        cursor: "pointer",
        position: "relative",
        transition: "background-color 0.3s, border-color 0.3s",
        "&:hover": { borderColor: "#bbdefb" },
      }}
    >
      <Box
        sx={{
          transform: "rotate(-45deg)",
          textAlign: "center",
          width: 76,
        }}
      >
        <Typography variant="caption" fontWeight="bold" lineHeight={1.1}>
          {label}
        </Typography>
        <Typography
          variant="caption"
          fontSize={7}
          color="text.secondary"
          display="block"
          lineHeight={1.1}
          sx={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
        >
          {address}
        </Typography>
      </Box>
      <Handle type="target" position={Position.Top} style={handleStyle} />
      <Handle type="source" position={Position.Bottom} style={handleStyle} />
    </Box>
  );
}

export const ExternalNode = memo(ExternalNodeInner);
