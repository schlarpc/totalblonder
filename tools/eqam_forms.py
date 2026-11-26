#!/usr/bin/env python3
"""Summarize forms/fields in saved EQAM HTML pages (offline; no network).
Usage: python3 -I tools/eqam_forms.py page.html [...]"""
import sys, re
from html.parser import HTMLParser

class P(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.forms = []; self.cur = None; self.sel = None
    def form(self):
        if self.cur is None:
            self.cur = {"attrs": {"(implicit)": ""}, "fields": []}; self.forms.append(self.cur)
        return self.cur
    def handle_starttag(self, tag, a):
        a = dict(a)
        if tag == "form":
            self.cur = {"attrs": a, "fields": []}; self.forms.append(self.cur)
        elif tag in ("input", "textarea", "button"):
            self.form()["fields"].append({"tag": tag, **a})
        elif tag == "select":
            self.sel = {"tag": "select", **a, "options": [], "selected": None}
            self.form()["fields"].append(self.sel)
        elif tag == "option" and self.sel is not None:
            self.sel["options"].append(a.get("value"))
            if "selected" in a: self.sel["selected"] = a.get("value")
    def handle_endtag(self, tag):
        if tag == "select": self.sel = None

def rng(opts):
    if not opts: return "[]"
    if all(o is not None and re.fullmatch(r"-?\d+", o) for o in opts):
        n = [int(o) for o in opts]
        if n == list(range(n[0], n[0] + len(n))): return f"[{n[0]}..{n[-1]}]"
    s = ",".join(str(o) for o in opts)
    return "[" + (s if len(s) < 200 else s[:200] + f"... ({len(opts)} opts)") + "]"

for fn in sys.argv[1:]:
    p = P(); p.feed(open(fn, encoding="latin-1").read())
    print(f"##### {fn}")
    for f in p.forms:
        print(" FORM", f["attrs"])
        for x in f["fields"]:
            if x["tag"] == "select":
                print(f"   select {x.get('name')} sel={x['selected']!r} opts={rng(x['options'])}"
                      + (f" onchange={x['onchange']!r}" if x.get('onchange') else ""))
            else:
                extra = {k: v for k, v in x.items() if k not in ("tag", "name", "type", "value", "style", "size")}
                print(f"   {x['tag']} {x.get('type','')} {x.get('name')} = {x.get('value')!r} {extra if extra else ''}")
