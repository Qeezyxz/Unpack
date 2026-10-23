const invoke = (...args) => window.__TAURI__.core.invoke(...args);
const listen = (...args) => window.__TAURI__.event.listen(...args);

const HOMEBREW = [
  { name: "Aurora Dashboard", cat: "Dashboards & File Explorers", blurb: "Feature-rich custom dashboard for RGH/JTAG consoles (game scanning, covers, scripts).", url: "https://consolemods.org/wiki/Xbox_360:Aurora" },
  { name: "XeXMenu 1.2", cat: "Dashboards & File Explorers", blurb: "Classic file manager / launcher used as a backup dashboard on modded consoles.", url: "https://consolemods.org/wiki/Xbox_360:XeXMenu" },
  { name: "DashLaunch", cat: "Dashboards & File Explorers", blurb: "Configures launch.ini (default dashboard, plugins, fans) on modded consoles.", url: "https://consolemods.org/wiki/Xbox_360:DashLaunch" },
  { name: "XE Unshackle", cat: "Dashboards & File Explorers", blurb: "Temporary HV/kernel patch loader for BadUpdate; loads DashLaunch from USB.", url: "https://github.com/Byrom90/XeUnshackle" },
  { name: "Horizon", cat: "PC Utilities", blurb: "PC content manager for Xbox 360 profiles, saves, and device browsing.", url: "https://www.horizonmb.com/" },
  { name: "Modio", cat: "PC Utilities", blurb: "PC all-in-one for browsing Xbox 360 USB/HDD content and editing packages.", url: "https://github.com/bboxy/modio" },
  { name: "Le Fluffie", cat: "PC Utilities", blurb: "Open-source PC tool for profiles, achievements, and package security (rehash/resign).", url: "https://consolemods.org/wiki/Xbox_360:Game_Saves" },
  { name: "Xbox 360 Hash Block Calculator", cat: "PC Utilities", blurb: "Recalculates STFS hash blocks after editing a package or save.", url: "https://consolemods.org/wiki/Xbox_360:STFS" },
  { name: "USBXTAFGUI", cat: "PC Utilities", blurb: "Browse, extract, and inject files on Xbox 360-formatted USB drives.", url: "https://github.com/Swizzy/USBXTAFGUI" },
  { name: "RDR Gamesave Editor", cat: "Save Editors", blurb: "Edit Red Dead Redemption Xbox 360 save games offline.", url: "https://consolemods.org/wiki/Xbox_360:Game_Saves" },
  { name: "Saints Row save editor", cat: "Save Editors", blurb: "Edit Saints Row Xbox 360 save games offline.", url: "https://www.saintsrowmods.com/" },
  { name: "WillowTree (Borderlands)", cat: "Save Editors", blurb: "Borderlands save editor for Xbox 360 (characters, items, stats).", url: "https://github.com/mattweidner/WillowTreeSharp" },
];

