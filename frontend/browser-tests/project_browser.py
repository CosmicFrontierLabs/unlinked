#!/usr/bin/env python3
"""Check project file navigation with mocked file metadata; no server mutations."""
import argparse
import copy
import json
import uuid
from playwright.sync_api import sync_playwright, expect

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url', required=True)
parser.add_argument('--project', required=True, help='/projects/<id>')
parser.add_argument('--browser')
args = parser.parse_args()

with sync_playwright() as p:
    browser = p.chromium.launch(headless=True, **({'executable_path': args.browser} if args.browser else {}))
    page = browser.new_page(viewport={'width': 1280, 'height': 1000})
    errors = []
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.goto(args.url + '/api/auth/dev-login')
    resource = args.url + '/api' + args.project + '/files'
    template = page.request.get(resource).json()[0]
    files = []
    for i, path in enumerate(['plant/gain.SLX', 'plant/inner/step.mdl', 'plants/other.mdl', 'scripts/setup.m'] + [f'model{i:03}.mdl' for i in range(105)]):
        item = copy.deepcopy(template)
        item.update(id=str(uuid.uuid4()), path=path)
        item['latest'].update(id=str(uuid.uuid4()), file_id=item['id'], size_bytes=(i + 1) * 100, version=3)
        files.append(item)
    page.route(resource, lambda route: route.fulfill(content_type='application/json', body=json.dumps(files)))
    page.goto(args.url + args.project)
    region = page.get_by_role('region', name='Project files', exact=True)
    rows = region.locator('tbody tr')
    expect(rows).to_have_count(100)
    expect(region.get_by_text('Page 1 of 2', exact=True)).to_be_visible()
    region.get_by_role('button', name='Next', exact=True).click()
    expect(rows).to_have_count(8)
    region.get_by_role('button', name='Previous', exact=True).click()
    region.get_by_role('button', name='plant', exact=True).click()
    expect(rows).to_have_count(2)
    assert page.url.endswith('?folder=plant')
    upload_folder = page.get_by_placeholder('Upload folder (optional)')
    upload_folder.fill('new/destination')
    upload_folder.blur()
    expect(rows).to_have_count(2)
    expect(region.get_by_role('link', name='gain.SLX', exact=True)).to_be_visible()
    expect(region.get_by_role('link', name='other.mdl', exact=True)).to_have_count(0)
    region.get_by_role('button', name='inner', exact=True).click()
    expect(region.get_by_role('link', name='step.mdl', exact=True)).to_be_visible()
    expect(upload_folder).to_have_value('plant/inner')
    page.reload()
    expect(region.get_by_role('link', name='step.mdl', exact=True)).to_be_visible()
    page.go_back()
    expect(region.get_by_role('link', name='gain.SLX', exact=True)).to_be_visible()
    page.go_forward()
    expect(region.get_by_role('link', name='step.mdl', exact=True)).to_be_visible()
    region.get_by_role('button', name='plant', exact=True).click()
    region.get_by_role('searchbox', name='Search project files').fill('SETUP')
    expect(rows).to_have_count(1)
    expect(region.get_by_role('link', name='scripts/setup.m', exact=True)).to_be_visible()
    region.get_by_label('File type', exact=True).select_option('model')
    expect(region.get_by_text('No matching files', exact=True)).to_be_visible()
    region.get_by_role('searchbox').fill('')
    expect(rows).to_have_count(2)
    region.get_by_role('button', name='Project files', exact=True).click()
    region.get_by_label('Sort files').select_option('size')
    expect(region.locator('tbody tr:not(.folder-row)').first).to_contain_text('model104.mdl')
    region.get_by_role('searchbox').fill('gain')
    link = region.get_by_role('link', name='Download plant/gain.SLX', exact=True)
    expect(link).to_have_attribute('href', f"/api{args.project}/files/{files[0]['id']}/versions/{files[0]['latest']['id']}/content")
    dialogs = []
    def dismiss(dialog):
        dialogs.append(dialog.message)
        dialog.dismiss()
    page.on('dialog', dismiss)
    region.get_by_role('button', name='Delete plant/gain.SLX', exact=True).click()
    assert dialogs == ['Delete plant/gain.SLX from this project?']
    expect(rows).to_have_count(1)
    page.set_viewport_size({'width': 390, 'height': 844})
    expect(region.get_by_role('searchbox')).to_be_visible()
    assert page.evaluate('document.documentElement.scrollWidth <= window.innerWidth'), 'Page overflows mobile viewport'
    page.screenshot(path='/tmp/unlinked-project-browser-mobile.png', full_page=True)
    page.set_viewport_size({'width': 1280, 'height': 1000})
    region.get_by_role('searchbox').fill('')
    page.screenshot(path='/tmp/unlinked-project-browser-desktop.png', full_page=True)
    assert not errors, errors
    print('PASS: pagination, folder boundaries, breadcrumbs, global search, type/sort, pinned download, delete cancellation, mobile layout')
    browser.close()
