#!/usr/bin/env python3
"""Exercise retained workspaces against a development server with a stored model.

Install Python Playwright, then run:
    python workspace.py --url http://localhost:3121 --model /projects/.../files/...

Uses dev login and changes an unsaved solver setting. Never saves model edits.
The selected model should have a completed simulation in its run history.
"""
import argparse
from playwright.sync_api import sync_playwright, expect

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url', required=True)
parser.add_argument('--model', required=True)
parser.add_argument('--browser', help='Optional Chromium executable path')
args = parser.parse_args()

with sync_playwright() as p:
    launch = {'headless': True}
    if args.browser:
        launch['executable_path'] = args.browser
    browser = p.chromium.launch(**launch)
    page = browser.new_page(viewport={'width': 1440, 'height': 1000})
    errors = []
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.goto(args.url + '/api/auth/dev-login')
    page.goto(args.url + args.model)
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
    diagram.get_by_role('button', name='Model settings', exact=True).click()
    stop = diagram.get_by_label('Stop time', exact=True)
    before = stop.input_value()
    stop.fill('17')
    stop.blur()
    expect(stop).to_have_value('17')
    page.get_by_role('button', name='Simulate', exact=True).click()
    expect(diagram).to_be_hidden()
    expect(simulation).to_be_visible()
    expect(simulation.get_by_role('status')).to_contain_text('unsaved changes')
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
    simulation.locator('.runs a').first.click()
    expect(simulation.locator('.sim-actions')).to_contain_text('Completed')
    expect(simulation.locator('.signal-list input[type=checkbox]').first).to_be_visible()
    selected = simulation.locator('.signal-list input[type=checkbox]:checked').count()
    page.evaluate('window.workspaceCanvas = document.querySelector(".plot-host canvas")')
    page.get_by_role('button', name='Diagram', exact=True).click()
    page.get_by_role('button', name='Simulate', exact=True).click()
    expect(simulation.locator('.sim-actions')).to_contain_text('Completed')
    assert simulation.locator('.signal-list input[type=checkbox]:checked').count() == selected
    assert page.evaluate('window.workspaceCanvas === document.querySelector(".plot-host canvas")')
    assert not errors, errors
    print('PASS: history visibility, edits retained, hidden shortcuts disabled, visible undo restored, results retained; no browser errors')
    browser.close()
