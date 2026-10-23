window.__calls = [];
window.__pending = {};
window.__handlers = {};
window.__listeners = {};
window.__settings = { xboxHost: "192.168.1.9", maxJobs: 3, extractNested: false, xboxPass: "<b>x</b>" };
window.__behaviour = {};
window.__TAURI__ = {
  core: {
    invoke: (cmd, args) => {
      window.__calls.push({ cmd, args });
      const b = window.__behaviour[cmd];
      if (b) return b(args);
      switch (cmd) {
        case "app_meta": return Promise.resolve({ name: "Unpack", version: "9.9.9", title_count: 5000, seven_zip: "/usr/bin/7za", config_dir: "/tmp/<i>cfg</i>" });
        case "load_settings": return Promise.resolve(window.__settings);
        case "save_settings": window.__saved = args.settings; return Promise.resolve();
        case "search_titles": return Promise.resolve([{ name: "<img src=x onerror=window.__xss=1>", title_id: "<script>window.__xss=2</script>" }]);
        default: return Promise.resolve(null);
      }
    },
  },
  event: { listen: (name, cb) => { window.__listeners[name] = cb; return Promise.resolve(() => {}); } },
  webview: { getCurrentWebview: () => ({ onDragDropEvent: (cb) => { window.__drop = cb; } }) },
};
