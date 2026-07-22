# smod Architecture

This document explains how `smod` is put together, and why. It's written for
Rust developers who want to contribute a feature, fix a bug, or just
understand what they're looking at before making a change.

If you only read one section, read [Layers](#layers) and
[The install flow](#the-install-flow-walkthrough) — together they explain the
one rule the rest of the codebase bends over backwards to protect:

> **Commands never do work. They ask a lower layer to do the work, then print
> the result.**

---

## Table of contents

- [Goals](#goals)
- [Layers](#layers)
- [Module map](#module-map)
- [Dependency graph](#dependency-graph)
- [The install flow (walkthrough)](#the-install-flow-walkthrough)
- [The registry abstraction](#the-registry-abstraction)
- [How HTTP support will fit in later](#how-http-support-will-fit-in-later-without-touching-commands)
- [Error handling strategy](#error-handling-strategy)
- [Testing strategy](#testing-strategy)
- [Adding a new command](#adding-a-new-command-a-checklist)
- [Known limitations](#known-limitations--things-a-contributor-should-know)

---

## Goals

`smod` is meant to feel like `cargo`/`npm`/`pnpm`/`brew` for Solana modules —
not just in its output, but in how it's built: a small, well-factored core
that a real package manager could grow out of. Two design goals follow
directly from that:

1. **The registry backend is swappable.** `smod` currently reads packages
   from a bundled JSON file. It will eventually talk to a real HTTP API.
   Nothing above the registry layer should need to change when that
   happens — see [How HTTP support will fit in](#how-http-support-will-fit-in-later-without-touching-commands).

2. **Business logic is unit-testable without a terminal.** Every non-trivial
   behavior (installing a package, editing `smod.toml`, parsing a lockfile)
   is a plain function or method you can call from a `#[test]`, with no
   `clap` parsing, no stdout, and no real registry involved. The CLI layer
   is deliberately "boring" — thin enough that it barely needs testing on
   its own.

---

## Layers

```
┌─────────────────────────────────────────────────────────────────┐
│  main.rs                                                        │
│  Parses argv, dispatches to a command module. No logic.         │
└───────────────────────────────┬───────────────────────────────┘
                                  │
┌───────────────────────────────▼───────────────────────────────┐
│  cli.rs                                                          │
│  clap's #[derive] schema for the whole program (the `smod`      │
│  struct and the `Commands` enum). Pure data - no behavior.       │
└───────────────────────────────┬───────────────────────────────┘
                                  │
┌───────────────────────────────▼───────────────────────────────┐
│  commands/*.rs   (CLI layer)                                    │
│  One file per subcommand. Each owns its *Args struct and an      │
│  async run(args). Orchestration only: call the business logic,   │
│  print colored/spinner output, map errors to exit codes.         │
└─────┬───────────────┬───────────────┬───────────────┬──────────┘
      │                │                │                │
┌─────▼──────┐  ┌──────▼──────┐  ┌──────▼──────┐  ┌──────▼──────┐
│ installer.rs│  │  config.rs  │  │ lockfile.rs │  │ registry.rs │
│ (business    │  │ (business   │  │ (business   │  │ (abstraction│
│  logic)      │  │  logic)     │  │  logic)     │  │  + mock)    │
└─────┬────────┘  └─────┬───────┘  └─────┬───────┘  └─────┬───────┘
      │                  │                 │                 │
      └──────────────────┴────────┬────────┴─────────────────┘
                                    │
                          ┌────────▼─────────┐
                          │    package.rs      │
                          │  (data model:       │
                          │   Manifest)          │
                          └────────────────────┘
```

Plus one layer that sits beside all of this rather than under it:

```
┌─────────────────┐
│      ui/*.rs      │   Presentation helpers (spinners, progress bars).
└─────────────────┘   Used by commands/, never by business logic -
                        installer.rs has no idea a terminal exists.
```

Why this shape specifically:

- **`commands/` is thin on purpose.** If you're tempted to write a `for`
  loop, a `match` with real branching, or anything touching `std::fs`
  directly inside a `commands/*.rs` file, that's a signal the logic belongs
  in `installer.rs`, `config.rs`, or `lockfile.rs` instead. The payoff:
  every interesting behavior has a test that runs in milliseconds with no
  process spawning, and the same logic is reusable from a future command
  (`update` already reuses `installer::remove_package` and
  `Installer::install_one` internally, for example).

- **Business logic modules never `println!`.** They return `Result<T, E>`
  with a concrete `E` (via `thiserror`), and it's the command's job to
  decide how to render success or failure. This is what lets tests assert
  on `matches!(err, InstallError::ChecksumMismatch { .. })` instead of
  scraping stdout.

- **`package.rs` sits at the bottom because everything reads it, and it
  reads nothing.** It's the one file that's pure data model (the `Manifest`
  struct and its `[smod.dependencies]` table) with no filesystem or
  registry awareness at all. `config.rs` is what connects `Manifest` to
  disk.

---

## Module map

### `src/main.rs`
The entire binary entry point. Parses `Cli::parse()`, applies `--no-color`,
and `match`es `cli.command` to the right `commands::*::run(args).await`.
That `match` (`dispatch`) is the *only* place that knows all the
subcommands exist. If `dispatch` returns `Err`, `main` prints
`error: {rendered chain}` in red and exits `1`. There is no logic here
beyond that — deliberately, so nothing about "how installing works" is
ever duplicated between `main.rs` and the command it's calling.

### `src/cli.rs`
The `clap` schema, and nothing else. `Cli` is the top-level struct
(`--verbose`, `--no-color`, plus the subcommand). `Commands` is an enum
with one variant per subcommand, each wrapping that command's own `*Args`
struct — which is defined *in the command's own file*, not here. This
keeps `cli.rs` from becoming a second place you need to edit every time a
command gains a flag.

### `src/commands/`
One file per subcommand (`new`, `init`, `install`, `search`, `remove`,
`list`, `publish`, `info`, `doctor`, `update`, `verify`), registered in
`commands/mod.rs`. That module root also defines one shared, reusable
`OutputArgs` (`--json`), flattened into the commands that support
machine-readable output (`list`, `search`, `info`) so the flag is defined
once rather than repeated per command. Every file follows the same shape:

```rust
#[derive(Args, Debug)]
pub struct FooArgs { /* clap flags/positional args */ }

pub async fn run(args: FooArgs) -> anyhow::Result<()> {
    // 1. resolve the project root / build a registry client
    // 2. call into installer.rs / config.rs / lockfile.rs
    // 3. print colored, cargo/npm-style output
}
```

`install.rs` and `list.rs` are the two commands with any real branching
(single-package vs. batch install; declared-vs-installed status), and even
there the branching is about *which business-logic call to make and how to
render its result* — never new logic. `doctor` and `verify` follow the same
rule: their checking logic lives in the business layer (`doctor.rs` and
`Installer::verify` respectively), and the command only formats the typed
result and maps it to an exit code. `new` follows it too: all project
generation lives in `scaffold.rs`, and the command only prints the files it
was told were created. `publish` remains an intentionally-unimplemented stub
today; it `bail!`s with a clear "not implemented yet" message (and a non-zero
exit code) rather than silently no-op-ing.

Presentation decisions — an aligned table vs. a labeled block, human text vs.
`--json`, staged progress lines — are all made here in the command layer.
`search`, `info`, and `list` branch on `OutputArgs::json` to either serialize
the business-layer data structure directly (`ui::json`) or render it for a
human (`ui::table` for `search`, labeled fields for `info`). `install`'s
single-package path renders `Installer`'s `InstallStage` progress callbacks
(resolving → verifying → extracting → updating lockfile); the installer reports
each stage, the command decides how to show it.

### `src/package.rs`
The data model for `smod.toml`: the `Manifest` struct (`name`, `version`,
`description`, `author`, `license`, `program_id`, `repository`) plus its
`smod: SmodSection` field, which holds the `[smod.dependencies]` table as
a `BTreeMap<String, String>` (sorted, so the file diffs predictably).
`Manifest::to_toml_string` / `from_toml_str` are the only place TOML
(de)serialization happens for the manifest. Nothing in this file touches
the filesystem — that's `config.rs`'s job.

Two pure version helpers also live here, since versions are plain strings in
the manifest/lockfile/registry: `compare_versions` (component-wise ordering)
and `VersionReq` — a small parser + matcher for the subset of requirement
syntax `smod` needs (`1.2.3` exact, `>=1.0.0` minimum, `^1.0` caret). This is
deliberately *not* a full semver crate; it is built on `compare_versions` and
kept in the data-model layer so requirement resolution never leaks into a
command (`commands::list` calls it to report whether an installed version
satisfies its declared requirement).

### `src/config.rs`
The filesystem boundary for `smod.toml`. Three groups of functions:

- **Location & detection** — `manifest_path_in`, `modules_dir_in`,
  `is_smod_project` (checks one exact directory), `find_project_root`
  (searches upward through ancestors, like `cargo`/`npm`), and
  `require_project_root` (the same search, wrapped into a friendly
  `anyhow` error commands can just `?` on).
- **Read/write** — `read_manifest` / `write_manifest` / `save_manifest`
  (an alias used by the dependency-editing functions below, so their
  "read → mutate → save" shape reads naturally) and `ensure_modules_dir`.
- **Dependency editing** — `add_dependency`, `remove_dependency`,
  `list_dependencies`. These are the *only* functions in the codebase
  that mutate `[smod.dependencies]`; `installer.rs` calls them rather than
  touching `Manifest` fields directly, which is what makes "duplicate
  dependencies are impossible" a property of a `BTreeMap` key instead of
  something every call site has to remember to check.

`ConfigError` (via `thiserror`) has three variants: `ManifestNotFound`
(no file at all — the "run `smod init`" case), `Io` (the file exists but
couldn't be read — permissions, or it's a directory; kept distinct from
`ManifestNotFound` so a permissions problem doesn't get misreported as
"you forgot to init"), and `InvalidManifest` (it parsed as a file but not
as valid TOML/schema).

### `src/lockfile.rs`
The `smod.lock` counterpart to `config.rs`. `LockedPackage` (`name`,
`version`, `checksum`, `installed_at`) and `Lockfile` (`packages: Vec<...>`,
serializing as repeated `[[packages]]` tables). `read`/`write` mirror
`config.rs`'s pattern, except a *missing* lockfile isn't an error —
`read` just returns an empty `Lockfile`, since "no lockfile yet" and "no
packages installed yet" are the same thing. `Lockfile::upsert` is what
guarantees re-installing a package updates its entry in place instead of
appending a duplicate.

One more thing lives here: a hand-rolled Unix-timestamp → RFC 3339 date
formatter (`now_rfc3339`, `civil_from_days`). It exists so `installed_at`
timestamps don't require pulling in a date/time crate — `civil_from_days`
is Howard Hinnant's public-domain calendar algorithm, unit-tested against
leap years, the leap-day/century-leap-year edge cases, and pre-epoch dates.

### `src/registry.rs`
See [The registry abstraction](#the-registry-abstraction) below — this is
the module most likely to matter to you if you're planning to add HTTP
support.

### `src/installer.rs`
The biggest file, and the heart of the business logic layer. Covered in
detail in [The install flow](#the-install-flow-walkthrough), but briefly:

- `Installer<'a, R: RegistryClient>` — generic over the registry client,
  so tests can swap in an in-memory `MockRegistryClient` pointed at a
  temp-directory archive instead of anything real.
  - `install` / `install_one` (an alias — see the doc comment on
    `install_one` for why both names exist) — install a single package.
  - `install_all` — install every dependency in `smod.toml`, skipping
    ones already in `smod.lock`. Continues past individual failures
    (each becomes a `DependencyOutcome::Failed`) rather than aborting the
    whole batch.
  - `compute_checksum`, `verify_checksum`, `extract_package`,
    `write_lockfile` — the individual pipeline steps, each a standalone
    method so they're independently testable.
- `remove_package` (a free function, not a method — it doesn't need a
  registry, just a project root and a name) — the inverse of `install`:
  deletes `smod_modules/<name>/`, drops the `smod.lock` entry, drops the
  `smod.toml` dependency entry.
- `InstallError` / `RemoveError` — `thiserror` enums covering every
  failure mode in the pipeline (missing package, missing/unreadable
  archive, checksum mismatch, invalid zip, extraction I/O failure,
  lockfile/manifest I/O failure, "not a smod project").

### `src/scaffold.rs`
The business logic behind `smod new`: generating a new project directory tree
(`smod.toml`, `src/lib.rs`, `README.md`). `create_project` validates the name
through the same `validate_package_name` gate everything else uses, refuses to
overwrite an existing destination, and returns a typed `CreatedProject`
(root + created file paths) or a `NewProjectError` — it never prints. It
*reuses* `package::Manifest` + `config::write_manifest` rather than
reimplementing manifest serialization, so `smod.toml` is generated exactly one
way. It lives in its own module (rather than in `config.rs`) because it does
more than the manifest boundary: it creates a subtree and writes several files.

### `src/doctor.rs`
The business logic behind `smod doctor`: environment diagnostics. It returns a
typed `DoctorReport { checks: Vec<DoctorCheck> }` (each `DoctorCheck` carries a
`name`, a `CheckStatus`, and a `message`) rather than printing — rendering is
`commands::doctor`'s job. It lives in its own module because a diagnostic is
cross-cutting: it composes `config`'s project detection, `lockfile`'s reader,
the `RegistryClient` abstraction, and `Installer::verify` (for the
modules-present / archives-accessible / checksums-valid checks) instead of
re-implementing any of them. `diagnose` never returns `Err` — every problem is
captured as a failing check so the command can show the whole picture.

Package integrity checking itself lives in `installer.rs` as
`Installer::verify` (backing both `smod verify` and `doctor`'s checksum check),
returning a `Vec<PackageVerification>`. It reuses the same `compute_checksum`
and archive-reading path (`read_archive`) as `install`, so no checksum logic is
duplicated.

### `src/ui/`
Presentation helpers, only ever touched by `commands/*.rs` and never by
business logic. Two tiny `indicatif` wrappers — `spinner::new(message)`
(indeterminate) and `progress::new(total)` (a determinate byte-progress bar,
styled and ready for streamed HTTP downloads, currently unused and that's
fine) — plus two output helpers: `table::render(headers, rows)` (fixed-width
aligned columns as plain text, so column widths aren't thrown off by ANSI
codes) and `json::print(value)` (pretty-prints any `Serialize` value to
stdout). Nothing here does real work; it only formats what a command hands it.

---

## Dependency graph

Arrows mean "imports / calls into," i.e. "depends on":

```
main.rs
  └─> cli.rs
  └─> commands::{new,init,install,search,remove,list,publish,info,doctor,update,verify}

commands::new        ─> scaffold
commands::init       ─> config, package, ui
commands::install    ─> config, installer, registry, ui
commands::search     ─> registry, ui
commands::info       ─> registry, ui
commands::remove     ─> config, installer
commands::list       ─> config, lockfile, package, ui
commands::update     ─> config, installer, registry, ui
commands::doctor     ─> doctor, registry
commands::verify     ─> config, installer, registry, ui
commands::publish    ─> (nothing yet - unimplemented stub)

scaffold   ─> config, package
doctor     ─> config, lockfile, installer, registry
installer  ─> config, lockfile, package, registry
config       ─> package
lockfile     ─> package (only the shared validate_package_name gate)
registry     ─> (nothing internal - self-contained)
package      ─> (nothing internal - leaf module)

ui  ─> (nothing internal - leaf module; external indicatif + serde_json)
```

Two things worth noticing:

- **The arrows only point one way.** `package.rs`, `lockfile.rs`, and
  `registry.rs` don't know `config.rs` or `installer.rs` exist. That's
  what makes each of them independently testable and (mostly)
  independently *understandable* — you can read `lockfile.rs` start to
  finish without any other file open.
- **`commands/*.rs` never import each other.** `install.rs` doesn't call
  into `remove.rs`; if two commands need the same behavior, that behavior
  lives in `installer.rs` and both commands call it from there. (`remove`
  and the internals of `install_all` are the concrete example today.)

---

## The install flow (walkthrough)

This is the most involved path in the codebase, so it's worth tracing
end-to-end. Two entry points converge on the same pipeline:

- `smod install payment-stream` → `commands::install::install_single` →
  `Installer::install_one` → `Installer::install`
- `smod install` (no argument) → `commands::install::install_all` →
  `Installer::install_all`, which calls `Installer::install_one` once per
  undeclared-as-installed dependency

`Installer::install` is a thin wrapper over `install_with_progress`, which
takes a `&mut dyn FnMut(InstallStage)` callback and invokes it as each stage is
entered (`Resolving` → `Verifying` → `Extracting` → `UpdatingLockfile`). This
is how `commands::install` renders staged progress without the installer ever
printing; `install` itself just passes a no-op closure. Either way, the same
steps run in order, returning at the first failure:

```
 1. config::is_smod_project(project_root)?          -> InstallError::NotASmodProject
 2. registry.get_package(name)                       -> InstallError::Registry(..)
 3. resolve_archive_path(info.archive)                (local file today; see below)
 4. std::fs::read(archive_path)                      -> ArchiveMissing / ArchiveIo
 5. compute_checksum(bytes)
 6. verify_checksum(&info, &checksum)                -> ChecksumMismatch
 7. extract_package(name, bytes, smod_modules/<name>) -> InvalidArchive / ExtractIo
 8. write_lockfile(&info, &checksum)                 -> Lockfile(..)
 9. config::add_dependency(project_root, name, ver)  -> Manifest(..)
10. return InstalledPackage { info, checksum, install_path }
```

A few steps deserve more detail:

- **Step 3, `resolve_archive_path`:** `PackageInfo::archive` is a path
  like `"./packages/payment-stream.zip"`. Absolute paths are used as-is
  (this is what tests use, pointing at fixtures in a temp directory);
  relative paths are resolved against `env!("CARGO_MANIFEST_DIR")` — i.e.
  "next to this crate's own `Cargo.toml`." That's a stand-in for "wherever
  archives actually live," and it's the one place in the pipeline that
  will change shape when HTTP support lands (see the next section).

- **Step 7, `extract_package`:** uses `enclosed_name()` from the `zip`
  crate, which refuses absolute paths and `..` components — this is what
  prevents a malicious archive from writing outside `smod_modules/`
  ("zip slip"). It also detects whether every entry in the archive shares
  one common top-level directory (the conventional
  `payment-stream/README.md` layout) and, if so, strips it — but only if
  stripping wouldn't erase a root-level file. (An archive with files
  directly at its root, no wrapping folder, is left alone rather than
  having its only file's name mistaken for a directory to strip and
  silently dropped — a real bug this exact logic used to have, now
  covered by a regression test.)

- **Steps 8–9 are not atomic together.** The lockfile is written, then
  the manifest is updated, as two separate file writes. If the second
  write fails after the first succeeds, `smod.lock` and `smod.toml` can
  drift out of sync. This is a known, documented limitation — see
  [Known limitations](#known-limitations--things-a-contributor-should-know).

`install_all` wraps this differently from a single install: it reads
`smod.toml`'s dependency table and `smod.lock` once up front, and for each
declared dependency either records `DependencyOutcome::AlreadyInstalled`
(if it's already in the lockfile — no version-diff check yet, just
presence) or calls `install_one` and records `Installed` or `Failed`. A
single dependency failing doesn't stop the rest from being attempted —
only project-level problems (no `smod.toml` at all, an unreadable
`smod.lock`) fail the whole call. `commands::install` then turns
`Vec<DependencyOutcome>` into the `✓ installed` / `↷ already installed` /
`✗ failed` summary output, and exits non-zero if anything failed, so a
script checking `$?` can tell the difference between "everything's fine"
and "3 of 4 installed."

`remove_package` runs the mirror image without needing a registry at all:
look the package up in `smod.lock` (→ `RemoveError::NotInstalled` if
absent) → delete `smod_modules/<name>/` → drop the lockfile entry → drop
the manifest dependency entry.

---

## The registry abstraction

```rust
pub trait RegistryClient {
    async fn search(&self, query: &str) -> Result<Vec<PackageInfo>, RegistryError>;
    async fn get_package(&self, name: &str) -> Result<PackageInfo, RegistryError>;
    async fn list_packages(&self) -> Result<Vec<PackageInfo>, RegistryError>;
}
```

This is the *only* seam between the rest of `smod` and "where package
metadata comes from." `installer.rs`, `commands::search`, and
`commands::info` all depend on this trait, never on a concrete client
type directly (`Installer<'a, R: RegistryClient>` is generic for exactly
this reason).

Two implementation details worth understanding if you're touching this
file:

- **The trait uses native `async fn` in a trait**, stabilized in Rust
  1.75, rather than the `async-trait` crate. The trade-off: `RegistryClient`
  is **not object-safe** — you cannot have a `Box<dyn RegistryClient>` or
  a `Vec<Box<dyn RegistryClient>>` today. Every caller is generic over a
  concrete `R: RegistryClient` instead (monomorphized at compile time).
  This has been a non-issue so far because there's only ever one registry
  client alive at a time, chosen at compile time. If a future feature
  needs to pick a backend *at runtime* (e.g. a `--registry-backend`
  flag), that's the point to either wrap clients in a hand-written enum
  (`enum AnyRegistryClient { Mock(...), Http(...) }`, dispatching
  manually) or pull in `async-trait` to make the trait object-safe. Don't
  reach for either speculatively — cross that bridge when something
  actually needs it.

- **`MockRegistryClient` has three constructors**, and the choice of
  which one to use matters:
  - `from_json_str(json)` — parse an in-memory string. What tests use.
  - `from_file(path)` — read and parse a real path off disk via
    `tokio::fs`. Currently unused by any command (only exercised by
    tests) — it's there for the day a `--registry <path>` flag exists.
  - `embedded()` — parse a copy of `registry.json` baked into the binary
    at compile time via `include_str!("../registry.json")`. **This is
    what every real command uses today.** It exists so `smod search` /
    `smod info` / `smod install` work immediately regardless of the
    current working directory, the same way a real HTTP-backed client
    wouldn't depend on `cwd` either — deliberately not "whatever
    `registry.json` happens to be in the directory you're standing in."

`PackageInfo` (`name`, `version`, `description`, `author`, `program_id`,
`archive`, `checksum: Option<String>`, `dependencies: BTreeMap<String,
String>`) is the registry's view of a package — kept distinct from
`package::Manifest`, which is a *project's own* description of itself before
it's published. They will diverge more once `publish` exists (a manifest
doesn't need to carry a checksum of itself, for instance). `dependencies` is
registry metadata surfaced by `smod info`; it defaults to empty (older
registry documents deserialize unchanged) and does not yet drive transitive
installation.

---

## How HTTP support will fit in later (without touching commands)

This is the scenario the trait boundary was built for, so it's worth
spelling out concretely. Adding a real registry backend should look like:

1. **Add an `HttpRegistryClient` in `registry.rs`** (or a new
   `registry_http.rs` module, `pub use`d from `registry.rs`) implementing
   the same `RegistryClient` trait: `search`, `get_package`,
   `list_packages`, now making `reqwest` calls instead of reading
   `include_str!`. Add `reqwest`/`futures-util` to `Cargo.toml` at that
   point — they're deliberately not dependencies yet.

2. **`PackageInfo` likely doesn't need to change.** It's already
   transport-agnostic (a plain `Deserialize`/`Serialize` struct); an HTTP
   JSON response can deserialize straight into it, same as
   `registry.json` does today.

3. **The install pipeline changes in exactly one place:** step 3/4 in
   [the walkthrough above](#the-install-flow-walkthrough) —
   `resolve_archive_path` + `std::fs::read`. Once `PackageInfo::archive`
   is a URL instead of a relative path, that pair becomes "stream an HTTP
   GET into memory (or into a temp file, for large archives) instead of
   reading a local path." Everything from `compute_checksum` onward is
   already transport-agnostic — it just operates on `&[u8]`, and doesn't
   care whether those bytes came from disk or a socket. This is also
   where `ui::progress` (currently unused, already built and styled)
   becomes useful: wiring a real byte-progress bar to a streamed download
   body.

4. **`Installer<'a, R: RegistryClient>` doesn't change at all.** Whatever
   command constructs the client just does
   `Installer::new(&HttpRegistryClient::new(url), project_root)` instead
   of `Installer::new(&MockRegistryClient::embedded(), project_root)`.

5. **`commands/*.rs` change by exactly one line each** — swapping which
   concrete `RegistryClient` gets constructed at the top of `run()`.
   `install_single`, `install_all`, `Installer::install`,
   `Installer::install_all`, `commands::search::run`,
   `commands::info::run` — none of their internal logic changes, because
   none of them know or care which `RegistryClient` implementation they
   were handed.

The one design question left open for whoever does this: does `smod` talk
to *one* fixed registry URL, or does it need multiple registries
(`--registry`, private registries, etc.) at once? That decision affects
whether step 1 needs the object-safety workaround mentioned above. Until
that's decided, resist the urge to add it speculatively.

---

## Error handling strategy

Two error styles are used on purpose, at two different layers:

- **Business logic (`config.rs`, `lockfile.rs`, `registry.rs`,
  `installer.rs`) uses `thiserror` enums** (`ConfigError`, `LockfileError`,
  `RegistryError`, `InstallError`, `RemoveError`). Callers that need to
  branch on *why* something failed — tests, mostly, plus `installer.rs`
  itself distinguishing a missing file from a checksum mismatch — can
  `matches!(err, SomeError::SpecificVariant)` instead of parsing a
  message string. Each variant carries just enough context (`name`,
  `path`, `expected`/`actual`, `#[source]`) to render a useful message
  without the caller having to reconstruct it.

- **`commands/*.rs` uses `anyhow::Result<()>`.** Commands don't need to
  branch on error variants — they just need to `?`-propagate whatever
  went wrong up to `main.rs`, which renders it uniformly
  (`error: {chain}`, red, exit code 1). Every `thiserror` type above
  implements `std::error::Error`, so it converts into `anyhow::Error` via
  `?` for free; commands never construct their own error enums.

- **No `.unwrap()` in non-test code.** The two exceptions are
  `MockRegistryClient::embedded()`'s `.expect(...)` (justified: the
  embedded `registry.json` is a build-time asset under our own control,
  and every possible corruption of it is already covered by a test that
  would fail CI immediately) and one `unreachable!()` in `commands::list`
  guarded by an exhaustive match where the unreachable arm is genuinely
  provably unreachable.

---

## Testing strategy

Most tests live next to the code they test, in a `#[cfg(test)] mod tests`
block at the bottom of each file. These are the fast, precise unit tests that
assert on typed results rather than printed text. In addition, `tests/cli.rs`
holds end-to-end integration tests that spawn the compiled `smod` binary in a
temp directory and assert on its stdout/stderr and exit code — exercising the
full stack (arg parsing, dispatch, the embedded registry, the real
`packages/*.zip`, and on-disk `smod.toml`/`smod.lock`) the way a user would.
A few patterns worth following if you're adding more:

- **Business logic tests never touch `commands/`.** They construct a
  `MockRegistryClient::from_json_str(...)` pointed at a `tempfile`
  fixture archive, build an `Installer` directly, and assert on the
  returned `Result`/`Vec<DependencyOutcome>` and the resulting files on
  disk. This is what makes them fast (no process spawn, no real registry)
  and precise (they assert on typed error variants, not printed text).
- **Every `thiserror` variant that's reachable in practice has at least
  one test that reaches it** — missing package, missing archive, invalid
  zip, checksum mismatch, corrupt manifest, corrupt lockfile, "not a
  project," and so on.
- **Hand-rolled algorithms get edge-case tests.** `civil_from_days` is
  tested against the Unix epoch, a leap day, a non-leap year's February,
  the century-leap-year special case (1900 vs. 2000), a pre-epoch date,
  and a year boundary — because a date algorithm that's "probably right"
  is exactly the kind of thing that produces a wrong `installed_at`
  timestamp six months from now if it isn't pinned down now.
- **`package.rs`'s one doctest is marked `ignore`,** since `smod` is
  currently a bin-only crate with no lib target for a doctest to link
  against; the same coverage exists as a regular `#[test]` instead. If
  this crate ever grows a `lib.rs`, that doctest is a candidate to
  un-ignore.

---

## Adding a new command: a checklist

If you're implementing `doctor`, `update`, or `publish` for real (or
adding a new command entirely), this is the shape to follow:

1. Does the behavior need new business logic? Add it to `installer.rs`
   (package operations), `config.rs` (manifest operations), or
   `lockfile.rs` (lockfile operations) — as a plain function/method
   returning a `thiserror` error type, with its own tests. If the
   behavior doesn't fit any existing module (e.g. `doctor`'s environment
   diagnostics), consider whether it deserves its own module rather than
   being bolted onto an unrelated one.
2. Write the command's `*Args` struct and `run()` in
   `src/commands/<name>.rs`, calling into step 1's function(s) and
   printing colored, cargo/npm-style output. Use `ui::spinner`/
   `ui::progress` if the operation takes noticeable time.
3. If the command needs an existing project, resolve it with
   `config::require_project_root(std::env::current_dir()?)?` at the top
   — not a raw `is_smod_project` check against `cwd` directly, so the
   command works from subdirectories like the others do.
4. Register the module in `commands/mod.rs` (`pub mod <name>;`) and wire
   it into `Commands` in `cli.rs` and the `match` in `main.rs::dispatch`.
5. Make sure a genuinely-failed run exits non-zero (`bail!`/`?`, not a
   printed message followed by `Ok(())`) — this matters for anyone
   scripting against `smod`.

---

## Known limitations / things a contributor should know

These are documented rather than fixed because fixing them properly is
either a larger design decision or genuinely new scope, not a quick
patch:

- **`install`/`remove` aren't transactional across `smod.lock` and
  `smod.toml`.** Each is a separate file write; a failure between them
  (disk full, permissions changed mid-run) can leave the two out of sync.
  A real fix looks like write-both-or-neither (e.g. write to temp files,
  then rename both), not a quick patch.
- **`install_all` treats "present in `smod.lock`" as "installed," with no
  version-diff check.** Hand-editing `smod.toml` to bump a dependency's
  version without reinstalling won't trigger a reinstall — `install_all`
  will just report it as already installed. This is where `update`'s real
  implementation is expected to live.
- **`--verbose` and `--dev` are parsed but currently no-ops.** Wiring
  real behavior behind them is out of scope until a specific feature
  needs them (verbose logging; a `[smod.dev-dependencies]` table).
- **`RegistryClient` is not object-safe** (see
  [The registry abstraction](#the-registry-abstraction)) — a deliberate,
  revisit-when-needed trade-off, not an oversight.
