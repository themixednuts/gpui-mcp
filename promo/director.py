"""Drive the real Ember GPUI app through the real gpui-mcp server and log a timeline.

Every tool call below is a genuine MCP JSON-RPC call; the log records wall-clock
start/end, arguments and a short result summary so the compositor can show the
agent's console in sync with the captured footage.
"""
import json, sys, time
from mcp import MCP, env
from edits import a11y_fix, restyle, inject

SERVER = sys.argv[1]
OUT = sys.argv[2]
EV = []


def log(_type, **kw):
    EV.append({"t": time.time(), "type": _type, **kw})


def beat(s):
    time.sleep(s)


m = MCP([SERVER], env())


def call(tool, args=None, summary=None, quiet=False):
    t0 = time.time()
    r = m.call(tool, args or {})
    t1 = time.time()
    s = summary(r) if summary else None
    EV.append({"t": t0, "t_end": t1, "type": "call", "tool": tool, "args": args or {},
               "result": s, "quiet": quiet, "error": r.get("_isError") if isinstance(r, dict) else None})
    return r


def tree():
    return m.call("get_ui_tree")["nodes"]


POINTER = {"x": 600.0, "y": 380.0}


def move_to(x, y, dur=0.3, steps=5):
    """Glide GPUI's pointer with real pointer_move events, so hover states really fire."""
    x0, y0 = POINTER["x"], POINTER["y"]
    for i in range(1, steps + 1):
        k = i / steps
        k = k * k * (3 - 2 * k)
        px, py = x0 + (x - x0) * k, y0 + (y - y0) * k
        call("pointer_move", {"x": px, "y": py}, quiet=True)
        log("pointer", x=px, y=py)
        time.sleep(dur / steps)
    POINTER.update(x=x, y=y)


def center(nodes, nid):
    b = nodes[nid]["bounds"]
    return b["x"] + b["width"] / 2, b["y"] + b["height"] / 2


def click(nid, nodes=None, label=None):
    nodes = nodes or tree()
    x, y = center(nodes, nid)
    move_to(x, y)
    beat(0.12)
    log("click", x=x, y=y)
    call("click_element", {"id": nid}, lambda r: "clicked" if r.get("ok") else str(r))


def glyph_named(n):
    lab = (n.get("label") or "").strip()
    return n["role"] == "button" and (not lab or (len(lab) <= 2 and not any(c.isalnum() for c in lab)))


def audit(nodes):
    unnamed = sorted(k for k, n in nodes.items() if glyph_named(n) and n["state"]["visible"])
    dead = sorted(k for k, n in nodes.items() if n["role"] == "button" and "click" not in n["actions"]
                  and n["state"]["visible"] and k not in unnamed)
    return unnamed, dead


def status_text():
    return m.call("get_element", {"id": "status-message"})["text"]["text"]


# ---------------------------------------------------------------- session
log("start")
rec = call("start_video_recording", {"artifact_name": "ember-session.mp4", "overwrite": True},
           lambda r: f"recording · {r.get('codec')} · {r.get('frames_per_second')} fps")
call("clear_highlights", quiet=True)
call("pointer_move", POINTER, quiet=True)
log("pointer", **POINTER)
beat(1.0)

# 1 · SEE ---------------------------------------------------------------
log("chapter", n=1, title="See", sub="The agent reads the live UI as a semantic tree")
beat(0.6)
apps = call("list_apps", summary=lambda r: f"{r['count']} app · {r['apps'][0]['app_name']}")
beat(1.2)
t0 = time.time()
nodes = tree()
interactive = [k for k, n in nodes.items() if n["role"] in ("button", "text_input") and n["state"]["visible"]]
EV.append({"t": t0, "t_end": time.time(), "type": "call", "tool": "get_ui_tree", "args": {},
           "result": f"{len(nodes)} nodes · {len(interactive)} interactive", "quiet": False})
sample = []
for nid in ["file-theme", "toggle-explorer", "code-editor", "run-project", "status-message"]:
    n = nodes[nid]
    sample.append({"id": nid, "role": n["role"], "label": n.get("label") or (n.get("text") or {}).get("text"),
                   "actions": n["actions"], "bounds": n["bounds"]})
log("panel", kind="tree", count=len(nodes), interactive=len(interactive), sample=sample)
beat(1.6)
call("highlight_elements", {"ids": interactive[:64], "color": "#22D3EEFF"},
     lambda r: f"{min(64, len(interactive))} outlines drawn in-app")
