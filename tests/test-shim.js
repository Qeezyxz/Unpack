// TEST ONLY: fake window.__TAURI__ that forwards to examples/bridge.rs. Never loaded by the app.
(() => {
  const listeners = {};
  let since = null;
  const deliver = (events) => {
    for (const e of events) (listeners[e.event] || []).forEach((fn) => fn({ payload: e.payload }));
  };
  const bootstrapEventsCursor = async () => {
    for (let i = 0; i < 200 && since === null; i++) {
      try {
        const ac = new AbortController();
        const timer = setTimeout(() => ac.abort(), 3000);
        const res = await fetch("/events?since=0", { signal: ac.signal });
        clearTimeout(timer);
        if (!res.ok) continue;
        const body = await res.json();
        since = body.next;
        return;
      } catch {}
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  };
  const pollEvents = async () => {
    await bootstrapEventsCursor();
    if (since === null) return;
    for (;;) {
      try {
        const ac = new AbortController();
        const timer = setTimeout(() => ac.abort(), 3000);
        const res = await fetch("/events?since=" + since, { signal: ac.signal });
        clearTimeout(timer);
        if (!res.ok) throw new Error("events " + res.status);
        const r = await res.json();
        since = r.next;
        deliver(r.events || []);
      } catch {
        await new Promise((resolve) => setTimeout(resolve, 50));
        continue;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  };
  void pollEvents();
  let webviewDragDropCb = null;
  window.__TEST = { pick: [], folder: null, save: null, drop: null, calls: [], version: "1.7.4" };
  window.__TEST.drop = async (paths) => {
    await fetch("/invoke/__test_simulate_native_drop", { method: "POST", body: JSON.stringify({ paths }) });
    webviewDragDropCb?.({ payload: { type: "drop" } });
  };
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        window.__TEST.calls.push([cmd, args]);
        if (cmd === "pick_save") {
          const v = window.__TEST.save;
          if (v) {
            const parent = v.replace(/[\\/][^\\/]+$/, "") || v;
            await fetch("/invoke/__test_grant", { method: "POST", body: JSON.stringify({ paths: [parent, v] }) });
          }
          return v;
        }
        if (cmd === "pick_path") {
          // fake native picker: what the test set up, granted on the bridge like the real one does
          const v = args.kind === "folder" ? (window.__TEST.folder ? [window.__TEST.folder] : []) : [...(window.__TEST.pick || [])];
          await fetch("/invoke/__test_grant", { method: "POST", body: JSON.stringify({ paths: v }) });
          return v;
        }
        const r = await fetch("/invoke/" + cmd, { method: "POST", body: JSON.stringify(args || {}) });
        const v = await r.json();
        if (!r.ok) throw v;
        return v;
      },
    },
    app: { getVersion: async () => window.__TEST.version },
    event: { listen: async (name, fn) => { (listeners[name] ||= []).push(fn); return () => {}; } },
    dialog: {
      open: async (o) => (o.directory ? window.__TEST.folder : window.__TEST.pick),
      save: async () => window.__TEST.save,
    },
    webview: { getCurrentWebview: () => ({ onDragDropEvent: async (fn) => { webviewDragDropCb = fn; } }) },
  };
})();
