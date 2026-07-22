# smod

A `cargo`/`npm`/`pnpm`/`brew`-style package manager for **Solana modules**.

`smod` installs, removes, and inspects reusable on-chain modules, tracking them
in a human-readable `smod.toml` manifest and a `smod.lock` lockfile. It is built
as a small, well-factored core that a real package manager could grow out of:
the registry backend is swappable (bundled data today, HTTP later) and every
non-trivial behavior is unit-testable without spawning a terminal.

## Features

- **Manifest + lockfile** — `smod.toml` (declared dependencies) and `smod.lock`
  (exact installed versions, checksums, and install timestamps).
- **Verified installs** — archives are SHA-256 checksummed and verified against
  the registry before extraction, with zip-slip protection.
- **Swappable registry** — a `RegistryClient` trait behind which today's
  embedded/mock backend can be replaced by HTTP without touching any command.
- **Scriptable** — failed runs exit non-zero; batch installs report per-package
  success/failure so `$?` is meaningful.

## Installation

Requires Rust **1.75+** (for native `async fn` in traits).

```bash
git clone https://github.com/MLYTC1/smod.git smod
cd smod
cargo build --release
# the binary is at target/release/smod (smod.exe on Windows)
```

To install it onto your `PATH`:

```bash
cargo install --path .
```

## Usage

```bash
# Start a new project in the current directory
smod init --name my-app

# Discover packages
smod search vault
smod info payment-stream

# Install a single package (updates smod.toml + smod.lock, extracts to smod_modules/)
smod install payment-stream

# Install every dependency declared in smod.toml
smod install

# Inspect what's declared vs. installed
smod list

# Remove a package (deletes its module dir + lockfile + manifest entries)
smod remove payment-stream

# Update installed packages to the newest version the registry offers
smod update              # all installed packages
smod update payment-stream   # just one

# Verify installed packages match their recorded checksums
smod verify

# Diagnose the local project and environment
smod doctor
```

Global flags: `--no-color` disables colored output; `--verbose` is accepted
(reserved for future logging).

### Bundled registry

Out of the box `smod` ships with a small embedded registry (`registry.json`)
containing sample Solana modules: `payment-stream`, `token-vault`, `nft-mint`,
`staking-pool`, and `oracle-feed`. Their archives live under `packages/`.

### Not yet implemented

`smod publish` is intentionally unimplemented today. It fails clearly with a
non-zero exit code (rather than silently doing nothing) and explains what is
needed to implement it.

## Project layout

```
src/
├── main.rs          # entry point: parse argv, dispatch (no logic)
├── cli.rs           # clap schema (pure data)
├── commands/        # one file per subcommand (orchestration only)
├── installer.rs     # install/remove/update/verify workflows (business logic)
├── doctor.rs        # environment diagnostics (business logic)
├── config.rs        # smod.toml filesystem boundary
├── lockfile.rs      # smod.lock read/write + timestamps
├── registry.rs      # RegistryClient trait + MockRegistryClient
├── package.rs       # pure Manifest data model
└── ui/              # spinners/progress bars (presentation only)
```

The one rule the codebase protects: **commands never do work — they ask a lower
layer to do the work, then print the result.** Business-logic modules never
print; they return typed `thiserror` errors that commands surface via `anyhow`.

## Development

```bash
cargo fmt
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

## Architecture

The full design — layering, the install pipeline walkthrough, the registry
abstraction, how HTTP support will fit in later, and the error-handling and
testing strategies — is documented in **[ARCHITECTURE.md](./ARCHITECTURE.md)**.

## License

MIT
