#!/usr/bin/env python3
"""Check responsive navigation and guarded sign-out without logging out on server."""
import argparse
import re
from playwright.sync_api import sync_playwright, expect

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url', required=True)
parser.add_argument('--model', required=True)
parser.add_argument('--browser')
parser.add_argument('--engine', choices=['chromium', 'firefox', 'webkit'], default='chromium')
args = parser.parse_args()
with sync_playwright() as p:
    browser = getattr(p, args.engine).launch(headless=True, **({'executable_path': args.browser} if args.browser else {}))
    page = browser.new_page(viewport={'width': 1280, 'height': 1000})
    errors = []
    page.on('pageerror', lambda e: errors.append(str(e)))
    page.request.get(args.url + '/api/auth/dev-login')
    page.goto(args.url + args.model)
    diagram = page.locator('.diagram')
    expect(diagram).to_be_visible()
    page.locator('.diagram g.block').first.click()
    diagram.press('ArrowRight')
    expect(page.get_by_role('button', name=re.compile(r'^Save v'))).to_be_enabled()
    calls = []
    success = False
    def logout(route):
        calls.append(route.request.method)
        if success:
            route.fulfill(status=204)
        else:
            route.fulfill(status=503, content_type='application/json', body='{"error":"Please retry"}')
    page.route('**/api/auth/logout', logout)
    accepted = False
    confirmations = []
    def dialog(d):
        confirmations.append(d.type)
        d.accept() if accepted else d.dismiss()
    page.on('dialog', dialog)
    skip = page.get_by_role('link', name='Skip to content', exact=True)
    skip.focus()
    skip.press('Enter')
    expect(page.locator('main')).to_be_focused()
    assert page.url == args.url + args.model and not confirmations
    expect(page.get_by_role('navigation', name='Main navigation').get_by_role('link', name='Projects', exact=True)).to_have_attribute('aria-current', 'page')
    page.get_by_role('button', name='Sign out', exact=True).click()
    assert confirmations == ['confirm'] and not calls
    expect(diagram).to_be_visible()
    accepted = True
    page.get_by_role('button', name='Sign out', exact=True).click()
    expect(page.get_by_role('alert')).to_contain_text('Please retry')
    assert calls == ['POST']
    assert page.evaluate("() => !window.dispatchEvent(new Event('beforeunload', {cancelable:true}))"), 'Failed logout must retain leave guard'
    page.set_viewport_size({'width': 390, 'height': 844})
    nav = page.get_by_role('navigation', name='Main navigation')
    expect(nav.get_by_role('link', name='Projects', exact=True)).to_be_visible()
    expect(nav.get_by_role('link', name='MATLAB to Rust', exact=True)).to_be_visible()
    assert page.evaluate('document.documentElement.scrollWidth <= innerWidth')
    header = page.locator('.topbar').bounding_box()
    main = page.locator('main').bounding_box()
    assert main['y'] >= header['y'] + header['height'] - 1
    page.screenshot(path='/tmp/unlinked-navigation-mobile.png')
    success = True
    page.get_by_role('button', name='Sign out', exact=True).click()
    page.wait_for_url(args.url + '/')
    assert calls == ['POST', 'POST']
    assert confirmations == ['confirm', 'confirm', 'confirm'], confirmations
    assert not errors, errors
    print('PASS: mobile navigation, cancel before logout, failed logout retains guard, confirmed logout leaves once')
    browser.close()
