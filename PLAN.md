# Unlinked — Plan

Unlinked is a web-based, pure-Rust clone of Simulink: open existing `.slx` /
`.mdl` models in the browser, render them faithfully, and (eventually) simulate
them. A second deliverable is a MATLAB/Octave → Rust (and later LLVM)
transpiler, which also doubles as the expression evaluator Simulink block
parameters need.

The backbone is [single-binary-rust-website](https://github.com/meawoppl/single-binary-rust-website):
Axum backend + Yew/Trunk frontend embedded with `memory-serve`, Diesel/Postgres
with embedded migrations, typed WebSockets via `ws-bridge`, one Docker image.

Owners: **claude** (web app, auth/orgs, import, render, frontend) and
**codex** (test corpus, simulation, MATLAB). Every change lands as a PR on a
`meawoppl/<topic>` branch and is reviewed by the other agent.

## Workspace layout

```
Cargo.toml                 # workspace: backend, frontend, shared, crates/*
shared/                    # HTTP/WS API types (serde), shared by backend + frontend
backend/                   # Axum server, OAuth, orgs, file storage, sim jobs
frontend/                  # Yew SPA: file browser, diagram viewer/editor, scopes
crates/
  unlinked-model/          # Model IR (serde only, wasm-safe)            [claude]
  unlinked-import/         # .slx (OPC zip + XML) and .mdl (text) → IR  [claude]
  unlinked-render/         # IR → SVG scene (pure, wasm-safe, testable) [claude]
  unlinked-cli/            # `unlinked` binary: info/render/sim/transpile [both]
  unlinked-matlab/         # MATLAB lexer/parser/AST, evaluator, Rust codegen [codex]
  unlinked-sim/            # block library, scheduler, solvers          [codex]
```

Rules:

- `unlinked-model`, `unlinked-import`, `unlinked-render`, `unlinked-matlab` and
  `unlinked-sim` must build for `wasm32-unknown-unknown` so the browser can
  parse, render and run small simulations locally. Anything native-only (LLVM
  via `inkwell`, threads, filesystem) sits behind a cargo feature or a separate
  crate.
- The backend is the source of truth for stored files; the frontend receives the
  parsed IR as JSON (`shared` types wrap `unlinked_model::Model`).
- Crate dependencies flow one way:
  `model ← import ← render`, `model ← sim ← matlab (evaluator)`,
  `backend/frontend/cli ← all`.

## Test corpus — `../unlinked-test-cases`

A separate repo ([meawoppl/unlinked-test-cases](https://github.com/meawoppl/unlinked-test-cases))
holds openly licensed third-party models, so licensing stays clean and the main
repo stays small.

```
fixtures/<source>/...        # .slx / .mdl / .m copied byte-for-byte from upstream
fixtures/synthetic/...       # hand-written models with analytic expected outputs
licenses/                    # upstream license texts
manifest.json                # per fixture: source repo + pinned revision, license,
                             # sha256, Simulink release, block types, coverage
                             # (import/render/sim/transpile), expected outputs
scripts/corpus.py            # verify hashes/licenses; refresh pinned upstreams
```

The main repo reads the corpus from `UNLINKED_TEST_CASES` (default
`../unlinked-test-cases`). Corpus tests skip (not fail) when the directory is
absent so a plain `cargo test` works anywhere; CI checks the corpus out
alongside and runs them for real.

## Model IR (`unlinked-model`)

Simulink concepts mapped 1:1 so render and sim share a single representation:

- `Model { name, source, simulink_version, config: SimConfig, root: System, workspace }`
- `System { blocks, lines, annotations }` — one diagram level
- `Block { id, block_type, name, position: Rect, orientation, mirrored, ports: PortCounts,
  parameters: BTreeMap<String,String>, mask, library_source, subsystem: Option<Box<System>>, style }`
- `Line { name, src, points, dst, branches }` with recursive `Branch`es
- `Endpoint { block: BlockId, port: PortRef { kind, index } }`

Parameters stay as raw MATLAB expression strings in the IR; evaluation is the
simulator's job (via `unlinked-matlab`'s evaluator against the model workspace
and mask scopes). `System::connections()` flattens line trees to
`(src, dst)` pairs for the simulator.

## Import (`unlinked-import`)

- **SLX** (R2012a+): OPC zip. `simulink/blockdiagram.xml` holds the model and,
  in newer releases, `simulink/systems/system_<sid>.xml` holds each subsystem.
  Parse with `zip` + `quick-xml`; resolve `<System Ref=...>` across parts.
  Stateflow lives in `simulink/stateflow.xml` (parsed later).
- **MDL**: nested `Key { ... }` text format with quoted strings and implicit
  string concatenation. Hand-written tokenizer → generic tree → IR. Old files
  lack `SID`; synthesize ids from the block path.
- Library links (`Reference` blocks with `SourceBlock`) render using a built-in
  appearance table for common libraries; unknown ones show as masked boxes.
- Unknown elements are preserved in `parameters` so nothing is silently lost.

## Rendering (`unlinked-render` + frontend)

`unlinked-render` turns a `System` into a backend-agnostic scene (shapes,
text, polylines, ports) and serializes to SVG. The same code produces:

- server-side thumbnails and `unlinked render` CLI output,
- golden-file snapshot tests over the corpus,
- the frontend diagram (Yew renders the scene as inline SVG with pan/zoom,
  click-to-open subsystems, breadcrumbs, hover tooltips for parameters).

Block glyphs cover the common library (Gain triangle, Sum circle, Integrator
`1/s`, Transfer Fcn fraction, Scope, In/Outport ovals, Mux/Demux bars,
Constant, Product, Saturation, Switch, From/Goto tags, SubSystem, Stateflow
chart box). Everything else renders as a labeled box with port stubs. Masked
blocks render their mask display text when simple (`disp`, `fprintf` of a
literal), otherwise a box.

## Simulation (`unlinked-sim`) — codex

1. Flatten the hierarchy (virtual subsystems, Goto/From, Mux/Demux bus
   expansion) into a dataflow graph.
2. Evaluate block parameters with `unlinked-matlab`.
3. Sort execution (direct feedthrough), detect algebraic loops.
4. Sample-time propagation: continuous, discrete, inherited, multirate.
5. Solvers: fixed-step ode1/ode2/ode3/ode4/ode5, variable-step ode23/ode45
   (Dormand–Prince) with zero-crossing detection.
6. Block library behind a `Block` trait (outputs / update / derivatives).
7. Logging: Scope / To Workspace / Outport signals → typed traces.

The backend runs simulations as jobs and streams traces to the browser over
the WebSocket; small models may also run in-browser via wasm.

## MATLAB/Octave transpiler (`unlinked-matlab`) — codex

- Lexer handling transpose-vs-quote, command syntax, `...` continuations,
  `%{ %}` block comments, `end` in indexing.
- Parser → AST (scripts, functions, nested/local functions, classdef later).
- Evaluator (tree-walking interpreter) over a `Value` type (double matrices,
  logical, char, cell, struct, function handles) — used for block parameters
  and as the transpiler's reference oracle.
- Type/shape inference → Rust code generation against a small runtime crate;
  LLVM backend (`inkwell`) behind a feature once the Rust path is stable.
- Tests compare evaluator/transpiled output against Octave-generated
  expectations stored in the corpus.

## Web application

### Auth

OAuth2 login (Google + GitHub, pattern from agent-portal) using the `oauth2`
crate; signed session cookie via `tower-cookies`. `--dev-mode` bypasses OAuth
with a local dev user. Allowed email domains are configurable
(`ALLOWED_EMAIL_DOMAINS`) so a deployment can be locked to one organization.

### Organizations and sharing

- `users` — identity from OAuth (provider, subject, email, name, avatar).
- `organizations`, `org_members (role: owner | admin | member)`.
- `projects` — owned by an org; `project_members` for per-project roles
  (`viewer | editor | owner`); org members inherit a default role.
- `files` — a path within a project (models, `.m` scripts, data).
- `file_versions` — immutable content blobs (bytea, sha256, size, author,
  message); the IR is re-derived on demand and cached.
- `sim_runs` — requested simulations with status and stored traces.
- `audit_log` — who did what, for organizations that care.

Every API handler authorizes through one `Access` extractor that resolves the
caller's role on the target project.

### Frontend

Routes: `/` (projects), `/p/:project` (file browser + upload),
`/p/:project/f/*path` (diagram viewer; later editor + scopes),
`/orgs/:org` (members), `/login`.

### Conventions (inherited from the template)

- All API/WS types in `shared`, with a serde roundtrip test per type.
- Migrations embedded and applied at startup; names checked by
  `scripts/check-migration-names.sh`.
- `build_app(state)` tested in-process with `tower::ServiceExt::oneshot`; DB
  tests run against a real Postgres when `DATABASE_URL` is set.
- CI: lint, audit, fmt, clippy (`-Dwarnings`), test, release build, container.
- Squash-merge PRs with automerge once checks pass.

## Milestones

| # | Milestone | Owner |
|---|-----------|-------|
| M0 | Template bootstrap, workspace layout, PLAN.md | claude |
| M1 | Corpus repo with ≥30 permissively licensed models + manifest | codex |
| M2 | IR + SLX/MDL import; every corpus model parses | claude |
| M3 | SVG renderer + golden tests; `unlinked render` CLI | claude |
| M4 | OAuth, orgs, projects, file upload/versioning, diagram viewer | claude |
| M5 | MATLAB lexer/parser/evaluator; block parameter evaluation | codex |
| M6 | Simulation engine: core blocks + fixed/variable-step solvers | codex |
| M7 | Sim runs from the UI with scope plots streamed over WS | both |
| M8 | MATLAB → Rust transpiler; LLVM backend | codex |
| M9 | Diagram editing (move/connect/add blocks, save back to SLX) | claude |
