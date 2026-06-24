import { useMemo, useState } from "react";
import {
  Box,
  Button,
  Checkbox,
  Divider,
  FormControlLabel,
  Radio,
  RadioGroup,
  Slider,
  Stack,
  Switch,
  TextField,
  Typography,
} from "@mui/material";
import { useStore } from "@/store";
import type {
  AttackRequest,
  BehaviourSelection,
  BehaviourSpec,
} from "@/types";

// All behaviours the backend's runtime attack path accepts. `honest` and
// `composite` are omitted from the picker — honest is a no-op, and composite
// is produced automatically when more than one behaviour is selected.
const BEHAVIOURS = [
  { kind: "lazy-voter", label: "LazyVoter" },
  { kind: "rb-header-equivocator", label: "RbHeaderEquivocator" },
  { kind: "lie-about-eb-size", label: "LieAboutEbSize" },
  { kind: "echo-to-source", label: "EchoToSource" },
  { kind: "t22", label: "T22" },
  { kind: "deep-reorg", label: "DeepReorg" },
  { kind: "drop-inbound-peers", label: "DropInboundPeers" },
] as const;

type SelectionKind =
  | "all"
  | "nodes"
  | "stake-random"
  | "stake-ordered"
  | "stake-fraction";

const numberFieldSx = {
  "& input[type=number]::-webkit-inner-spin-button, & input[type=number]::-webkit-outer-spin-button":
    {
      appearance: "auto",
      filter: "invert(1)",
    },
};

function parseIndices(csv: string): number[] {
  return csv
    .split(/[,\s]+/)
    .map((tok) => tok.trim())
    .filter((tok) => tok.length > 0)
    .map((tok) => Number(tok))
    .filter((n) => Number.isFinite(n) && Number.isInteger(n) && n >= 0);
}

const clamp = (n: number, lo: number, hi: number) =>
  Math.min(hi, Math.max(lo, n));
const intOr = (s: string, fallback: number) => {
  const n = Math.floor(Number(s));
  return Number.isFinite(n) ? n : fallback;
};

function describeBehaviour(b: BehaviourSpec): string {
  if (b.kind === "composite")
    return `composite[${b.children.map(describeBehaviour).join(" + ")}]`;
  if (b.kind === "rb-header-equivocator")
    return `${b.kind}(ways=${b.ways})`;
  if (b.kind === "lie-about-eb-size")
    return `${b.kind}(${b.scale_num}/${b.scale_den}+${b.offset})`;
  return b.kind;
}

