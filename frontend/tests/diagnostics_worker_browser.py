"""Run after building diagnostics_worker_probe.html and serving its dist on :3140.
Uses Playwright; CHROME can override its installed Chromium executable.
"""
import json
import os
from playwright.sync_api import sync_playwright

URL = os.environ.get("WORKER_TEST_URL", "http://localhost:3140/")
MODEL = dict(name="Worker test", source="Mdl", simulink_version=None,
             config=dict(solver="ode4", start_time="0", stop_time="1", fixed_step="0.01", raw={}),
             root=dict(blocks=[], lines=[], annotations=[], properties={"InitFcn": "error('must never execute')"}),
             workspace={}, charts=[], type_defaults={})

def request(generation, compile=False):
    context = {"mode": "Compile" if compile else "Static"}
    if compile:
        context["options"] = {"start": 0., "stop": 1., "step": .01, "solver": "rk4"}
    return json.dumps(dict(generation=generation, model=MODEL, context=context))

with sync_playwright() as p:
    browser = p.chromium.launch(executable_path=os.environ.get("CHROME", "/usr/bin/google-chrome"), args=["--headless=new"])
    page = browser.new_page()
    page.add_init_script("""window.workerLog=[];window.workers=[];const NativeWorker=Worker;
        window.Worker=class extends NativeWorker {
            constructor(url, options){super(url,options);this.ready=false;this.dead=false;workers.push(this);
              this.addEventListener('message',e=>{try {if(JSON.parse(e.data).type==='ready')this.ready=true;}catch{}});}
            postMessage(message){workerLog.push({ready:this.ready,url:location.href});super.postMessage(message);}
            terminate(){this.dead=true;super.terminate();}
        };""")
    page.goto(URL)
    page.wait_for_function("typeof window.wasmBindings?.WorkerProbe === 'function'")
    page.evaluate("window.probe=new wasmBindings.WorkerProbe(15000)")
    for generation, compile in [(1, False), (2, True)]:
        page.evaluate("request=>probe.start(request)", request(generation, compile))
        page.wait_for_function("generation=>JSON.parse(probe.events()).some(e=>e.generation===generation)", arg=generation)
        outcome = page.evaluate("JSON.parse(probe.events()).at(-1)")
        assert "report" in outcome, outcome
        assert outcome["report"]["simulation"] == ("Compiled" if compile else "NotChecked"), outcome
        assert page.evaluate("workers.at(-1).dead")
    assert page.evaluate("workerLog.length===2 && workerLog.every(e=>e.ready)"), "Request posted before WASM ready"

    # Supersede a request immediately and ignore a queued old event, even when
    # the caller reuses the same public generation.
    page.evaluate("request=>{probe.start(request);window.old=workers.at(-1);probe.start(request);old.dispatchEvent(new MessageEvent('message',{data:JSON.stringify({type:'error',generation:3,message:'stale'})}));}", request(3))
    page.wait_for_function("JSON.parse(probe.events()).some(e=>e.generation===3)")
    events = page.evaluate("JSON.parse(probe.events()).filter(e=>e.generation===3)")
    assert len(events) == 1 and "report" in events[0], events
    assert page.evaluate("old.dead")

    count = page.evaluate("JSON.parse(probe.events()).length")
    page.evaluate("request=>{probe.start(request);probe.cancel();}", request(4))
    page.wait_for_timeout(100)
    assert page.evaluate("JSON.parse(probe.events()).length") == count
    assert page.evaluate("workers.at(-1).dead")
    page.evaluate("request=>{let dropping=new wasmBindings.WorkerProbe(15000);dropping.start(request);dropping.free();}", request(5))
    assert page.evaluate("workers.at(-1).dead")

    page.evaluate("window.slow=new wasmBindings.WorkerProbe(1)")
    page.evaluate("request=>slow.start(request)", request(6))
    page.wait_for_function("JSON.parse(slow.events()).length===1")
    assert "timed out" in page.evaluate("JSON.parse(slow.events())[0].error")
    page.evaluate("slow.free()")
    assert page.evaluate("workers.at(-1).dead")

    # A missing loader must produce an error (or the bounded timeout), never
    # leave the caller waiting forever or silently run checks on the UI thread.
    page.route("**/unlinked-diagnostics-worker_loader.js", lambda route: route.fulfill(status=404, content_type="text/plain", body="not found"))
    page.evaluate("request=>probe.start(request)", request(7))
    page.wait_for_function("JSON.parse(probe.events()).some(e=>e.generation===7)")
    error = page.evaluate("JSON.parse(probe.events()).at(-1)")
    assert "error" in error, error
    assert page.evaluate("workers.at(-1).dead")
    page.unroute("**/unlinked-diagnostics-worker_loader.js")

    # Exercise the worker's protocol error path using the actual generated loader.
    malformed = page.evaluate("""() => new Promise((resolve,reject)=>{
       const worker=new Worker('/unlinked-diagnostics-worker_loader.js');
       const timeout=setTimeout(()=>{worker.terminate();reject(Error('protocol timeout'));},15000);
       worker.onmessage=e=>{const value=JSON.parse(e.data);if(value.type==='ready')worker.postMessage('{');
         else {clearTimeout(timeout);worker.terminate();resolve(value);}};
    })""")
    assert malformed["type"] == "error" and "Invalid diagnostics request" in malformed["message"], malformed
    # A client-side nested file route still resolves the worker at Trunk's base.
    page.evaluate("history.pushState({}, '', '/projects/probe/files/probe')")
    page.evaluate("request=>probe.start(request)", request(8))
    page.wait_for_function("JSON.parse(probe.events()).some(e=>e.generation===8)")
    assert "report" in page.evaluate("JSON.parse(probe.events()).at(-1)")
    page.evaluate("probe.free()")
    browser.close()
print("PASS: generated worker ready/request/static/compile/error/supersede/cancel/drop/timeout/bootstrap failure")