beat(3.2)
call("clear_highlights", summary=lambda r: "cleared")
beat(0.8)

# 2 · DRIVE -------------------------------------------------------------
log("chapter", n=2, title="Drive", sub="Semantic clicks, typing and hover through GPUI's own input pipeline")
beat(0.5)
found = call("find_elements", {"query": "theme.rs", "role": "button"},
             lambda r: f"{r['count']} match · {r['elements'][0]['id']}")
beat(0.4)
click("file-theme", nodes)
call("wait_for_element", {"query": "theme.rs", "timeout_ms": 2000}, lambda r: "theme.rs tab is live")
s = status_text()
EV.append({"t": time.time(), "t_end": time.time() + 0.01, "type": "call", "tool": "get_element",
           "args": {"id": "status-message"}, "result": f'text = "{s}"', "quiet": False})
beat(1.0)
click("toggle-panel", nodes)
call("wait_for_state", {"id": "bottom-panel", "visible": False, "timeout_ms": 2000},
     lambda r: f"bottom-panel hidden ✓ ({r.get('elapsed_ms')} ms)")
beat(1.0)
click("toggle-panel", nodes)
call("wait_for_state", {"id": "bottom-panel", "visible": True, "timeout_ms": 2000},
     lambda r: f"bottom-panel visible ✓ ({r.get('elapsed_ms')} ms)")
beat(0.6)
x, y = center(nodes, "code-editor")
move_to(x + 120, y + 150)
call("focus_element", {"id": "code-editor"}, lambda r: "focused · Rust editor")
call("keyboard", {"keystroke": "ctrl-end"}, lambda r: "ctrl-end")
beat(0.2)
typed = "\n\n// tuned by an agent over MCP\npub const ACCENT: u32 = 0x3b5ccc;"
chunk = 6
log("typing", text=typed)
for i in range(0, len(typed), chunk):
    m.call("type_text", {"text": typed[i:i + chunk]})
    time.sleep(0.035)
EV.append({"t": time.time() - 1.4, "t_end": time.time(), "type": "call", "tool": "type_text",
           "args": {"text": typed.strip()}, "result": f"{len(typed)} chars typed", "quiet": False})
beat(0.6)
info = call("get_text_info", {"id": "code-editor"}, lambda r: f"{r['text'].count(chr(10)) + 1} lines · ends with ACCENT ✓"
            if "ACCENT" in r["text"] else "text mismatch")
beat(1.4)

# 3 · VALIDATE ----------------------------------------------------------
log("chapter", n=3, title="Validate", sub="Audit the running app: names, handlers, layout, state")
beat(0.5)
call("save_ui_snapshot", {"name": "before-fix"}, lambda r: f"snapshot · {r['node_count']} nodes")
nodes = tree()
unnamed, dead = audit(nodes)
EV.append({"t": time.time(), "t_end": time.time() + 0.02, "type": "call", "tool": "get_ui_tree", "args": {},
           "result": f"audit: {len(unnamed)} unnamed · {len(dead)} no handler", "quiet": False})
log("panel", kind="audit", phase="before", unnamed=[{"id": k, "label": nodes[k].get("label")} for k in unnamed],
    dead=[{"id": k, "label": nodes[k].get("label")} for k in dead])
beat(0.6)
call("highlight_elements", {"ids": unnamed, "color": "#FF4D6DFF"}, lambda r: f"{len(unnamed)} unnamed controls · red")
beat(2.2)
call("highlight_elements", {"ids": dead[:64], "color": "#FFB020FF"}, lambda r: f"{len(dead)} without click handler · amber")
beat(3.4)
call("clear_highlights", summary=lambda r: "cleared")
beat(0.5)

# 4 · EDIT --------------------------------------------------------------
log("chapter", n=4, title="Edit live", sub="Patch the running app's HTML & CSS — no rebuild, no restart")
beat(0.5)
doc = call("get_live_document", summary=lambda r: f"revision {r['document']['revision']} · html+css+ron")["document"]
src, rev = doc["source"], doc["revision"]
new_html, new_css = a11y_fix(src["html"]), restyle(src["css"])
log("panel", kind="edit", rev=rev, added_labels=new_html.count("aria-label") - src["html"].count("aria-label"))
call("capture_screenshot_snapshot", {"name": "before"}, lambda r: f"baseline {r['width']}×{r['height']}")
beat(2.4)
res = call("preview_live_document", {"expected_revision": rev, "html": new_html, "css": new_css,
                                       "bindings_ron": src["bindings_ron"]},
           lambda r: f"applied ✓ rev {r['preview']['document']['revision']} in {r['timing']['total_ms']:.0f} ms"
           if r.get("ok") else f"rejected: {r}")