export function AttackPanel() {
  const topology = useStore((s) => s.topology);
  const activeAttack = useStore((s) => s.activeAttack);
  const triggerAttack = useStore((s) => s.triggerAttack);
  const stopAttack = useStore((s) => s.stopAttack);

  const numNodes = topology?.nodes.length ?? 1;
  const countMax = Math.max(1, numNodes);

  // Which behaviours are checked (one → sent directly, many → composite).
  const [enabled, setEnabled] = useState<Set<string>>(new Set());
  const toggle = (kind: string) =>
    setEnabled((prev) => {
      const next = new Set(prev);
      if (next.has(kind)) next.delete(kind);
      else next.add(kind);
      return next;
    });

  // Per-behaviour params.
  const [ways, setWays] = useState("2");
  const [lieNum, setLieNum] = useState("0");
  const [lieDen, setLieDen] = useState("1");
  const [lieOff, setLieOff] = useState("0");
  const [t22Vote, setT22Vote] = useState("128");
  const [t22NonVoting, setT22NonVoting] = useState("128");
  const [t22Hide, setT22Hide] = useState(false);
  const [reorgEvery, setReorgEvery] = useState("50");
  const [reorgDepth, setReorgDepth] = useState("3");
  const [dropProb, setDropProb] = useState(0.5);

  const [selectionKind, setSelectionKind] = useState<SelectionKind>("all");
  const [nodeIndicesCsv, setNodeIndicesCsv] = useState("");
  const [count, setCount] = useState(1);
  const [fractionPct, setFractionPct] = useState(20);

  const indicesPreview = useMemo(() => parseIndices(nodeIndicesCsv), [nodeIndicesCsv]);

  const buildChild = (kind: string): BehaviourSpec | null => {
    switch (kind) {
      case "lazy-voter":
        return { kind: "lazy-voter" };
      case "rb-header-equivocator":
        return { kind: "rb-header-equivocator", ways: clamp(intOr(ways, 2), 2, 8) };
      case "lie-about-eb-size":
        return {
          kind: "lie-about-eb-size",
          scale_num: Math.max(0, intOr(lieNum, 0)),
          scale_den: Math.max(1, intOr(lieDen, 1)),
          offset: intOr(lieOff, 0),
        };
      case "echo-to-source":
        return { kind: "echo-to-source" };
      case "t22":
        return {
          kind: "t22",
          vote_threshold: clamp(intOr(t22Vote, 128), 0, 255),
          non_voting_threshold: clamp(intOr(t22NonVoting, 128), 0, 255),
          hide_eb_tx_received: t22Hide,
        };
      case "deep-reorg":
        return {
          kind: "deep-reorg",
          every_slots: Math.max(1, intOr(reorgEvery, 50)),
          depth: Math.max(1, intOr(reorgDepth, 3)),
        };
      case "drop-inbound-peers":
        return { kind: "drop-inbound-peers", probability: dropProb };
      default:
        return null;
    }
  };

  // Selected behaviours, in the panel's fixed order. One → that spec; many →
  // wrapped in a composite (matches the backend's Composite variant).
  const buildSpec = (): BehaviourSpec | null => {
    const children = BEHAVIOURS.map((b) => b.kind)
      .filter((k) => enabled.has(k))
      .map(buildChild)
      .filter((c): c is BehaviourSpec => c !== null);
    if (children.length === 0) return null;
    if (children.length === 1) return children[0] ?? null;
    return { kind: "composite", children };
  };

  const buildSelection = (): BehaviourSelection | null => {
    switch (selectionKind) {
      case "all":
        return { kind: "all" };
      case "nodes":
        if (indicesPreview.length === 0) return null;
        return { kind: "nodes", indices: indicesPreview };
      case "stake-random":
        return { kind: "stake-random", count };
      case "stake-ordered":
        return { kind: "stake-ordered", count };
      case "stake-fraction":
        return { kind: "stake-fraction", fraction: fractionPct / 100 };
    }
  };

  const indicesValid =
    selectionKind !== "nodes" ||
    (indicesPreview.length > 0 && indicesPreview.every((i) => i < numNodes));

  const canTrigger =
    !activeAttack && buildSpec() !== null && buildSelection() !== null && indicesValid;

  const handleTrigger = () => {
    const spec = buildSpec();
    const selection = buildSelection();
    if (!spec || !selection) return;
    const req: AttackRequest = { behaviour: spec, selection };
    void triggerAttack(req);
  };

  const handleStop = () => {
    void stopAttack();
  };

  const disabled = !!activeAttack;
  const numProps = (extra?: object) => ({
    type: "number" as const,
    size: "small" as const,
    disabled,
    sx: { width: 92, ...numberFieldSx },
    slotProps: { htmlInput: extra },
  });

  return (
    <Box
      sx={{
        p: 2,
        display: "flex",
        flexDirection: "column",
        gap: 1.5,
        width: 280,
      }}
    >
      <Typography variant="subtitle2" sx={{ color: "#ff7043", fontWeight: 700 }}>
        Attack Trigger
      </Typography>

      {activeAttack ? (
        <Box
          sx={{
            p: 1,
            borderRadius: 1,
            border: "1px solid rgba(255,112,67,0.6)",
            bgcolor: "rgba(255,112,67,0.08)",
          }}
        >
          <Typography variant="caption" sx={{ color: "#ffab91" }}>
            Active: <b>{describeBehaviour(activeAttack.behaviour)}</b>
            <br />
            {activeAttack.indices.length} node
            {activeAttack.indices.length === 1 ? "" : "s"}:&nbsp;
            {activeAttack.indices.slice(0, 8).join(", ")}
            {activeAttack.indices.length > 8 ? ", …" : ""}
          </Typography>
        </Box>
      ) : (
        <Typography variant="caption" sx={{ color: "text.secondary" }}>
          No attack active.
        </Typography>
      )}

      <Divider sx={{ borderColor: "rgba(255,255,255,0.1)" }} />

      <Typography variant="caption" sx={{ color: "text.secondary" }}>
        Behaviours {enabled.size > 1 ? "(composite)" : ""}
      </Typography>

      {BEHAVIOURS.map((b) => {
        const on = enabled.has(b.kind);
        return (
          <Box key={b.kind}>
            <FormControlLabel
              control={
                <Checkbox
                  size="small"
                  checked={on}
                  disabled={disabled}
                  onChange={() => toggle(b.kind)}
                />
              }
              label={<Typography variant="body2">{b.label}</Typography>}
            />
            {on && b.kind === "rb-header-equivocator" && (
              <Stack direction="row" spacing={1} sx={{ ml: 3, mb: 0.5 }}>
                <TextField
                  label="ways"
                  value={ways}
                  onChange={(e) => setWays(e.target.value)}
                  {...numProps({ min: 2, max: 8, step: 1 })}
                />
              </Stack>
            )}
            {on && b.kind === "lie-about-eb-size" && (
              <Stack direction="row" spacing={1} sx={{ ml: 3, mb: 0.5, flexWrap: "wrap", gap: 1 }}>
                <TextField
                  label="scale_num"
                  value={lieNum}
                  onChange={(e) => setLieNum(e.target.value)}
                  {...numProps({ min: 0, step: 1 })}
                />
                <TextField
                  label="scale_den"
                  value={lieDen}
                  onChange={(e) => setLieDen(e.target.value)}
                  {...numProps({ min: 1, step: 1 })}
                />
                <TextField
                  label="offset"
                  value={lieOff}
                  onChange={(e) => setLieOff(e.target.value)}
                  {...numProps({ step: 1 })}
                />
              </Stack>
            )}
            {on && b.kind === "t22" && (
              <Stack sx={{ ml: 3, mb: 0.5, gap: 1 }}>
                <Stack direction="row" spacing={1} sx={{ flexWrap: "wrap", gap: 1 }}>
                  <TextField
                    label="vote_thr"
                    value={t22Vote}
                    onChange={(e) => setT22Vote(e.target.value)}
                    {...numProps({ min: 0, max: 255, step: 1 })}
                  />
                  <TextField
                    label="non_vote_thr"
                    value={t22NonVoting}
                    onChange={(e) => setT22NonVoting(e.target.value)}
                    {...numProps({ min: 0, max: 255, step: 1 })}
                  />
                </Stack>
                <FormControlLabel
                  control={
                    <Switch
                      size="small"
                      checked={t22Hide}
                      disabled={disabled}
                      onChange={(e) => setT22Hide(e.target.checked)}
                    />
                  }
                  label={
                    <Typography variant="caption">hide_eb_tx_received</Typography>
                  }
                />
              </Stack>
            )}
            {on && b.kind === "deep-reorg" && (
              <Stack direction="row" spacing={1} sx={{ ml: 3, mb: 0.5, flexWrap: "wrap", gap: 1 }}>
                <TextField
                  label="every_slots"
                  value={reorgEvery}
                  onChange={(e) => setReorgEvery(e.target.value)}
                  {...numProps({ min: 1, step: 1 })}
                />
                <TextField
                  label="depth"
                  value={reorgDepth}
                  onChange={(e) => setReorgDepth(e.target.value)}
                  {...numProps({ min: 1, step: 1 })}
                />
              </Stack>
            )}
            {on && b.kind === "drop-inbound-peers" && (
              <Stack direction="row" spacing={2} alignItems="center" sx={{ ml: 3, mb: 0.5 }}>
                <Slider
                  value={dropProb}
                  min={0}
                  max={1}
                  step={0.05}
                  onChange={(_, v) => setDropProb(v as number)}
                  disabled={disabled}
                  valueLabelDisplay="auto"
                />
                <Typography variant="caption" sx={{ width: 56, textAlign: "right" }}>
                  p={dropProb.toFixed(2)}
                </Typography>
              </Stack>
            )}
          </Box>
        );
      })}

      <Divider sx={{ borderColor: "rgba(255,255,255,0.1)" }} />

      <Typography variant="caption" sx={{ color: "text.secondary" }}>
        Target nodes
      </Typography>
      <RadioGroup
        value={selectionKind}
        onChange={(e) => setSelectionKind(e.target.value as SelectionKind)}
      >
        <FormControlLabel
          value="all"
          control={<Radio size="small" disabled={disabled} />}
          label="All"
        />
        <FormControlLabel
          value="stake-ordered"
          control={<Radio size="small" disabled={disabled} />}
          label={`Top ${count} by stake`}
        />
        <FormControlLabel
          value="stake-random"
          control={<Radio size="small" disabled={disabled} />}
          label={`${count} random (stake>0)`}
        />
        <FormControlLabel
          value="stake-fraction"
          control={<Radio size="small" disabled={disabled} />}
          label={`Stake fraction (${fractionPct}%)`}
        />
        <FormControlLabel
          value="nodes"
          control={<Radio size="small" disabled={disabled} />}
          label="Specific indices"
        />
      </RadioGroup>

      {(selectionKind === "stake-ordered" || selectionKind === "stake-random") && (
        <Stack direction="row" spacing={2} alignItems="center">
          <Slider
            value={count}
            min={1}
            max={countMax}
            step={1}
            onChange={(_, v) => setCount(v as number)}
            disabled={disabled}
          />
          <TextField
            type="number"
            size="small"
            value={count}
            onChange={(e) => {
              const n = Math.max(1, Math.min(countMax, Number(e.target.value) || 1));
              setCount(n);
            }}
            disabled={disabled}
            slotProps={{ htmlInput: { min: 1, max: countMax, step: 1 } }}
            sx={{ width: 80, ...numberFieldSx }}
          />
        </Stack>
      )}

      {selectionKind === "stake-fraction" && (
        <Stack direction="row" spacing={2} alignItems="center">
          <Slider
            value={fractionPct}
            min={1}
            max={100}
            step={1}
            onChange={(_, v) => setFractionPct(v as number)}
            disabled={disabled}
            valueLabelDisplay="auto"
            valueLabelFormat={(v) => `${v}%`}
          />
          <Typography variant="caption" sx={{ width: 40, textAlign: "right" }}>
            {fractionPct}%
          </Typography>
        </Stack>
      )}

      {selectionKind === "nodes" && (
        <TextField
          label="Node indices (csv)"
          size="small"
          value={nodeIndicesCsv}
          onChange={(e) => setNodeIndicesCsv(e.target.value)}
          disabled={disabled}
          placeholder="0, 2, 5"
          helperText={
            !indicesValid
              ? `Out of range (0..${numNodes - 1})`
              : indicesPreview.length > 0
                ? `→ ${indicesPreview.length} node(s)`
                : ""
          }
          error={!indicesValid && nodeIndicesCsv.length > 0}
        />
      )}

      <Divider sx={{ borderColor: "rgba(255,255,255,0.1)" }} />

      <Button
        variant="contained"
        color="warning"
        onClick={handleTrigger}
        disabled={!canTrigger}
      >
        Trigger Attack
      </Button>
      <Button
        variant="outlined"
        color="warning"
        onClick={handleStop}
        disabled={!activeAttack}
      >
        Stop Attack
      </Button>
    </Box>
  );
}
