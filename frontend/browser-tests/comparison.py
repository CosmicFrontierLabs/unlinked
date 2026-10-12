#!/usr/bin/env python3
"""Check reference-run overlays with mocked historical traces, no model mutations."""
import argparse
import base64
from pathlib import Path
import copy
import csv
import io
import json
import re
import uuid
from playwright.sync_api import sync_playwright, expect
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url',required=True)
parser.add_argument('--model',required=True)
parser.add_argument('--browser')
parser.add_argument('--engine', choices=['chromium', 'firefox', 'webkit'], default='chromium')
args=parser.parse_args()
with sync_playwright() as p:
    browser=getattr(p, args.engine).launch(headless=True,**({'executable_path':args.browser} if args.browser else {}))
    page=browser.new_page(viewport={'width':1440,'height':1000})
    errors=[]
    page.on('pageerror',lambda e:errors.append(str(e)))
    page.request.get(args.url+'/api/auth/dev-login')
    file_id=args.model.split('/files/')[1]
    history_url=args.url+'/api/files/'+file_id+'/simulations'
    template=page.request.get(history_url).json()[0]
    runs=[]
    for i in range(4):
        run=copy.deepcopy(template)
        run.update(id=str(uuid.uuid4()),status='completed',file_version=template['file_version'] if i==0 else 1,error=None)
        runs.append(run)
    metadata=[{'id':'1','name':'model/output:1'},{'id':'2','name':'model/control:1'}]
    primary={'run':runs[0],'signals':metadata,'trace':{'time':[0,0.5,1],'signals':{'1':[0,1,2],'2':[0,2,4]},'solver':'rk4'}}
    reference={'run':runs[1],'signals':[{'id':'1','name':'old/renamed:1'},{'id':'3','name':'model/control:1'}],
               'trace':{'time':[0,0.3,0.9],'signals':{'1':[3,4,3],'3':[9,9,9]},'solver':'rk4'}}
    responses={runs[0]['id']:primary,runs[1]['id']:reference,runs[2]['id']:{'run':runs[2],'signals':[],'trace':None},runs[3]['id']:dict(reference,run=runs[3])}
    delayed=[]
    def result(route):
        id=route.request.url.rsplit('/',1)[1]
        if id==runs[3]['id']:
            delayed.append(route)
        else:
            route.fulfill(content_type='application/json',body=json.dumps(responses[id]))
    page.route(history_url,lambda route:route.fulfill(content_type='application/json',body=json.dumps(runs)))
    page.route('**/api/simulations/*',result)
    page.goto(args.url+args.model)
    page.get_by_role('button',name='Simulate',exact=True).click()
    sim=page.get_by_role('region',name='Simulation',exact=True)
    def load(i): return sim.get_by_role('button',name=re.compile('^Load .*'+runs[i]['id'][:8]))
    def compare(i): return sim.get_by_role('button',name=f"Compare with v{runs[i]['file_version']} run {runs[i]['id'][:8]}",exact=True)
    expect(sim.get_by_role('button',name='Export PNG',exact=True)).to_be_disabled()
    load(0).click()
    expect(sim.get_by_role('status')).to_contain_text('3 samples')
    expect(sim.get_by_text('Signals · 2 selected',exact=True)).to_be_visible()
    canvas=sim.locator('canvas')
    page.wait_for_function("document.querySelector('.sim-panel canvas').width > 1")
    before=canvas.evaluate('e=>e.toDataURL()')
    compare(1).click()
    banner=sim.get_by_role('region',name='Run comparison',exact=True)
    expect(banner).to_contain_text('1 of 2 selected signals matched')
    page.wait_for_function('(before)=>document.querySelector(".sim-panel canvas").toDataURL()!==before',arg=before)
    expect(banner).to_contain_text('Reference v1')
    with page.expect_download() as download:
        sim.get_by_role('button',name='Export CSV',exact=True).click()
    rows=list(csv.reader(open(download.value.path())))
    assert rows[0]==['time','model/output:1','model/control:1']
    assert [float(x) for x in rows[-1]]==[1,2,4],rows
    # Image export captures exactly this zoomed view, including the reference.
    canvas.hover();page.mouse.wheel(0,-240);page.wait_for_timeout(200)
    comparison_pixels=canvas.evaluate('e=>e.toDataURL()')
    with page.expect_download() as image:
        sim.get_by_role('button',name='Export PNG',exact=True).click()
    assert image.value.suggested_filename==f"plot-{runs[0]['id']}.png"
    assert Path(image.value.path()).read_bytes()==base64.b64decode(comparison_pixels.split(',',1)[1])
    assert canvas.evaluate('e=>e.toDataURL()')==comparison_pixels,'Export changed the view'
    canvas.evaluate("e=>{e.originalToDataURL=e.toDataURL;e.toDataURL=()=>{throw new Error('Test encoder failure')}}")
    sim.get_by_role('button',name='Export PNG',exact=True).click()
    expect(sim.get_by_role('alert')).to_have_text('Could not export plot: Test encoder failure')
    canvas.evaluate('e=>{e.toDataURL=e.originalToDataURL;delete e.originalToDataURL}')
    with page.expect_download():
        sim.get_by_role('button',name='Export PNG',exact=True).click()
    expect(sim.get_by_role('alert')).to_have_count(0)
    page.get_by_role('button',name='Diagram',exact=True).click()
    page.get_by_role('button',name='Simulate',exact=True).click()
    expect(banner).to_be_visible()
    assert comparison_pixels==canvas.evaluate('e=>e.toDataURL()')
    sim.locator('.signal-options').get_by_role('checkbox',name='output:1',exact=True).uncheck()
    expect(banner).to_contain_text('0 of 1 selected signals matched')
    sim.locator('.signal-options').get_by_role('checkbox',name='output:1',exact=True).check()
    sim.get_by_role('button',name='Remove comparison',exact=True).click()
    expect(banner).to_have_count(0)
    compare(2).click()
    expect(sim.get_by_role('alert')).to_contain_text('no recorded samples')
    expect(sim.get_by_role('status')).to_contain_text('3 samples')
    compare(3).click()
    expect(sim.get_by_text('Loading comparison…',exact=True)).to_be_visible()
    sim.get_by_role('button',name='Cancel comparison',exact=True).click()
    assert len(delayed)==1
    delayed[0].fulfill(content_type='application/json',body=json.dumps(responses[runs[3]['id']]))
    page.wait_for_timeout(200)
    expect(banner).to_have_count(0)
    compare(1).click();expect(banner).to_be_visible()
    page.screenshot(path='/tmp/unlinked-run-comparison.png')
    assert not errors,errors
    print('PASS: ID matching, independent reference trace, overlay changes, retained tabs, current-only CSV, selection, empty trace and canceled stale response')
    browser.close()
