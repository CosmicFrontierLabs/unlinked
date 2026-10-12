#!/usr/bin/env python3
"""Keyboard file pickers and local replacement drops; never uploads files."""
import argparse
from pathlib import Path
from playwright.sync_api import sync_playwright, expect
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--url',required=True)
parser.add_argument('--model',required=True)
parser.add_argument('--browser')
parser.add_argument('--engine', choices=['chromium', 'firefox', 'webkit'], default='chromium')
args=parser.parse_args()
source=(Path(__file__).resolve().parents[2]/'examples/digital_pi_first_order.mdl').read_text()
with sync_playwright() as p:
    browser=getattr(p, args.engine).launch(headless=True,**({'executable_path':args.browser} if args.browser else {}))
    page=browser.new_page(viewport={'width':1440,'height':1000})
    errors=[];writes=[]
    page.on('pageerror',lambda error:errors.append(str(error)))
    page.on('request',lambda request:writes.append(request.url) if request.method in ['POST','PUT','PATCH','DELETE'] else None)
    page.request.get(args.url+'/api/auth/dev-login')
    page.goto(args.url+'/view')
    picker=page.get_by_label('Choose a model…',exact=True)
    picker.focus();expect(picker).to_be_focused()
    assert page.locator('.file-picker').evaluate('e=>getComputedStyle(e).outlineStyle')=='solid'
    with page.expect_file_chooser() as choice:
        picker.press('Enter')
    choice.value.set_files({'name':'local.mdl','mimeType':'text/plain','buffer':source.encode()})
    expect(page.locator('.diagram')).to_be_visible()
    expect(page.locator('.crumbs')).to_contain_text('digital_pi_first_order')
    picker=page.get_by_label('Choose a model…',exact=True)
    expect(picker).to_have_value('')
    with page.expect_file_chooser() as choice:
        picker.press('Space')
    choice.value.set_files({'name':'local.mdl','mimeType':'text/plain','buffer':source.replace('digital_pi_first_order','keyboard_reloaded').encode()})
    expect(page.locator('.crumbs')).to_contain_text('keyboard_reloaded')
    # File drops on the already loaded canvas replace the model locally.
    data=page.evaluate_handle("text=>{const d=new DataTransfer();d.items.add(new File([text],'drop.mdl',{type:'text/plain'}));return d;}",source.replace('digital_pi_first_order','dropped_model'))
    page.locator('.diagram').dispatch_event('dragover',{'dataTransfer':data})
    expect(page.locator('.local-viewer')).to_have_class('page-fill local-viewer over')
    page.locator('.diagram').dispatch_event('drop',{'dataTransfer':data})
    expect(page.locator('.crumbs')).to_contain_text('dropped_model')
    # An accidental non-model file must leave the current diagram intact.
    data=page.evaluate_handle("()=>{const d=new DataTransfer();d.items.add(new File(['image'],'photo.png',{type:'image/png'}));return d;}")
    page.locator('.diagram').dispatch_event('drop',{'dataTransfer':data})
    expect(page.get_by_role('alert')).to_contain_text('choose a .slx or .mdl model file')
    expect(page.locator('.crumbs')).to_contain_text('dropped_model')
    # A palette-style payload is not a file replacement.
    data=page.evaluate_handle("()=>{const d=new DataTransfer();d.setData('application/x-unlinked-block','Gain');return d;}")
    page.locator('.diagram').dispatch_event('dragover',{'dataTransfer':data})
    page.locator('.diagram').dispatch_event('drop',{'dataTransfer':data})
    expect(page.locator('.crumbs')).to_contain_text('dropped_model')
    expect(page.locator('.local-viewer')).to_have_class('page-fill local-viewer')
    # Upload pickers use the same native keyboard control; open, never select.
    page.goto(args.url+args.model)
    picker=page.get_by_label('Upload new version…',exact=True)
    picker.focus()
    with page.expect_file_chooser(): picker.press('Enter')
    page.goto(args.url+args.model.split('/files/')[0])
    picker=page.locator('input[type=file]')
    picker.focus()
    with page.expect_file_chooser(): picker.press('Space')
    assert not writes,writes
    assert not errors,errors
    print('PASS: three keyboard pickers, focus indicator, same-name local reload, canvas file replacement, non-file drop ignored; no uploads')
    browser.close()
