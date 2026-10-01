"""Turn the real session's event log into a render timeline.

Session time `s` is seconds since the footage's first frame (x11grab start).
Output time maps intro -> warped session -> outro. Pointer glides (each
pointer_move waits for the software-rendered frame) are sped up so the cursor
moves at a natural pace; everything else plays in real time.
"""
import json, re, sys

take = sys.argv[1]
E = json.load(open(f"{take}/events.json"))
start = float(re.search(r"start: ([0-9.]+)", open(f"{take}/ffmpeg.log").read()).group(1))
FPS = 30
n_frames = int(sys.argv[2])
footage_len = n_frames / FPS

for e in E:
    e["s"] = e["t"] - start
    if "t_end" in e:
        e["s_end"] = e["t_end"] - start

# glide intervals: runs of pointer events
glides = []
ptr = [e for e in E if e["type"] == "pointer"]
run = []
for e in ptr:
    if run and e["s"] - run[-1]["s"] > 1.2:
        glides.append((run[0]["s"] - 0.05, run[-1]["s"] + 0.05)); run = []
    run.append(e)
if run:
    glides.append((run[0]["s"] - 0.05, run[-1]["s"] + 0.05))
# typing burst
typing = [e for e in E if e["type"] == "typing"]
tcall = [e for e in E if e.get("tool") == "type_text"]
fast = [(a, b, 2.6) for a, b in glides]
if typing and tcall:
    fast.append((typing[0]["s"], tcall[0]["s_end"], 1.6))

s_begin = max(0.0, [e for e in E if e["type"] == "start"][0]["s"] - 0.4)
end_ev = [e for e in E if e["type"] == "end"][0]
s_end = min(footage_len - 0.05, end_ev["s"] + 0.2)


def speed(s):
    for a, b, k in fast:
        if a <= s <= b:
            return k
    return 1.0


# integrate output frames
INTRO, OUTRO = 7.0, 10.5
session_map = []
s = s_begin
while s < s_end:
    session_map.append(round(s, 4))
    s += speed(s) / FPS

chapters = [e for e in E if e["type"] == "chapter"]
calls = [e for e in E if e["type"] == "call" and not e["quiet"]]
panels = [e for e in E if e["type"] == "panel"]
pointer = [{"s": e["s"], "x": e["x"], "y": e["y"]} for e in E if e["type"] == "pointer"]
clicks = [{"s": e["s"], "x": e["x"], "y": e["y"]} for e in E if e["type"] == "click"]
quiet_count = sum(1 for e in E if e["type"] == "call" and e["quiet"])

for c in calls:
    c.pop("t", None); c.pop("t_end", None)
    # keep long string args readable
    a = c["args"]
    for k, v in list(a.items()):
        if isinstance(v, str) and len(v) > 60:
            a[k] = f"<{len(v):,} chars>"
        if isinstance(v, list) and len(v) > 4:
            a[k] = v[:3] + [f"+{len(v) - 3} more"]

T = {
    "fps": FPS, "intro": INTRO, "outro": OUTRO, "session_map": session_map,
    "chapters": [{"s": c["s"], "n": c["n"], "title": c["title"], "sub": c["sub"]} for c in chapters],
    "calls": calls, "panels": [{k: v for k, v in p.items() if k != "t"} for p in panels],
    "pointer": pointer, "clicks": clicks, "quiet_count": quiet_count,
    "total_frames": int((INTRO + OUTRO) * FPS) + len(session_map),
}
open("timeline.js", "w").write("window.TL = " + json.dumps(T) + ";")
print("session out secs", len(session_map) / FPS, "total", T["total_frames"] / FPS, "glides", len(glides))
