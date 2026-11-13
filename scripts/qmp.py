#!/usr/bin/env python3
# A small QMP client for the UTM VMs of the tests: it types, presses keys,
# clicks, and takes screenshots.
#
#     scripts/qmp.py PORT type TEXT...
#     scripts/qmp.py PORT key ctrl-alt-delete
#     scripts/qmp.py PORT click X Y WIDTH HEIGHT
#     scripts/qmp.py PORT shot NAME          (to $QMP_SHOTS/NAME.png, or ./NAME.png)
#     scripts/qmp.py PORT raw COMMAND [JSON]
#
# The VM needs QMP on 127.0.0.1:PORT. UTM writes a screendump into its group
# container, and sips converts it to PNG.
import json, socket, sys, time, os
port = int(sys.argv[1]); op = sys.argv[2]; args = sys.argv[3:]
s = socket.create_connection(("127.0.0.1", port)); f = s.makefile("rw")
f.readline()
def q(execute, **a):
    f.write(json.dumps({"execute": execute, "arguments": a} if a else {"execute": execute}) + "\n"); f.flush()
    while True:
        r = json.loads(f.readline())
        if "event" not in r: return r
q("qmp_capabilities")
SCR = os.environ.get("QMP_SHOTS", ".")
G = os.path.expanduser("~/Library/Group Containers/WDNLXAD4W8.com.utmapp.UTM")
SH = {'!':'1','@':'2','#':'3','$':'4','%':'5','^':'6','&':'7','*':'8','(':'9',')':'0','_':'minus','+':'equal',
      '{':'bracket_left','}':'bracket_right','|':'backslash',':':'semicolon','"':'apostrophe','<':'comma','>':'dot','?':'slash','~':'grave_accent'}
PL = {' ':'spc','-':'minus','=':'equal','[':'bracket_left',']':'bracket_right','\\':'backslash',';':'semicolon',
      "'":'apostrophe',',':'comma','.':'dot','/':'slash','`':'grave_accent','\n':'ret'}
def key(keys):
    q("send-key", keys=[{"type":"qcode","data":k} for k in keys], **{"hold-time":40}); time.sleep(0.12)
def ch(c):
    if c.isalpha(): key((['shift'] if c.isupper() else []) + [c.lower()])
    elif c.isdigit(): key([c])
    elif c in SH: key(['shift', SH[c]])
    elif c in PL: key([PL[c]])
    else: raise SystemExit(f"no key for {c!r}")
if op == "type":
    for c in " ".join(args): ch(c)
elif op == "key":
    key(args[0].split("-"))
elif op == "shot":
    print(q("screendump", filename=f"{G}/{args[0]}.ppm"))
    os.system(f"sips -s format png \"{G}/{args[0]}.ppm\" --out \"{SCR}/{args[0]}.png\" >/dev/null")
elif op == "click":
    x, y, w, h = map(float, args[:4])
    ev = lambda: [{"type":"abs","data":{"axis":"x","value":int(x*32767/w)}},{"type":"abs","data":{"axis":"y","value":int(y*32767/h)}}]
    q("input-send-event", events=ev()); time.sleep(0.05)
    q("input-send-event", events=[{"type":"btn","data":{"down":True,"button":"left"}}]); time.sleep(0.05)
    q("input-send-event", events=[{"type":"btn","data":{"down":False,"button":"left"}}])
elif op == "raw":
    a = json.loads(args[1]) if len(args) > 1 else {}
    print(q(args[0], **a))
