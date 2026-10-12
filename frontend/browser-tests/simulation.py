#!/usr/bin/env python3
"""Validate simulation controls against a dev instance; websocket runs are mocked.
No simulation jobs are created and no models are changed.
"""
import argparse
import json
import uuid
from playwright.sync_api import sync_playwright, expect

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url', required=True)
parser.add_argument('--model', required=True)
parser.add_argument('--browser')
args = parser.parse_args()

with sync_playwright() as p:
    browser = p.chromium.launch(headless=True, **({'executable_path': args.browser} if args.browser else {}))
    page = browser.new_page(viewport={'width': 1440, 'height': 1000})
    errors, requests, cancelled = [], [], []
    sockets = []
    file_id = args.model.split('/files/')[1].split('/')[0]
    run_id = str(uuid.uuid4())
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.add_init_script("window.closedSimulationSockets = 0; const WS = window.WebSocket; window.WebSocket = class extends WS { constructor(...args) { super(...args); this.addEventListener('close', () => window.closedSimulationSockets++); } };")
    def socket_handler(socket):
        sockets.append(socket)
        def receive(message):
            message = json.loads(message)
            if message['type'] == 'Simulate':
                requests.append(message['request'])
                if len(requests) == 1:
                    socket.send(json.dumps({'type':'SimulationStarted', 'run_id':run_id, 'signals':[
                        {'id':'1','name':'model/output:1'}, {'id':'2','name':'model/control:1'}]}))
                    socket.send(json.dumps({'type':'SimulationSamples', 'run_id':run_id, 'time':[0,0.5,1], 'values':[[0,0],[0.5,1],[1,2]]}))
            elif message['type'] == 'CancelSimulation':
                cancelled.append(message['run_id'])
                if len(requests) == 3:
                    return
                socket.send(json.dumps({'type':'SimulationStatus', 'run':{
                    'id':run_id, 'file_id':file_id, 'file_version_id':str(uuid.uuid4()),
                    'file_version':requests[-1]['version'], 'requested_by':None,
                    'status':'cancelled', 'request':requests[-1], 'error':None,
                    'created_at':'2026-10-12T00:00:00Z', 'finished_at':'2026-10-12T00:00:01Z'}}))
        socket.on_message(receive)
    page.route_web_socket('**/ws/simulations', socket_handler)
    page.goto(args.url + '/api/auth/dev-login')
    page.goto(args.url + args.model)
    page.get_by_role('button', name='Simulate', exact=True).click()
    sim = page.get_by_role('region', name='Simulation', exact=True)
    run = sim.get_by_role('button', name='▶ Run', exact=True)
    stop_time = sim.get_by_label('Stop time (s)', exact=True)
    step = sim.get_by_label('Output step (s)', exact=True)
    original_step = step.input_value()
    stop_time.fill('')
    expect(run).to_be_disabled()
    expect(stop_time).to_have_value('')
    expect(stop_time).to_have_attribute('aria-invalid', 'true')
    stop_time.fill('1e')
    expect(run).to_be_disabled()
    expect(stop_time).to_have_value('1e')
    stop_time.fill('1e1')
    expect(run).to_be_enabled()
    step.fill('0')
    expect(run).to_be_disabled()
    step.fill('0.0000001')
    expect(run).to_be_disabled()
    expect(sim.locator('.field-error')).to_contain_text('sample limit')
    step.fill(original_step)
    expect(run).to_be_enabled()
    run.click()
    expect(sim.get_by_role('status')).to_contain_text('Running')
    expect(sim.get_by_role('progressbar')).to_have_attribute('value', '10')
    expect(stop_time).to_be_disabled()
    assert requests[0]['options']['stop'] == 10
    assert not page.evaluate("window.dispatchEvent(new Event('beforeunload', {cancelable:true}))")
    page.get_by_role('button', name='Diagram', exact=True).click()
    page.wait_for_timeout(150)
    assert not cancelled and page.evaluate('window.closedSimulationSockets') == 0, 'Tab switch canceled the simulation'
    page.get_by_role('button', name='Simulate', exact=True).click()
    expect(sim.get_by_role('status')).to_contain_text('Running')
    sim.get_by_role('button', name='■ Stop', exact=True).click()
    expect(sim.get_by_role('status')).to_contain_text('Stopped · 3 samples')
    assert cancelled == [run_id]
    expect(sim.get_by_role('link', name='Export CSV')).to_be_visible()
    assert page.evaluate("window.dispatchEvent(new Event('beforeunload', {cancelable:true}))")
    page.wait_for_function('window.closedSimulationSockets >= 1')
    closed_before = page.evaluate('window.closedSimulationSockets')
    # The second mock connection never sends a run ID. Stop must still work.
    run.click()
    expect(sim.get_by_role('status')).to_contain_text('Connecting')
    sim.get_by_role('button', name='■ Stop', exact=True).click()
    expect(sim.get_by_role('status')).to_contain_text('Stopped · 0 samples')
    expect(run).to_be_enabled()
    page.wait_for_function('window.closedSimulationSockets > ' + str(closed_before))
    # A Running status can precede Started. Stop must remain sticky through both.
    run.click()
    expect(sim.get_by_role('status')).to_contain_text('Connecting')
    while len(requests) < 3:
        page.wait_for_timeout(20)
    delayed = {'type':'SimulationStatus', 'run':{
        'id':run_id, 'file_id':file_id, 'file_version_id':str(uuid.uuid4()),
        'file_version':requests[-1]['version'], 'requested_by':None,
        'status':'running', 'request':requests[-1], 'error':None,
        'created_at':'2026-10-12T00:00:00Z', 'finished_at':None}}
    sockets[-1].send(json.dumps(delayed))
    expect(sim.get_by_role('status')).to_contain_text('Running')
    sim.get_by_role('button', name='■ Stop', exact=True).click()
    expect(sim.get_by_role('status')).to_contain_text('Stopping')
    sockets[-1].send(json.dumps({'type':'SimulationStarted', 'run_id':run_id, 'signals':[]}))
    sockets[-1].send(json.dumps(delayed))
    page.wait_for_timeout(100)
    expect(sim.get_by_role('status')).to_contain_text('Stopping')
    expect(sim.get_by_role('button', name='■ Stop', exact=True)).to_be_disabled()
    delayed['run']['status'] = 'cancelled'
    sockets[-1].send(json.dumps(delayed))
    expect(sim.get_by_role('status')).to_contain_text('Stopped')
    expect(run).to_be_enabled()
    assert not errors, errors
    page.screenshot(path='/tmp/unlinked-simulation-controls.png')
    print('PASS: numeric drafts, validation/budget, progress, locked settings, retained active stream, Stop with/without run ID; no jobs created or browser errors')
    browser.close()
