#!/usr/bin/env python3
"""Check mobile access to signals/history; reads a stored run without mutations."""
import argparse
from playwright.sync_api import sync_playwright, expect
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url',required=True)
parser.add_argument('--model',required=True)
parser.add_argument('--browser')
parser.add_argument('--engine', choices=['chromium', 'firefox', 'webkit'], default='chromium')
args=parser.parse_args()
with sync_playwright() as p:
    browser=getattr(p, args.engine).launch(headless=True,**({'executable_path':args.browser} if args.browser else {}))
    page=browser.new_page(viewport={'width':390,'height':844})
    errors=[]
    page.on('pageerror',lambda error:errors.append(str(error)))
    page.request.get(args.url+'/api/auth/dev-login')
    page.goto(args.url+args.model)
    page.get_by_role('button',name='Simulate',exact=True).click()
    sim=page.get_by_role('region',name='Simulation',exact=True)
    toggle=sim.locator('.mobile-signals-toggle')
    panel=sim.locator('.signal-list')
    expect(toggle).to_be_visible()
    expect(toggle).to_have_attribute('aria-expanded','false')
    expect(panel).to_be_hidden()
    box=toggle.bounding_box()
    assert box['y']+box['height']<844,'History access should precede the plot'
    toggle.focus();toggle.press('Enter')
    expect(toggle).to_have_attribute('aria-expanded','true')
    expect(panel).to_be_visible()
    assert toggle.get_attribute('aria-controls')==panel.get_attribute('id')
    toggle.press('Tab')
    # Firefox includes scroll containers in sequential keyboard navigation.
    for _ in range(2):
        if page.evaluate("document.activeElement.matches('.sim-body, .signal-list')"):
            page.keyboard.press('Tab')
    expect(sim.get_by_role('searchbox',name='Search signals')).to_be_focused()
    panel.locator('.run-history-item').first.click()
    expect(sim.locator('.sim-status')).to_contain_text('samples')
    expect(panel.locator('.signal-options input').first).to_be_visible()
    toggle.click()
    expect(panel).to_be_hidden()
    expect(sim.locator('canvas')).to_be_visible()
    page.set_viewport_size({'width':1440,'height':1000})
    expect(toggle).to_be_hidden()
    expect(panel).to_be_visible()
    assert panel.bounding_box()['x']>sim.locator('.results-main').bounding_box()['x']
    page.set_viewport_size({'width':390,'height':844})
    expect(panel).to_be_hidden()
    toggle.click()
    page.get_by_role('button',name='Diagram',exact=True).click()
    page.get_by_role('button',name='Simulate',exact=True).click()
    expect(panel).to_be_visible()
    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth'), 'No horizontal page overflow'
    assert not errors,errors
    print('PASS: mobile signals/history access, keyboard order, recorded run, responsive layout and retained disclosure')
    browser.close()
