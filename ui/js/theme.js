// Applied synchronously in <head> so the first paint already uses the right theme.
// (A classic script file: inline scripts are forbidden by our Content-Security-Policy.)
(function () {
  var theme = "system";
  try {
    var saved = window.localStorage.getItem("oc-theme");
    if (saved === "light" || saved === "dark" || saved === "system") theme = saved;
  } catch (e) { /* storage unavailable: follow the system */ }
  document.documentElement.setAttribute("data-theme", theme);
})();