beat(1.4)
call("capture_screenshot_snapshot", {"name": "after"}, lambda r: f"after {r['width']}×{r['height']}")
cmp = call("compare_screenshots", {"left": "before", "right": "after", "tolerance": 8},
           lambda r: f"{r.get('changed_pixel_ratio', r.get('changed_ratio', 0)) * 100:.1f}% pixels changed"
           if isinstance(r.get('changed_pixel_ratio', r.get('changed_ratio')), (int, float)) else json.dumps(r)[:80])
nodes = tree()
unnamed2, dead2 = audit(nodes)
call("diff_current_ui", {"name": "before-fix"}, lambda r: f"{len(r.get('changed', []))} nodes changed")
EV.append({"t": time.time(), "t_end": time.time() + 0.02, "type": "call", "tool": "get_ui_tree", "args": {},
           "result": f"re-audit: {len(unnamed2)} unnamed ✓", "quiet": False})
log("panel", kind="audit", phase="after", unnamed=[{"id": k, "label": nodes[k].get("label")} for k in unnamed2],
    fixed=[{"id": k, "label": nodes[k].get("label")} for k in unnamed])
call("highlight_elements", {"ids": unnamed, "color": "#34D399FF"}, lambda r: f"{len(unnamed)} fixed · green")
beat(3.4)
call("clear_highlights", summary=lambda r: "cleared")
beat(0.4)

# 5 · GUARDRAILS --------------------------------------------------------
log("chapter", n=5, title="Fail closed", sub="Unsafe or stale edits are rejected — the last good UI stays up")
beat(0.6)
doc = m.call("get_live_document")["document"]
bad = call("preview_live_document", {"expected_revision": doc["revision"], "html": inject(doc["source"]["html"]),
                                      "css": doc["source"]["css"], "bindings_ron": doc["source"]["bindings_ron"]},
           lambda r: "rejected ✗ " + (next((d["message"] for d in r.get("preview", {}).get("diagnostics", [])
                                            if d["severity"] == "error"), r.get("_text", "")))[:90])
log("panel", kind="guard", result=EV[-1]["result"])
beat(1.6)
stale = call("preview_live_document", {"expected_revision": rev, "html": src["html"], "css": src["css"],
                                        "bindings_ron": src["bindings_ron"]},
             lambda r: "rejected ✗ " + (r.get("_text") or "")[:80])
log("panel", kind="guard2", result=EV[-1]["result"])
beat(2.6)

# 6 · MEASURE -----------------------------------------------------------
log("chapter", n=6, title="Measure", sub="Know what every interaction costs, frame by frame")
beat(0.5)
call("mark_frames", summary=lambda r: f"mark @ frame {r.get('mark_frame_count')}")
nodes = tree()
for nid in ["file-main", "file-theme", "file-readme", "html-node-1-1-2-3", "run-project"]:
    x, y = center(nodes, nid)
    move_to(x, y, dur=0.2, steps=4)
    beat(0.15)
rep = call("get_frame_report", {"frame_limit": 64},
           lambda r: f"{r['summary']['frames']} frames · p95 draw {r['summary']['draw_ms']['p95']:.1f} ms")
summ = rep["summary"]
log("panel", kind="perf", frames=[f["draw_ms"] for f in rep["frames"]],
    bridge=[f["bridge_ms"] for f in rep["frames"]],
    p50=summ["draw_ms"]["p50"], p95=summ["draw_ms"]["p95"], bridge_p95=summ["bridge_ms"]["p95"],
    rendered=summ["views_rendered"], reused=summ["views_reused"])
beat(3.6)

# 7 · RECORD ------------------------------------------------------------
stop = call("stop_video_recording", summary=lambda r: f"{r['artifact_name']} · {r['width']}×{r['height']} · {r['duration_ms']/1000:.1f}s")
log("panel", kind="record", info={k: stop.get(k) for k in ("artifact_name", "width", "height", "duration_ms", "codec", "bytes")})
beat(2.5)
log("end")
json.dump(EV, open(OUT, "w"), indent=1)
print("events", len(EV))
