// TEST ONLY: fake window.__TAURI__ that forwards to examples/bridge.rs. Never loaded by the app.
(() => {
  const listeners = {};
  let since = null;
  setInterval(async () => {
    const r = await fetch("/events?since=" + (since ?? 0)).then((r) => r.json());
    if (since === null) {
      since = r.next;
      return;
    }
    since = r.next;
    for (const e of r.events) (listeners[e.event] || []).forEach((fn) => fn({ payload: e.payload }));
  }, 50);
  window.__TEST = { pick: [], folder: null, save: null, drop: null, calls: [], version: "1.7.4" };
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
    webview: { getCurrentWebview: () => ({ onDragDropEvent: async (fn) => {
      window.__TEST.drop = async (paths) => {
        await fetch("/invoke/__test_simulate_native_drop", { method: "POST", body: JSON.stringify({ paths }) });
        fn({ payload: { type: "drop", paths } });
      };
    } }) },
  };
})();
