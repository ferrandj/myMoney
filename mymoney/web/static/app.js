// No inline scripts (strict Content-Security-Policy): behaviours are attached here.
document.addEventListener("submit", (event) => {
  const message = event.target.getAttribute("data-confirm");
  if (message && !window.confirm(message)) event.preventDefault();
});

// Installable web app: the service worker only exists so the browser offers "Install" (it caches nothing).
// It needs HTTPS (or localhost): over plain http the browser refuses it and the app stays a normal web page.
if ("serviceWorker" in navigator) {
  window.addEventListener("load", () => navigator.serviceWorker.register("/sw.js").catch(() => {}));
}
let installPrompt = null;
window.addEventListener("beforeinstallprompt", (event) => {
  event.preventDefault();
  installPrompt = event;
  const button = document.getElementById("install-app");
  if (button) button.hidden = false;
});
window.addEventListener("appinstalled", () => {
  installPrompt = null;
  const section = document.getElementById("install");
  if (section) section.hidden = true;
});
const installButton = document.getElementById("install-app");
if (installButton) {
  installButton.addEventListener("click", async () => {
    if (!installPrompt) return;
    installPrompt.prompt();
    await installPrompt.userChoice;
    installPrompt = null;
    installButton.hidden = true;
  });
}
const installNote = document.getElementById("install-note");
if (installNote) {
  if (window.matchMedia("(display-mode: standalone)").matches) {
    installNote.textContent = "L'application est déjà installée sur cet appareil.";
  } else if (!window.isSecureContext) {
    installNote.textContent =
      "L'installation demande une adresse en HTTPS (reverse proxy du NAS ou Tailscale). Ici, en HTTP, le navigateur la refuse.";
  }
}

function toggleNewAccount(select) {
  const fieldset = select.closest("form").querySelector(".new-account");
  if (fieldset) fieldset.hidden = select.value !== "new";
}
document.querySelectorAll("select[data-toggle-new]").forEach((select) => {
  toggleNewAccount(select);
  select.addEventListener("change", () => toggleNewAccount(select));
});
