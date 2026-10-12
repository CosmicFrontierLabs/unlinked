#!/usr/bin/env python3
"""Check committed member-role controls with mocked writes; no roles are changed."""
import argparse
import copy
import json
import uuid
from playwright.sync_api import sync_playwright, expect
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url',required=True)
parser.add_argument('--model',required=True)
parser.add_argument('--browser')
parser.add_argument('--engine',choices=['chromium','firefox','webkit'],default='chromium')
args=parser.parse_args()
with sync_playwright() as p:
    browser=getattr(p,args.engine).launch(headless=True,**({'executable_path':args.browser} if args.browser else {}))
    page=browser.new_page(viewport={'width':1440,'height':1000})
    errors=[]
    page.on('pageerror',lambda error:errors.append(str(error)))
    page.request.get(args.url+'/api/auth/dev-login')
    project_path=args.model.split('/files/')[0]
    project=page.request.get(args.url+'/api'+project_path).json()
    org_path='/orgs/'+project['org_id']
    template=page.request.get(args.url+'/api'+org_path+'/members').json()[0]
    user=copy.deepcopy(template['user'])
    user.update(id=str(uuid.uuid4()),email='role-review@example.test')
    for path,label,original,changed in [(org_path,'Organization','member','admin'),(project_path,'Project','viewer','editor')]:
        endpoint=args.url+'/api'+path+'/members'
        member={'user':user,'role':original,'joined_at':template['joined_at'],'added_at':template['joined_at']}
        page.route(endpoint,lambda route:route.fulfill(content_type='application/json',body=json.dumps([member])))
        requests=[]
        page.route(endpoint+'/'+user['id'],lambda route:requests.append(route))
        page.goto(args.url+path)
        select=page.get_by_label(label+' role for '+user['email'],exact=True)
        expect(select).to_have_value(original)
        select.select_option(changed)
        expect(select).to_have_value(original)
        page.wait_for_timeout(100)
        assert len(requests)==1 and requests[0].request.method=='PATCH'
        assert requests[0].request.post_data_json=={'role':changed}
        requests.pop().fulfill(status=503,content_type='application/json',body='{"error":"Please retry the role update"}')
        expect(page.get_by_role('alert')).to_contain_text('Please retry')
        expect(select).to_have_value(original)
        select.select_option(changed)
        page.wait_for_timeout(100)
        assert len(requests)==1
        member['role']=changed
        requests.pop().fulfill(status=204)
        expect(select).to_have_value(changed)
        expect(page.get_by_role('alert')).to_have_count(0)
        # A subsequent failure also retains the newly committed role.
        select.select_option(original)
        page.wait_for_timeout(100)
        assert len(requests)==1
        requests.pop().fulfill(status=403,content_type='application/json',body='{"error":"Role update refused"}')
        expect(page.get_by_role('alert')).to_contain_text('refused')
        expect(select).to_have_value(changed)
        page.unroute(endpoint)
        page.unroute(endpoint+'/'+user['id'])
    assert not errors,errors
    print('PASS: organization/project roles stay committed during pending/failed writes and reflect successful updates; writes mocked')
    browser.close()
