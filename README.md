# leios-tools

Rust tooling for Ouroboros Leios, extracted from
[input-output-hk/ouroboros-leios](https://github.com/input-output-hk/ouroboros-leios)
with full per-directory git history preserved.

## Workspaces

This repository contains three independent Cargo workspaces:

- **`shared-rs/`** — shared crates (`consensus`, `tcp-model`) used by the other two.
- **`net-rs/`** — networking simulator (`net-core`, `net-cli`, `net-node`, `net-cluster`).
- **`sim-rs/`** — the Rust Leios simulator (`sim-core`, `sim-cli`).

`net-rs` and `sim-rs` depend on `shared-rs` via relative `../../shared-rs/...`
path dependencies, so the directory layout above must be preserved.

## Supporting files

A few files outside the workspaces are kept, with their original paths intact:

- **`data/simulation/config.default.yaml`**, **`config.schema.json`** — the
  default parameters and schema. `sim-rs/parameters/*` symlinks resolve to
  these via `../../data/simulation/...`, so the relative layout must be kept.
- **`data/simulation/pseudo-mainnet/topology-v*`** — the pseudo-mainnet
  topology family used as simulator inputs.
- **`.github/workflows/sim-rs.yaml`**, **`net-node-docker.yaml`** — CI for the
  sim-rs build and the net-node docker image.

## Building

Each workspace builds independently from its own directory, e.g.:

```sh
cd shared-rs && cargo build
cd net-rs    && cargo build
cd sim-rs    && cargo build
```

## License

Apache-2.0. See [LICENSE](LICENSE).