const ESCAPES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
function esc(value) {
  return String(value ?? "").replace(/[&<>"']/g, (c) => ESCAPES[c]);
}

const state = {
  packFiles: [],
  xboxFiles: [],
  packageFiles: [],
  modFiles: [],
  settings: {},
  packOut: "",
};

const $ = (id) => document.getElementById(id);
let currentView = "extract";
const runningJobs = [];
const jobWaiters = [];
let jobSeq = 0;

function native() {
  return Boolean(window.__TAURI__?.core?.invoke);
}

function log(line) {
  $("job-status").textContent = line;
}

function showError(message) {
  const text = String(message ?? "").replace(/^Error:\s*/, "").trim() || "Something went wrong.";
  $("job-status").textContent = "";
  $("job-error-text").textContent = text;
  $("job-error").classList.remove("hidden");
}

function clearError() {
  $("job-error").classList.add("hidden");
  $("job-error-text").textContent = "";
}

function setPresence(view, job) {
  currentView = view;
  if (!native()) return;
  invoke("discord_set_activity", { view, job: job || null }).catch(() => {});
}

async function pickMany(title, extensions) {
  if (!native()) return [];
  return ((await invoke("pick_files", { title, extensions })) || []).filter(Boolean);
}
async function pickDir(title) {
  if (!native()) return null;
  return invoke("pick_folder", { title });
}

async function remember() {
  if (!native()) return true;
  try {
    await invoke("save_settings", { settings: collectSettings() });
    return true;
  } catch (_) {
    return false;
  }
}

function saveButton(id) {
  const btn = $(id);
  const label = btn.textContent;
  btn.onclick = async () => {
    const ok = await remember();
    btn.textContent = ok ? "Saved" : "Could not save";
    setTimeout(() => { btn.textContent = label; }, 1500);
  };
}

function maxJobs() {
  const n = Number($("max-jobs")?.value || 2);
  return Math.min(4, Math.max(1, Number.isFinite(n) ? n : 2));
}

function showJobs() {
  const bar = $("job-progress");
  if (!runningJobs.length) {
    bar.hidden = true;
    setPresence(currentView, null);
    return;
  }
  bar.hidden = false;
  const labels = runningJobs.map((j) => j.label);
  const extra = jobWaiters.length ? ` · ${jobWaiters.length} waiting` : "";
  log(`${labels.join(" · ")}${extra}`);
  setPresence(currentView, labels[labels.length - 1]);
}

async function job(label, fn) {
  if (runningJobs.length >= maxJobs()) {
    log(`${label} waiting (${runningJobs.length} already running)…`);
    await new Promise((resolve) => jobWaiters.push(resolve));
  }
  const id = ++jobSeq;
  if (!runningJobs.length) clearError();
  runningJobs.push({ id, label });
  showJobs();
  try {
    await fn();
    await remember();
  } catch (e) {
    showError(e);
  } finally {
    const i = runningJobs.findIndex((j) => j.id === id);
    if (i >= 0) runningJobs.splice(i, 1);
    const next = jobWaiters.shift();
    if (next) next();
    showJobs();
  }
}

function isIso(path) {
  return /\.(iso|000)$/i.test(path);
}

function openModal(name) {
  closeModal();
  $("overlay").classList.remove("hidden");
  $(`modal-${name}`).classList.remove("hidden");
  const view = { library: "library", mods: "mods", homebrew: "homebrew", xbox: "xbox", dlc: "content", archive: "extract", settings: "settings" }[name] || "extract";
  setPresence(view, null);
}

function closeModal() {
  $("overlay").classList.add("hidden");
  document.querySelectorAll(".modal").forEach((el) => el.classList.add("hidden"));
  setPresence("extract", null);
}

function collectSettings() {
  return {
    libraryRoot: $("library-root").value,
    usbRoot: $("usb-dest").value,
    downloadDir: $("dl-dest").value,
    xboxHost: $("xbox-host").value,
    xboxPort: Number($("xbox-port").value || 21),
    xboxUser: $("xbox-user").value,
    xboxPass: $("xbox-pass").value,
    xboxName: $("xbox-name").value,
    extractDest: $("extract-dest").value,
    contentRoot: $("content-root").value,
    theme: $("theme-select").value,
    discordPresence: $("discord-presence").checked,
    autoIso: $("auto-iso").value,
    extractAuto: $("extract-auto").checked,
    skipJunk: $("extract-junk").checked,
    extractNested: $("extract-nested").checked,
    existing: $("extract-existing").value,
    maxJobs: maxJobs(),
  };
}

function applySettings(s) {
  if (!s) return;
  const set = (id, v) => { if (v != null && $(id)) $(id).value = v; };
  set("library-root", s.libraryRoot);
  set("usb-dest", s.usbRoot);
  set("dl-dest", s.downloadDir);
  set("xbox-host", s.xboxHost);
  if (s.xboxPort) $("xbox-port").value = s.xboxPort;
  set("xbox-user", s.xboxUser);
  set("xbox-pass", s.xboxPass);
  set("xbox-name", s.xboxName);
  set("extract-dest", s.extractDest);
  set("content-root", s.contentRoot);
  if (s.autoIso) $("auto-iso").value = s.autoIso;
  if (typeof s.extractAuto === "boolean") $("extract-auto").checked = s.extractAuto;
  if (typeof s.skipJunk === "boolean") $("extract-junk").checked = s.skipJunk;
  if (typeof s.extractNested === "boolean") $("extract-nested").checked = s.extractNested;
  if (s.existing) $("extract-existing").value = s.existing;
  if (s.maxJobs) $("max-jobs").value = String(Math.min(4, Math.max(1, Number(s.maxJobs) || 2)));
  syncIsoBadge();
}

function syncIsoBadge() {
  const mode = $("auto-iso").value;
  const badge = $("auto-badge");
  if (mode === "off") {
    badge.textContent = "ISO OFF";
    badge.style.color = "var(--muted)";
  } else {
    badge.textContent = `AUTO ${mode.toUpperCase()}`;
    badge.style.color = "var(--green)";
  }
}

function setList(id, paths) {
  const el = $(id);
  el.innerHTML = "";
  paths.forEach((p, i) => {
    const li = document.createElement("li");
    li.innerHTML = `<span>${esc(p)}</span><button type="button">remove</button>`;
    li.querySelector("button").onclick = () => {
      paths.splice(i, 1);
      setList(id, paths);
    };
    el.appendChild(li);
  });
}

async function convertIsos(isos, dest) {
  if (!isos?.length || $("auto-iso").value === "off") return;
  await job("Converting ISO…", async () => {
    const reports = await invoke("convert_iso", {
      paths: isos,
      dest,
      mode: $("auto-iso").value,
      trim: true,
      verify: true,
      autoCompanions: true,
    });
    const list = Array.isArray(reports) ? reports : [reports];
    for (const report of list) {
      const discs = report.merged ? ` (${report.discs?.length || 0} discs)` : "";
      log(`Converted ${report.info?.name || "ISO"}${discs} → ${report.output}`);
    }
  });
}

function extractSummary(result) {
  const count = result.length;
  const opened = result.reduce((n, r) => n + (r.nested || 0), 0);
  const left = result.reduce((n, r) => n + (r.inner_left || 0), 0);
  let text = opened ? `Extracted ${count} archive(s) and opened ${opened} inside.` : `Extracted ${count} archive(s).`;
  if (left) {
    text += $("extract-nested").checked
      ? ` ${left} zip/rar/7z file(s) inside could not be opened and were left as they are.`
      : ` ${left} zip/rar/7z file(s) inside were left closed. Turn on "Open zips inside zips" to open them.`;
  }
  return text;
}

async function handlePaths(paths) {
  if (!paths?.length) return;
  const dest = $("extract-dest").value.trim();
  const isos = paths.filter(isIso);
  const archives = paths.filter((p) => !isIso(p));
  const tasks = [];
  if (archives.length) {
    setList("recent-files", archives.slice());
    if (!$("extract-auto").checked) {
      log(`Added ${archives.length} file(s) but nothing was extracted. Turn on "Extract as soon as they open" to extract on drop.`);
    }
    if ($("extract-auto").checked) {
      if (!dest) {
        showError("Choose a destination folder first.");
        return;
      }
      tasks.push((async () => {
        let found = [];
        await job("Extracting…", async () => {
          const result = await invoke("extract_archives", {
            archives,
            dest,
            skipJunk: $("extract-junk").checked,
            existing: $("extract-existing").value,
            nested: $("extract-nested").checked,
          });
          log(extractSummary(result));
          found = result.flatMap((r) => r.isos || []);
          await invoke("notify", { title: "Unpack", body: `Extracted ${result.length} archive(s)` }).catch(() => {});
        });
        if (found.length) await convertIsos(found, dest);
      })());
    }
  }
  if (isos.length && $("auto-iso").value === "off") {
    log("Auto ISO is Off, so the disc image was left alone. Pick GOD or XEX to convert it.");
  }
  if (isos.length && $("auto-iso").value !== "off") {
    if (!dest) {
      showError("Choose a destination folder first.");
      return;
    }
    tasks.push(convertIsos(isos, dest));
  }
  await Promise.all(tasks);
}

function renderHomebrew() {
  const q = ($("hb-search").value || "").toLowerCase();
  const cat = $("hb-cat").value;
  const el = $("hb-cards");
  el.innerHTML = "";
  HOMEBREW.filter((t) => (!cat || t.cat === cat) && (!q || `${t.name} ${t.blurb} ${t.cat}`.toLowerCase().includes(q)))
    .forEach((t) => {
      const card = document.createElement("article");
      card.className = "card";
      const save = t.cat === "Save Editors";
      card.innerHTML = `<div><h3>${esc(t.name)}</h3><span class="tag${save ? " save" : ""}">${esc(t.cat)}</span></div><p>${esc(t.blurb)}</p><button type="button">Open source page</button>`;
      card.querySelector("button").onclick = async () => {
        try { await invoke("open_path", { path: t.url }); }
        catch (e) { showError(e); window.open(t.url, "_blank"); }
      };
      el.appendChild(card);
    });
}

async function refreshLibrary() {
  const root = $("library-root").value.trim();
  const empty = $("library-empty");
  const list = $("library-results");
  list.innerHTML = "";
  if (!root) {
    empty.classList.remove("hidden");
    return;
  }
  try {
    const items = await invoke("scan_library", { root });
    if (!items.length) {
      empty.classList.remove("hidden");
      return;
    }
    empty.classList.add("hidden");
    items.forEach((item) => {
      const li = document.createElement("li");
      li.innerHTML = `<span>${esc(item.name)}</span><span class="muted">${esc(item.kind)} ${esc(item.title_id || "")}</span>`;
      li.onclick = () => {
        state.xboxFiles = [item.path].filter(Boolean);
        if (state.xboxFiles.length) {
          setList("xbox-files", state.xboxFiles);
          openModal("xbox");
        }
      };
      list.appendChild(li);
    });
    log(`Found ${items.length} item(s).`);
  } catch (e) {
    empty.classList.remove("hidden");
    showError(e);
  }
}

async function init() {
  document.querySelectorAll("[data-modal]").forEach((b) => {
    b.addEventListener("click", (e) => {
      e.preventDefault();
      openModal(b.dataset.modal);
      if (b.dataset.modal === "library") refreshLibrary();
      if (b.dataset.modal === "homebrew") renderHomebrew();
    });
  });
  document.querySelectorAll(".close-modal").forEach((b) => b.addEventListener("click", closeModal));
  $("overlay").onclick = closeModal;

  $("theme-select").value = localStorage.getItem("unpack.theme") || "dark";
  $("theme-select").onchange = () => {
    localStorage.setItem("unpack.theme", $("theme-select").value);
    window.__applyTheme();
  };
  $("auto-iso").onchange = syncIsoBadge;
  $("iso-mode-btn").onclick = () => {
    const order = ["god", "xex", "off"];
    const i = order.indexOf($("auto-iso").value);
    $("auto-iso").value = order[(i + 1) % order.length];
    syncIsoBadge();
  };

  const cats = [...new Set(HOMEBREW.map((t) => t.cat))];
  cats.forEach((c) => {
    const o = document.createElement("option");
    o.value = c;
    o.textContent = c;
    $("hb-cat").appendChild(o);
  });
  $("hb-search").oninput = renderHomebrew;
  $("hb-cat").onchange = renderHomebrew;
  renderHomebrew();

  document.querySelectorAll(".tab").forEach((tab) => {
    tab.onclick = () => {
      tab.parentElement.querySelectorAll(".tab").forEach((t) => t.classList.remove("on"));
      tab.classList.add("on");
      ["mods-files", "mods-dl", "mods-names"].forEach((id) => $(id).classList.toggle("hidden", tab.dataset.tab !== id));
    };
  });

  try {
    if (native()) {
      const meta = await invoke("app_meta");
      $("settings-meta").innerHTML = `
        <dt>Version</dt><dd>${esc(meta.version)}</dd>
        <dt>TitleIDs</dt><dd>${meta.title_count.toLocaleString()}</dd>
        <dt>7-Zip</dt><dd>${esc(meta.seven_zip || "not found (put 7za.exe next to Unpack.exe)")}</dd>
        <dt>Config</dt><dd>${esc(meta.config_dir)}</dd>`;
      state.settings = await invoke("load_settings");
      applySettings(state.settings);
      const presenceOn = state.settings?.discordPresence !== false;
      $("discord-presence").checked = presenceOn;
      await invoke("discord_set_enabled", { enabled: presenceOn }).catch(() => {});
      setPresence("extract", null);
    }
  } catch (err) {
    log(`Open via Unpack.exe / npx tauri dev (${err}).`);
  }

  if (window.__TAURI__?.event) {
    await listen("unpack-progress", (event) => {
      const p = event.payload;
      $("job-progress").max = Math.max(1, p.total);
      $("job-progress").value = p.current;
      $("job-status").textContent = p.message;
    });
    try {
      const webview = window.__TAURI__.webview.getCurrentWebview();
      webview.onDragDropEvent((event) => {
        if (event.payload.type === "drop") handlePaths(event.payload.paths || []);
      });
    } catch (_) { /* older webview */ }
  }

  $("dismiss-error").onclick = clearError;
  $("copy-error").onclick = async () => {
    const text = $("job-error-text").textContent;
    try {
      await navigator.clipboard.writeText(text);
      $("copy-error").textContent = "Copied";
    } catch (_) {
      const range = document.createRange();
      range.selectNodeContents($("job-error-text"));
      const sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
      $("copy-error").textContent = "Selected";
    }
    setTimeout(() => { $("copy-error").textContent = "Copy"; }, 1500);
  };

  const drop = $("drop-zone");
  drop.addEventListener("click", async () => {
    const files = await pickMany("Archives", ["zip", "rar", "7z", "tar", "gz", "xz", "iso"]);
    await handlePaths(files);
  });
  drop.addEventListener("dragover", (e) => { e.preventDefault(); drop.classList.add("over"); });
  drop.addEventListener("dragleave", () => drop.classList.remove("over"));
  drop.addEventListener("drop", (e) => {
    e.preventDefault();
    drop.classList.remove("over");
  });

  $("pick-extract-dest").onclick = async () => {
    const d = await pickDir("Extract to");
    if (d) $("extract-dest").value = d;
  };
  $("sample-zip").onclick = async () => {
    let sample = null;
    await job("Creating sample ZIP…", async () => {
      const dest = $("extract-dest").value.trim();
      if (!dest) throw new Error("Choose a destination folder first.");
      const inputs = await pickMany("Pick any small file to pack as a sample");
      if (!inputs.length) throw new Error("Pick a file to wrap in a sample ZIP, or drop a real archive.");
      const out = `${dest.replace(/[\\/]+$/, "")}/Unpack-sample.zip`;
      await invoke("create_archive", { inputs, dest: out });
      sample = out;
    });
    // Extracting is its own job. Starting it from inside this one would wait
    // for a slot this job is still holding, and hang when only one job may run.
    if (sample) await handlePaths([sample]);
  };

  $("pick-library").onclick = async () => {
    const d = await pickDir("Game library folder");
    if (d) $("library-root").value = d;
  };
  $("scan-library").onclick = () => job("Scanning library…", refreshLibrary);

  const renderMods = () => {
    const q = $("mods-search").value.trim().toLowerCase();
    const list = $("mods-list");
    list.innerHTML = "";
    state.modFiles.forEach((p, i) => {
      if (q && !p.toLowerCase().includes(q)) return;
      const li = document.createElement("li");
      li.innerHTML = `<span>${esc(p)}</span><button type="button">remove</button>`;
      li.querySelector("button").onclick = () => {
        state.modFiles.splice(i, 1);
        renderMods();
      };
      list.appendChild(li);
    });
    $("mods-empty").classList.toggle("hidden", state.modFiles.length > 0);
  };
  $("mods-search").oninput = renderMods;
  $("pick-mod-files").onclick = async () => {
    const files = await pickMany("Mod files");
    state.modFiles.push(...files);
    renderMods();
  };
  $("pick-mod-folder").onclick = async () => {
    const d = await pickDir("Mods folder");
    if (d) {
      state.modFiles.push(d);
      renderMods();
    }
  };
  $("pick-dl-dest").onclick = async () => {
    const d = await pickDir("Download folder");
    if (d) $("dl-dest").value = d;
  };
  $("run-download").onclick = () => job("Downloading…", async () => {
    const report = await invoke("download_url", {
      url: $("dl-url").value,
      destDir: $("dl-dest").value,
      expectedHash: $("dl-hash").value || null,
    });
    log(`Downloaded ${report.bytes} bytes to ${report.path}`);
  });

  let searchTimer;
  $("title-search").oninput = () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(async () => {
      try {
        const rows = await invoke("search_titles", { query: $("title-search").value, limit: 80 });
        const el = $("title-results");
        el.innerHTML = "";
        rows.forEach((r) => {
          const li = document.createElement("li");
          li.innerHTML = `<span>${esc(r.name)}</span><span>${esc(r.title_id)}</span>`;
          el.appendChild(li);
        });
      } catch (e) { showError(e); }
    }, 160);
  };

  $("pick-xbox-files").onclick = async () => {
    const files = await pickMany("Send to Xbox");
    state.xboxFiles.push(...files);
    setList("xbox-files", state.xboxFiles);
  };
  $("pick-xbox-folder").onclick = async () => {
    const d = await pickDir("Folder to send");
    if (d) {
      state.xboxFiles.push(d);
      setList("xbox-files", state.xboxFiles);
    }
  };
  $("xbox-from-library").onclick = () => openModal("library");
  $("xbox-test").onclick = () => job("Connecting…", async () => {
    const msg = await invoke("xbox_test", {
      host: $("xbox-host").value,
      port: Number($("xbox-port").value || 21),
      user: $("xbox-user").value,
      pass: $("xbox-pass").value,
    });
    log(msg);
  });
  saveButton("xbox-save");
  $("run-send").onclick = () => job("Sending to Xbox…", async () => {
    const report = await invoke("send_to_xbox", {
      host: $("xbox-host").value,
      port: Number($("xbox-port").value || 21),
      user: $("xbox-user").value,
      pass: $("xbox-pass").value,
      dest: "/Hdd1/Games",
      paths: state.xboxFiles,
      includeLaunchIni: $("xbox-launchini").checked,
    });
    const extra = report.resumed ? ` ${report.resumed} picked up after a drop.` : "";
    log(`Sent ${report.uploaded} file(s) to ${report.host}.${extra}`);
    (report.warnings || []).forEach(log);
  });

  $("pick-archive-inputs").onclick = async () => {
    state.packFiles = await pickMany("Files to pack");
    setList("pack-files", state.packFiles);
  };
  $("pick-archive-out").onclick = async () => {
    state.packOut = await invoke("pick_save", { title: "Save archive", defaultName: "archive.7z" });
    if (state.packOut) $("pack-out").textContent = state.packOut;
  };
  $("run-pack").onclick = () => job("Creating archive…", async () => {
    if (!state.packFiles.length || !state.packOut) throw new Error("Choose files and an archive name.");
    const result = await invoke("create_archive", { inputs: state.packFiles, dest: state.packOut });
    log(`Created ${result.archive}`);
    $("pack-out").textContent = `Created ${result.archive}`;
  });

  $("pick-packages").onclick = async () => {
    state.packageFiles = await pickMany("DLC / Title Updates");
    log(`${state.packageFiles.length} package(s) selected.`);
  };
  $("pick-content-root").onclick = async () => {
    const d = await pickDir("Content root");
    if (d) $("content-root").value = d;
  };
  $("run-place").onclick = () => job("Placing DLC / updates…", async () => {
    if (!state.packageFiles.length) throw new Error("Choose DLC / TU files first.");
    if (!$("content-root").value) throw new Error("Choose a library or USB root.");
    const el = $("content-results");
    el.innerHTML = "";
    let failed = 0;
    for (const path of state.packageFiles) {
      const li = document.createElement("li");
      try {
        const item = await invoke("place_content", { path, libraryRoot: $("content-root").value });
        li.innerHTML = `<span>${esc(item.name || path)}</span><span>${esc(item.detail)}</span>`;
      } catch (e) {
        failed++;
        li.innerHTML = `<span>${esc(path)}</span><span>${esc(String(e && e.message || e))}</span>`;
      }
      el.appendChild(li);
    }
    if (failed) throw new Error(`${failed} of ${state.packageFiles.length} package(s) could not be placed. See the list.`);
  });

  $("max-jobs").onchange = remember;
  $("extract-nested").onchange = remember;
  $("discord-presence").onchange = async () => {
    if (!native()) return;
    const enabled = $("discord-presence").checked;
    await invoke("discord_set_enabled", { enabled }).catch(() => {});
    await remember();
  };
  saveButton("save-settings");

  document.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "o") {
      e.preventDefault();
      $("drop-zone").click();
    }
    if (e.key === "Escape") closeModal();
  });

  syncIsoBadge();
  const preview = new URLSearchParams(location.search).get("modal");
  if (preview) {
    openModal(preview);
    if (preview === "library") refreshLibrary();
    if (preview === "homebrew") renderHomebrew();
  }
}

init();
