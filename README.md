# Unlinked

Unlinked is a Rust web application for opening Simulink diagrams, sharing versioned models within organizations, and running a growing subset of their simulations. It also includes a MATLAB/Octave-to-Rust compiler with LLVM IR output through `rustc`.

The application follows [single-binary-rust-website](https://github.com/meawoppl/single-binary-rust-website): Yew compiled to WebAssembly, Axum serving embedded frontend assets, shared Rust protocol types, PostgreSQL with Diesel migrations, and a single deployable backend binary. See [PLAN.md](PLAN.md) for the architecture and remaining milestones, and [the original template conventions](docs/TEMPLATE_CONVENTIONS.md) for background. That archived template describes the starting point; the code and instructions below describe Unlinked.

## Current capabilities

- Import ZIP/XML `.slx` and legacy textual `.mdl` models, retaining unknown blocks and parameters. Render root diagrams and nested subsystems as SVG; inspect blocks in the browser.
- Organization and project permissions, Google/GitHub OAuth, immutable file versions, and audit events. Viewers can read models and run simulations; editors can change files.
- Simulation jobs pinned to an immutable file version, with authenticated HTTP results and a typed WebSocket stream. Euler, RK4, and adaptive Dormand–Prince RK45 solvers are available.
- Simulation of the supported block subset, including basic sources, arithmetic, integrators, unit delays, switching, logic, and bounded transfer functions. Unsupported blocks and settings produce explicit errors.
- MATLAB/Octave compilation to standalone Rust or LLVM IR. See [compiler documentation](crates/unlinked-matlab/README.md) for the implemented language subset.

This is an independent implementation with partial compatibility. Rendering a model does not mean it can be simulated. Stateflow, arbitrary toolbox/library behavior, MATLAB callbacks, and general masked or conditional subsystem execution are not implemented. Imported workspace scripts are not executed. Supply parameter values explicitly. Solver settings must be selected explicitly when running from the CLI.

The separately licensed [test corpus](https://github.com/meawoppl/unlinked-test-cases) records upstream URLs, pinned revisions, licenses, checksums, and expected results where available. Corpus tests cover import and rendering; analytic and Octave differential tests cover numerical behavior. Passing import/render checks is not a numerical equivalence claim.

## Run locally

Install Rust, PostgreSQL development headers (`libpq-dev` on Debian/Ubuntu), and the WASM bundler:

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
cp .env.example .env
docker compose up -d db
(cd frontend && trunk build)
cargo run -p backend -- --dev-mode
```

Open `http://localhost:3000`. Development mode enables a local login and uses an ephemeral cookie key when `SESSION_SECRET` is unset. Sessions then end on restart. Use development mode only on a trusted local machine. The compose database port binds to localhost; its sample database password is for development.

Build the frontend before compiling the backend: `memory-serve` embeds `frontend/dist` at compile time. Rebuild both after changing browser code. Keep Trunk's hashed asset filenames enabled: HTML revalidates while hashed assets receive long-lived caching.

## Command line

```sh
cargo build -p unlinked-cli
cargo run -p unlinked-cli -- coverage /path/to/unlinked-test-cases -o coverage.json
cargo run -p unlinked-cli -- info model.slx
cargo run -p unlinked-cli -- render model.slx -o diagram.svg
cargo run -p unlinked-cli -- render model.slx --system Controller -o controller.svg
cargo run -p unlinked-cli -- sim model.mdl --stop 10 --step 0.01 --solver rk45 --var 'K=2*pi' -o trace.csv
cargo run -p unlinked-cli -- transpile example.m -o example.rs
cargo run -p unlinked-cli -- transpile example.m --emit llvm-ir -o example.ll
```

Repeat `--system` for each nested subsystem; each argument is an exact block name, including literal slashes. SVG output uses an opaque dark background. Simulation output supports JSON and CSV; RK45 exposes `--rtol`, `--atol`, and `--max-internal-steps`. Run a subcommand with `--help` for all options.

LLVM emission requires a local `rustc`. It compiles generated Rust without executing it. The web server does not spawn a compiler or execute uploaded scripts. CLI/model browser inputs are bounded at 16 MiB; server file storage has a separately configurable upload limit.

## Authentication and deployment

Set `DATABASE_URL`, `PUBLIC_URL` to the exact externally visible origin, and a persistent `SESSION_SECRET` containing at least 64 bytes. Generate a secret with `openssl rand -base64 64`; keep it outside source control. Production startup fails without a valid secret and at least one configured OAuth provider.

Configure all three variables for Google and/or GitHub as shown in [.env.example](.env.example). Register the corresponding `/api/auth/callback/google` or `/api/auth/callback/github` URL with the provider. `ALLOWED_EMAIL_DOMAINS` optionally restricts sign-in. Serve production traffic over HTTPS, including behind a reverse proxy; production session cookies are secure. OAuth state, flow expiry, session revocation, same-origin mutation checks, and organization isolation have automated tests. A real provider sign-in still needs validation with your registered credentials.

Migrations are embedded and run at startup. Back up PostgreSQL: it stores model bytes, immutable history, memberships, sessions, audit records, and simulation results. Simulation concurrency caps are per backend process (four jobs globally and two per user); multi-instance global scheduling is a remaining deployment milestone.

The Dockerfile packages a prebuilt binary, following the template's CI flow:

```sh
(cd frontend && trunk build --release)
cargo build --release -p backend --locked
mkdir -p build-output
cp target/release/backend build-output/backend
docker build -t unlinked .
```

Supply production configuration at runtime. The compose backend intentionally has no usable session-secret fallback. The bundled compose database credentials are a local example, not a production credential configuration. CI builds release artifacts and the container, and publishes main-branch images to GHCR.

## APIs and development

Shared DTOs live in `shared`; database models remain backend-only. Project file uploads create immutable versions. Simulation routes are:

- `POST /api/files/:file_id/simulations`: run a pinned version and return its persisted result.
- `GET /api/files/:file_id/simulations`: list recent runs.
- `GET /api/simulations/:run_id`: read an authorized result.
- `/ws/simulations`: stream `SimulationStarted`, sample-major `SimulationSamples`, and terminal `SimulationStatus` messages. Supports cancellation and checks session validity before each new run.

See [shared/src/simulation.rs](shared/src/simulation.rs) for request and stream types. Run records preserve the chosen settings and workspace overrides. Disconnecting a streaming client cancels its worker. A background task marks expired abandoned records failed. Each user is limited to 50 accepted runs per day; older results are pruned to retain approximately the latest 20 runs (plus any active jobs). Jobs, queues, output volume, input size, and solver work have explicit bounds.

Core crates separate model IR, import, rendering, MATLAB semantics, simulation, and CLI. Keep the computational crates WASM-compatible; native process invocation belongs in the CLI. Add migrations as `YYYY-MM-DD-HHMMSS_description` directories and run the naming check.

```sh
./scripts/check-migration-names.sh
cargo fmt --all --check
(cd frontend && trunk build)
cargo clippy --workspace --all-targets --locked -- -D warnings
UNLINKED_TEST_CASES=/path/to/unlinked-test-cases cargo test --workspace --locked
cargo clippy -p frontend --target wasm32-unknown-unknown --all-targets -- -D warnings
```

Set `DATABASE_URL` to a disposable PostgreSQL test database to run database-backed tests; without it those tests are skipped. Install Octave for differential compiler tests. Set `UNLINKED_TEST_CASES` to enable the external corpus checks. CI supplies PostgreSQL and checks out the corpus; native and WASM builds are both relevant.

## License

[Apache-2.0](LICENSE). Imported models retain their individual licenses in the test corpus. Simulink and MATLAB are MathWorks trademarks; this project is not affiliated with MathWorks.
