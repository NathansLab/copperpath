#!/usr/bin/env python3
"""Compare two kicad-cli DRC reports: violation counts per type and violations only in the new one.

usage: python3 scripts/drc_compare.py base_drc.json new_drc.json

Reports come from `kicad-cli pcb drc --format json --severity-all -o X.json B.kicad_pcb`.
Plain Python 3, no extra packages. `placeroute route --drc` does the same comparison itself.
"""
import collections
import json
import sys

if len(sys.argv) != 3:
    sys.exit(__doc__)
def load(p):
    d=json.load(open(p)); out=collections.Counter(); items={}
    for v in d.get('violations',[]):
        key=(v['type'],tuple(sorted(i.get('description','') for i in v['items'])))
        out[v['type']]+=1; items[key]=v
    return out,items,len(d.get('unconnected_items',[]))
a,ia,ua=load(sys.argv[1]); b,ib,ub=load(sys.argv[2])
print('type'.ljust(28),'base','new')
for t in sorted(set(a)|set(b)): print(t.ljust(28),a[t],b[t])
print('unconnected(capped)',ua,ub)
new=[k for k in ib if k not in ia]
print('NEW violations:',len(new))
for k in new[:40]: print(' ',k[0],'|',' / '.join(k[1])[:200])
