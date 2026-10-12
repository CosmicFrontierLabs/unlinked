#!/usr/bin/env python3
"""Check history/comparison drawers and focus without changing model contents."""
import argparse
from playwright.sync_api import sync_playwright, expect
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url',required=True)
parser.add_argument('--model',required=True)
parser.add_argument('--browser')
parser.add_argument('--engine',choices=['chromium','firefox','webkit'],default='chromium')
args=parser.parse_args()
with sync_playwright() as p:
    browser=getattr(p,args.engine).launch(headless=True,**({'executable_path':args.browser} if args.browser else {}))
    page=browser.new_page(viewport={'width':390,'height':844})
    errors=[]
    page.on('pageerror',lambda error:errors.append(str(error)))
    page.request.get(args.url+'/api/auth/dev-login')
    page.goto(args.url+args.model)
    expect(page.locator('.diagram')).to_be_visible()
    history=page.locator('#file-history')
    for width in [390,730]:
        page.set_viewport_size({'width':width,'height':844})
        before=page.locator('.file-main').bounding_box()['width']
        page.get_by_role('button',name='History',exact=True).click()
        expect(history).to_be_visible()
        assert page.locator('.file-main').bounding_box()['width']==before
        assert history.bounding_box()['x']>=0
        history.get_by_role('link').first.focus()
        page.keyboard.press('Escape')
        expect(history).to_be_hidden()
        expect(page.get_by_role('button',name='History',exact=True)).to_be_focused()
    page.get_by_role('button',name='History',exact=True).click()
    compare=history.locator('a.compare').first
    compare_url=compare.get_attribute('href')
    compare.click()
    expect(page.locator('.diagram')).to_be_visible()
    changes=page.locator('#comparison-changes')
    expect(changes).to_be_hidden()
    before=page.locator('.file-main').bounding_box()['width']
    page.get_by_role('button',name='Changes',exact=True).click()
    expect(changes).to_be_visible()
    assert page.locator('.file-main').bounding_box()['width']==before
    close=page.get_by_role('button',name='Close model changes',exact=True)
    close.focus();close.press('Escape')
    expect(changes).to_be_hidden()
    expect(page.get_by_role('button',name='Changes',exact=True)).to_be_focused()
    page.get_by_role('button',name='Changes',exact=True).click()
    close.click()
    expect(changes).to_be_hidden()
    expect(page.get_by_role('button',name='Changes',exact=True)).to_be_focused()
    assert page.evaluate('document.documentElement.scrollWidth<=innerWidth')
    # On a fresh desktop comparison the changes start open beside the canvas.
    page.set_viewport_size({'width':1440,'height':1000})
    page.goto(args.url+compare_url)
    expect(changes).to_be_visible()
    main=page.locator('.file-main')
    expect(page.locator('.diagram')).to_be_visible()
    assert main.bounding_box()['width']<1440
    assert main.bounding_box()['x']+main.bounding_box()['width']<=changes.bounding_box()['x']+1
    page.get_by_role('button',name='Hide changes',exact=True).click()
    expect(changes).to_be_hidden()
    assert main.bounding_box()['width']==1440
    assert not errors,errors
    print('PASS: narrow history/comparison preserve canvas width, close/Escape restore focus, desktop comparison stays side by side')
    browser.close()
