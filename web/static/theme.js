// Runs before the page is drawn (it is not deferred): applies the saved style, look and privacy mode, so nothing flashes.
(function () {
  try {
    var theme = localStorage.getItem("mymoney-theme");
    if (theme && theme !== "auto" && /^[a-z-]+$/.test(theme)) document.documentElement.setAttribute("data-theme", theme);
    if (localStorage.getItem("mymoney-look") === "modern") document.documentElement.setAttribute("data-look", "modern");
    if (localStorage.getItem("mymoney-privacy") === "1") document.documentElement.classList.add("privacy");
  } catch (error) {
    /* storage blocked: the default style applies */
  }
})();
