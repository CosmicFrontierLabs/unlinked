#!/usr/bin/env python3
"""Exercise retained workspaces against a development server with a stored model.

Install Python Playwright, then run:
    python workspace.py --url http://localhost:3121 --model /projects/.../files/...

Uses dev login and changes an unsaved solver setting. Never saves model edits.
The selected model should have a completed simulation in its run history.
"""
import argparse
import copy
import json
import uuid
from playwright.sync_api import sync_playwright, expect

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url', required=True)
parser.add_argument('--model', required=True)
parser.add_argument('--browser', help='Optional browser executable path')
parser.add_argument('--engine', choices=['chromium', 'firefox', 'webkit'], default='chromium')
args = parser.parse_args()

with sync_playwright() as p:
    launch = {'headless': True}
    if args.browser:
        launch['executable_path'] = args.browser
    browser = getattr(p, args.engine).launch(**launch)
    page = browser.new_page(viewport={'width': 1440, 'height': 1000})
    errors = []
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.request.get(args.url + '/api/auth/dev-login')
    page.goto(args.url + '/')
    expect(page.get_by_role('button', name='Sign out', exact=True)).to_be_visible()
    # Create an in-app history entry so Back can exercise the leave guard.
    page.evaluate("path => { history.pushState(null, '', path); window.dispatchEvent(new PopStateEvent('popstate')); }", args.model)
    diagram = page.get_by_role('region', name='Diagram', exact=True)
    simulation = page.get_by_role('region', name='Simulation', exact=True)
    expect(diagram.locator('.diagram svg').first).to_be_visible()
    expect(page.locator('#file-history')).to_be_hidden()
    page.get_by_role('button', name='History', exact=True).click()
    expect(page.locator('#file-history')).to_be_visible()
    page.get_by_role('button', name='Hide history', exact=True).click()
    expect(page.locator('#file-history')).to_be_hidden()

    # Pin the actual DOM nodes: switching tabs should hide, not replace them.
    page.evaluate('window.workspaceDiagram = document.querySelector(".diagram")')
    if diagram.get_by_role('button', name='Edit', exact=True).count():
        diagram.get_by_role('button', name='Edit', exact=True).click()
    diagram.get_by_role('button', name='Model settings' if diagram.get_by_role('button', name='Model settings', exact=True).count() else 'Settings', exact=True).click()
    stop = diagram.get_by_label('Stop time', exact=True)
    before = stop.input_value()
    stop.fill('17')
    stop.blur()
    expect(stop).to_have_value('17')
    # Mock metadata mutations: exercise refreshes without changing the server.
    resource = args.url + '/api' + args.model
    original_info = page.request.get(resource).json()
    original_versions = page.request.get(resource + '/versions').json()
    metadata = copy.deepcopy(original_info)
    content_requests = []
    page.on('request', lambda request: content_requests.append(request.url) if '/content' in request.url else None)
    def file_metadata(route):
        if route.request.method == 'PATCH':
            metadata['path'] = route.request.post_data_json['path']
            route.fulfill(status=204)
        else:
            route.fulfill(status=200, content_type='application/json', body=json.dumps(metadata))
    page.route(resource, file_metadata)
    page.once('dialog', lambda dialog: dialog.accept('workspace-review.mdl'))
    page.get_by_role('button', name='Rename', exact=True).click()
    expect(page.locator('.file-breadcrumbs strong')).to_have_text('workspace-review.mdl')
    expect(stop).to_have_value('17')
    assert page.evaluate('window.workspaceDiagram === document.querySelector(".diagram")')
    metadata['latest']['id'] = str(uuid.uuid4())
    metadata['latest']['version'] += 1
    page.route(resource + '/versions', lambda route: route.fulfill(status=200, content_type='application/json', body=json.dumps([metadata['latest'], *original_versions])))
    page.route('**/api/projects/*/files?*', lambda route: route.fulfill(status=200, content_type='application/json', body=json.dumps(metadata)))
    page.locator('.file-bar input[type=file]').set_input_files({'name':'review.mdl', 'mimeType':'application/octet-stream', 'buffer':b'Model {}'})
    expect(page.get_by_role('button', name='Open newer version')).to_be_visible()
    expect(stop).to_have_value('17')
    assert not content_requests, content_requests
    assert page.evaluate('window.workspaceDiagram === document.querySelector(".diagram")')
    # Declining navigation must retain the same editor and draft.
    prompts = []
    def reject_exit(dialog):
        prompts.append(dialog.message)
        dialog.dismiss()
    page.on('dialog', reject_exit)
    page.get_by_role('navigation', name='File breadcrumb').get_by_role('link', name='Projects', exact=True).click()
    expect(stop).to_have_value('17')
    assert len(prompts) == 1 and 'Unsaved' in prompts[-1]
    page.get_by_role('button', name='Open newer version').click()
    expect(stop).to_have_value('17')
    assert len(prompts) == 2
    page.evaluate('history.back()')
    page.wait_for_timeout(300)
    expect(stop).to_have_value('17')
    assert len(prompts) == 3 and 'Unsaved' in prompts[-1]
    assert page.url == args.url + args.model
    page.remove_listener('dialog', reject_exit)
    page.get_by_role('button', name='Simulate', exact=True).click()
    expect(diagram).to_be_hidden()
    expect(simulation).to_be_visible()
    expect(simulation.locator('.workspace-notice')).to_contain_text('unsaved changes')
    page.locator('body').click(position={'x': 8, 'y': 8})
    page.keyboard.press('Control+z')
    page.get_by_role('button', name='Diagram', exact=True).click()
    expect(stop).to_have_value('17')
    assert page.evaluate('window.workspaceDiagram === document.querySelector(".diagram")')

    # The editor's shortcut works again once visible, and reports clean state.
    page.locator('body').click(position={'x': 8, 'y': 8})
    page.keyboard.press('Control+z')
    expect(stop).to_have_value(before)
    page.get_by_role('button', name='Simulate', exact=True).click()
    expect(simulation.locator('.workspace-notice')).to_have_count(0)
    simulation.locator('.runs button').first.click()
    expect(simulation.locator('.sim-actions')).to_contain_text('Completed')
    expect(simulation.locator('.signal-list input[type=checkbox]').first).to_be_visible()
    selected = simulation.locator('.signal-list input[type=checkbox]:checked').count()
    page.evaluate('window.workspaceCanvas = document.querySelector(".plot-host canvas")')
    canvas = simulation.locator('.plot-host canvas')
    canvas.hover()
    page.mouse.wheel(0, -300)
    page.wait_for_timeout(150)
    plot_before = canvas.screenshot()
    page.get_by_role('button', name='Diagram', exact=True).click()
    page.get_by_role('button', name='Simulate', exact=True).click()
    expect(simulation.locator('.sim-actions')).to_contain_text('Completed')
    assert simulation.locator('.signal-list input[type=checkbox]:checked').count() == selected
    assert page.evaluate('window.workspaceCanvas === document.querySelector(".plot-host canvas")')
    page.wait_for_timeout(150)
    assert canvas.screenshot() == plot_before, 'Plot appearance/zoom changed after tab switch'
    original_width = canvas.evaluate('(c) => c.width')
    page.set_viewport_size({'width': 1100, 'height': 1000})
    page.wait_for_function('(w) => document.querySelector(".plot-host canvas").width !== w', arg=original_width)
    page.set_viewport_size({'width': 1440, 'height': 1000})
    page.wait_for_function('(w) => document.querySelector(".plot-host canvas").width === w', arg=original_width)
    page.wait_for_timeout(150)
    assert canvas.screenshot() == plot_before, 'Plot zoom changed after resize'
    simulation.get_by_role('button', name='Fit plot', exact=True).click()
    page.wait_for_timeout(150)
    assert canvas.screenshot() != plot_before, 'Fit plot did not restore full trace'
    # Text uploads also keep body, badge, and download pinned until opened.
    metadata.clear()
    metadata.update(copy.deepcopy(original_info))
    metadata['path'] = 'review.m'
    old_id = metadata['latest']['id']
    old_number = metadata['latest']['version']
    page.route(resource + '/versions/*/content', lambda route: route.fulfill(
        status=200, content_type='text/plain',
        body='a = 1;' if old_id in route.request.url else 'a = 2;'))
    page.reload()
    expect(page.locator('pre.source')).to_have_text('a = 1;')
    metadata['latest']['id'] = str(uuid.uuid4())
    metadata['latest']['version'] = old_number + 1
    page.locator('.file-bar input[type=file]').set_input_files(
        {'name': 'review.m', 'mimeType': 'text/plain', 'buffer': b'a = 2;'})
    expect(page.get_by_role('button', name='Open newer version')).to_be_visible()
    expect(page.locator('pre.source')).to_have_text('a = 1;')
    expect(page.locator('.version-badge')).to_have_text(f'v{old_number}')
    expect(page.get_by_role('link', name='Download', exact=True)).to_have_attribute(
        'href', f'/api{args.model}/versions/{old_id}/content')
    page.get_by_role('button', name='Open newer version').click()
    expect(page.locator('pre.source')).to_have_text('a = 2;')
    expect(page.locator('.version-badge')).to_have_text(f'v{old_number + 1}')
    expect(page.get_by_role('link', name='Download', exact=True)).to_have_attribute(
        'href', f"/api{args.model}/versions/{metadata['latest']['id']}/content")
    assert not errors, errors
    print('PASS: history visibility, edits retained, hidden shortcuts disabled, visible undo restored, results retained; no browser errors')
    browser.close()
