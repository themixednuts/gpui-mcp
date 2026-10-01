import json, subprocess, os, time, threading
class MCP:
    def __init__(self, cmd, env):
        self.p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(os.environ.get('MCP_ERR','/dev/null'),'w'), env=env, text=True, bufsize=1)
        self.i = 0
        self.req('initialize', {"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"promo","version":"1"}})
        self.send({"jsonrpc":"2.0","method":"notifications/initialized"})
    def send(self, m):
        self.p.stdin.write(json.dumps(m)+"\n"); self.p.stdin.flush()
    def req(self, method, params=None):
        self.i += 1; mid = self.i
        self.send({"jsonrpc":"2.0","id":mid,"method":method,"params":params or {}})
        while True:
            line = self.p.stdout.readline()
            if not line: raise RuntimeError("server closed")
            m = json.loads(line)
            if m.get("id") == mid: return m
    def call(self, name, args=None):
        r = self.req("tools/call", {"name":name, "arguments":args or {}})
        if "error" in r: return {"_error": r["error"]}
        res = r["result"]
        out = res.get("structuredContent")
        if out is None:
            texts=[c.get("text") for c in res.get("content",[]) if c.get("type")=="text"]
            out = {"_text": "\n".join(t for t in texts if t)}
            for c in res.get("content",[]):
                if c.get("type")=="image": out["_image"]=c
        if res.get("isError"): out={"_isError":True, **(out if isinstance(out,dict) else {"v":out})}
        return out
def env():
    """Server environment: same display and discovery directory as the app."""
    e = dict(os.environ)
    e.setdefault("DISPLAY", ":99")
    e.setdefault("XDG_RUNTIME_DIR", "/tmp/xdg")
    e["WAYLAND_DISPLAY"] = ""
    return e
