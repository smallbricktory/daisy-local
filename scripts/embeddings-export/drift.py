#!/usr/bin/env python3
"""drift.py <runs_dir> <reference> <name>... — same-chunk cosine and leave-one-out
nearest-neighbour agreement of each <name>.index.json against <reference>.index.json."""
import json, sys
def load(p):
    d=json.load(open(p)); return [v for s in d['sessions'] for v in s['vecs']]
dot=lambda a,c: sum(x*y for x,y in zip(a,c))
def nn(m): return [max((dot(v,w),j) for j,w in enumerate(m) if j!=i)[1] for i,v in enumerate(m)]
runs, ref = sys.argv[1], sys.argv[2]
f=load(f'{runs}/{ref}.index.json'); b=nn(f)
for n in sys.argv[3:]:
    q=load(f'{runs}/{n}.index.json'); c=sorted(dot(x,y) for x,y in zip(q,f)); a=nn(q)
    print(f"  {n:18s} vs {ref}: cos mean {sum(c)/len(c):.4f} p10 {c[len(c)//10]:.4f} min {c[0]:.4f}; LOO NN top1 agree {100*sum(x==y for x,y in zip(a,b))/len(a):.0f}%")
