#!/usr/bin/env python3
"""Inspect and reuse recorded settings without submitting a job."""
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
    run=copy.deepcopy(page.request.get(history_url).json()[0])
    run.update(id=str(uuid.uuid4()),status='completed',file_version=1,error=None)
    run['request']['options'].update(start=2,stop=8,step=0.25,solver='rk45',relative_tolerance=0.002,absolute_tolerance=1e-8)
    run['request']['workspace']={'K':'[1 2; 3 4]','tau':'1e-6'}
    run['request']['inputs']={'deleted-port':'123'}
    script_id=str(uuid.uuid4())
    run['request']['init_script']={'file_id':script_id,'version':2}
    result={'run':run,'signals':[{'id':'1','name':'output:1'}],'trace':{'time':[2,3,4],'signals':{'1':[1,2,3]},'solver':'rk45'}}
    page.route(history_url,lambda route:route.fulfill(content_type='application/json',body=json.dumps([run])))
    page.route('**/api/simulations/*',lambda route:route.fulfill(content_type='application/json',body=json.dumps(result)))
    sockets=[]
    page.route_web_socket('**/ws/simulations',lambda socket:sockets.append(socket))
    page.goto(args.url+args.model)
    page.get_by_role('button',name='Simulate',exact=True).click()
    sim=page.get_by_role('region',name='Simulation',exact=True)
    stop=sim.get_by_label('Stop time (s)',exact=True)
    stop.fill('12')
    sim.get_by_role('button',name=re.compile('^Load .*'+run['id'][:8])).click()
    expect(sim.locator('.sim-status')).to_contain_text('3 samples')
    expect(stop).to_have_value('12')
    sim.get_by_text('Recorded setup',exact=True).click()
    details=sim.locator('.recorded-setup')
    expect(details).to_contain_text('2.0 to 8.0')
    expect(details).to_contain_text('K = [1 2; 3 4]')
    expect(details).to_contain_text('deleted-port')
    details.get_by_role('button',name='Use recorded setup').click()
    expect(stop).to_have_value('8')
    expect(sim.get_by_label('Output step (s)',exact=True)).to_have_value('0.25')
    sim.get_by_text('Solver settings & workspace',exact=True).click()
    expect(sim.get_by_label('Start time (s)',exact=True)).to_have_value('2')
    expect(sim.get_by_label('Relative tolerance',exact=True)).to_have_value('0.002')
    expect(sim.locator('textarea')).to_have_value('K = [1 2; 3 4]\ntau = 1e-6')
    expect(sim.locator('select option:checked').filter(has_text='recorded v2')).to_have_count(1)
    expect(sim.locator('.setup-notice')).to_contain_text('Review it, then Run')
    assert not sockets,'Copying settings must not submit a simulation'
    # API-created multiline workspaces must not be silently rewritten.
    result['run']['request']['workspace']={'x':'[1\n2]'}
    sim.get_by_role('button',name=re.compile('^Load .*'+run['id'][:8])).click()
    expect(sim.locator('.sim-status')).to_contain_text('3 samples')
    stop.fill('17')
    sim.get_by_text('Recorded setup',exact=True).click()
    details.get_by_role('button',name='Use recorded setup').click()
    expect(sim.locator('.setup-notice')).to_contain_text('cannot represent exactly')
    expect(stop).to_have_value('17')
    page.screenshot(path='/tmp/unlinked-run-setup.png')
    assert not errors,errors
    print('PASS: recorded setup inspection, explicit reuse, pinned script, exact workspace, no auto-run, unsupported expressions preserve form')
    browser.close()
