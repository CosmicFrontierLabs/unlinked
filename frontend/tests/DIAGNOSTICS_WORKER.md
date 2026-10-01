The production entrypoint is `unlinked-diagnostics-worker`; Trunk builds its
classic-worker loader, JS wrapper, and WASM alongside the application.
`DiagnosticsWorker` waits for the Rust entrypoint's Ready message before posting
its one request. Keep the handle in a component ref, cancel on snapshot/context
changes, and drop on unmount. `start` cancels any previous request even if encoding
the new snapshot fails. Explicit cancellation never invokes the result callback.
All completion/error/timeout paths terminate the worker; the result carries the
caller generation, with an additional private token guarding reused generations.

`start` returns serialization/creation errors synchronously. Its callback receives
worker/bootstrap/protocol errors and timeouts asynchronously. The default 15-second
timeout includes bootstrap and compilation. This runs diagnostics, never a simulation
or initialization callback. Compile mode requires explicit `DiagnosticContext` options.

Worker filenames are deliberately stable in Trunk 0.21. The backend applies
`Cache-Control: no-cache` to the loader, JS, and WASM (including conditional 304
responses); ETags and all other asset cache policies remain intact.

Browser integration test (Playwright and Chromium required):

```sh
NO_COLOR=true trunk build --config frontend/Trunk.toml \
  tests/diagnostics_worker_probe.html --dist /tmp/diagnostics-worker-probe
python3 -m http.server 3140 --directory /tmp/diagnostics-worker-probe
# In another terminal:
python3 frontend/tests/diagnostics_worker_browser.py
```

The probe binary is gated by the `worker-probe` feature and is never included in
the normal HTML/build. Set `CHROME` to the browser executable and `WORKER_TEST_URL`
to another static server if needed. Tests cover actual generated assets, handshake,
static/compile reports, same-generation supersession, cancellation/drop, timeout,
missing loader, malformed request, and loading from a nested application route.
