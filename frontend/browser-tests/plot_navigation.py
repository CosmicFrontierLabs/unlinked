#!/usr/bin/env python3
"""Check diagram-to-plot navigation with mocked results, no model mutations."""
import argparse
import copy
import json
import re
import uuid
from playwright.sync_api import sync_playwright, expect
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url',required=True)
parser.add_argument('--model',required=True)
parser.add_argument('--browser')
args=parser.parse_args()
with sync_playwright() as p:
    browser=p.chromium.launch(headless=True,**({'executable_path':args.browser} if args.browser else {}))
    page=browser.new_page(viewport={'width':1440,'height':1000})
    errors=[]
    page.on('pageerror',lambda e:errors.append(str(e)))
    page.goto(args.url+'/api/auth/dev-login')
    file_id=args.model.split('/files/')[1]
    history_url=args.url+'/api/files/'+file_id+'/simulations'
    template=page.request.get(history_url).json()[0]
    run=copy.deepcopy(template)
    run.update(id=str(uuid.uuid4()),status='completed',error=None)
    metadata=[{'id':'1','name':'model/output:1'},{'id':'2','name':'model/control:1'}]
    result={'run':run,'signals':metadata,'trace':{'time':[0,0.5,1],'signals':{'1':[0,1,2],'2':[0,2,4]},'solver':'rk4'}}
    page.route(history_url,lambda route:route.fulfill(content_type='application/json',body=json.dumps([run])))
    page.route('**/api/simulations/*',lambda route:route.fulfill(content_type='application/json',body=json.dumps(result)))
    page.goto(args.url+args.model)
    page.get_by_role('button',name='Simulate',exact=True).click()
    sim=page.get_by_role('region',name='Simulation',exact=True)
    sim.get_by_role('button',name=re.compile('^Load .*'+run['id'][:8])).click()
    expect(sim.get_by_role('status')).to_contain_text('3 samples')
    expect(sim.get_by_text('Signals · 2 selected',exact=True)).to_be_visible()
    query=sim.get_by_role('searchbox',name='Search signals')
    query.fill('no-such-signal')
    expect(sim.locator('.signal-options')).to_contain_text('No matching signals')
    page.get_by_role('button',name='Diagram',exact=True).click()
    wire=page.locator('.diagram polyline.wire-hit').first
    wire.dispatch_event('click')
    page.locator('.recorded').get_by_role('button',name='Plot',exact=True).first.click()
    expect(sim).to_be_visible()
    expect(query).to_have_value('')
    expect(sim.locator('.signal-options input')).to_have_count(2)
    # Bulk selection retains the user's ordering instead of sorting by ID.
    sim.get_by_role('button',name='Clear visible',exact=True).click()
    options=sim.locator('.signal-options label')
    options.filter(has_text='control').locator('input').check()
    options.filter(has_text='output').locator('input').check()
    sim.get_by_role('button',name='▸ Measurements',exact=True).click()
    rows=sim.locator('.measurement-table tbody th')
    expect(rows).to_have_text(['control:1','output:1'])
    sim.get_by_role('button',name='Select visible',exact=True).click()
    expect(rows).to_have_text(['control:1','output:1'])
    query.fill('control')
    sim.get_by_role('button',name='Clear visible',exact=True).click()
    expect(rows).to_have_text(['output:1'])
    query.fill('')
    sim.get_by_role('button',name='Select visible',exact=True).click()
    expect(rows).to_have_text(['output:1','control:1'])
    assert not errors,errors
    print('PASS: explicit diagram Plot reveals its signal despite a stale signal filter')
    browser.close()
