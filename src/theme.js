// Applies the saved light/dark theme before first paint (no flash). "system" follows Windows.
(function () {
  var mq = window.matchMedia("(prefers-color-scheme: light)");
  function apply() {
    var t = localStorage.getItem("unpack.theme") || "dark";
    var light = t === "light" || (t === "system" && mq.matches);
    document.documentElement.setAttribute("data-theme", light ? "light" : "dark");
  }
  window.__applyTheme = apply;
  apply();
  if (mq.addEventListener) mq.addEventListener("change", apply);
})();
