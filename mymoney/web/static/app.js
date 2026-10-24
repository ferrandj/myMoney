// No inline scripts (strict Content-Security-Policy): behaviours are attached here.
document.addEventListener("submit", (event) => {
  // A button can carry its own question (several buttons submit the same form for different scopes).
  const message = (event.submitter && event.submitter.getAttribute("data-confirm")) || event.target.getAttribute("data-confirm");
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

function toggleNewContact(select) {
  const fieldset = select.closest("form").querySelector(".new-contact");
  if (!fieldset) return;
  fieldset.hidden = select.value !== "";
  fieldset.querySelectorAll("input[name=name]").forEach((input) => (input.required = select.value === ""));
}
document.querySelectorAll("select[data-toggle-contact]").forEach((select) => {
  toggleNewContact(select);
  select.addEventListener("change", () => toggleNewContact(select));
});

// "Qui est-ce ?" in a row: picking a contact already known hides the fields of a new one.
document.querySelectorAll("select[data-inline-contact]").forEach((select) => {
  const fields = select.closest("form").querySelector(".new-contact-fields");
  const sync = () => {
    const known = select.value !== "";
    fields.hidden = known;
    fields.querySelectorAll("input[name=name]").forEach((input) => (input.required = !known));
  };
  select.addEventListener("change", sync);
  sync();
});

// Drag and drop: dropping files on the zone sends them straight away.
const dropzone = document.getElementById("dropzone");
if (dropzone) {
  const input = dropzone.querySelector("input[type=file]");
  ["dragenter", "dragover"].forEach((name) =>
    dropzone.addEventListener(name, (event) => {
      event.preventDefault();
      dropzone.classList.add("dragging");
    }),
  );
  ["dragleave", "drop"].forEach((name) => dropzone.addEventListener(name, () => dropzone.classList.remove("dragging")));
  dropzone.addEventListener("drop", (event) => {
    event.preventDefault();
    if (!event.dataTransfer || !event.dataTransfer.files.length) return;
    input.files = event.dataTransfer.files;
    dropzone.requestSubmit();
  });
  input.addEventListener("change", () => input.files.length && dropzone.requestSubmit());
}

// Analyses: filters apply as soon as they change; table headers sort their column.
document.querySelectorAll("form[data-autosubmit]").forEach((form) => {
  form.addEventListener("change", () => form.requestSubmit());
  form.querySelectorAll("button[data-check]").forEach((button) =>
    button.addEventListener("click", () => {
      form.querySelectorAll("input[type=checkbox]").forEach((box) => (box.checked = button.dataset.check === "all"));
      form.requestSubmit();
    }),
  );
});
document.querySelectorAll("table[data-sortable]").forEach((table) => {
  const body = table.tBodies[0];
  table.tHead.querySelectorAll("th").forEach((th, column) =>
    th.addEventListener("click", () => {
      const direction = th.dataset.sortDir === "asc" ? "desc" : "asc";
      table.tHead.querySelectorAll("th").forEach((other) => delete other.dataset.sortDir);
      th.dataset.sortDir = direction;
      const key = (row) => {
        const cell = row.cells[column];
        const raw = cell.dataset.sort !== undefined ? cell.dataset.sort : cell.textContent.trim();
        const number = Number(raw);
        return raw !== "" && !Number.isNaN(number) ? number : raw.toLowerCase();
      };
      [...body.rows]
        .sort((a, b) => (key(a) > key(b) ? 1 : key(a) < key(b) ? -1 : 0) * (direction === "asc" ? 1 : -1))
        .forEach((row) => body.appendChild(row));
    }),
  );
});

// Appearance: the style and the "hide sensitive data" mode are kept on this device (the server never sees them).
const root = document.documentElement;
function store(key, value) {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch (error) {
    /* storage blocked: the choice only lasts for this page */
  }
}
function syncThemeUi() {
  const current = root.getAttribute("data-theme") || "auto";
  document.querySelectorAll("[data-set-theme]").forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.setTheme === current)));
  const color = getComputedStyle(root).getPropertyValue("--surface").trim();
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta && color) meta.setAttribute("content", color);
}
document.querySelectorAll("[data-set-theme]").forEach((button) =>
  button.addEventListener("click", () => {
    const id = button.dataset.setTheme;
    if (id === "auto") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", id);
    store("mymoney-theme", id === "auto" ? null : id);
    syncThemeUi();
  }),
);
syncThemeUi();
document.addEventListener("click", (event) => {
  const menu = document.getElementById("theme-menu");
  if (menu && menu.open && !menu.contains(event.target)) menu.open = false;
});
document.addEventListener("keydown", (event) => {
  const menu = document.getElementById("theme-menu");
  if (event.key === "Escape" && menu && menu.open) menu.open = false;
});

const privacyToggle = document.getElementById("privacy-toggle");
function syncPrivacyUi() {
  if (!privacyToggle) return;
  const hidden = root.classList.contains("privacy");
  privacyToggle.setAttribute("aria-pressed", String(hidden));
  privacyToggle.querySelector(".lbl").textContent = hidden ? "Afficher" : "Masquer";
  privacyToggle.querySelector(".ico").textContent = hidden ? "🙈" : "👁";
}
if (privacyToggle) {
  privacyToggle.addEventListener("click", () => {
    root.classList.toggle("privacy");
    store("mymoney-privacy", root.classList.contains("privacy") ? "1" : null);
    syncPrivacyUi();
  });
  syncPrivacyUi();
}

// Copy buttons: `data-copy` holds the text. The clipboard API needs HTTPS (or localhost): over plain http the NAS falls back
// to a hidden field and the old copy command.
async function copyText(text) {
  if (navigator.clipboard && window.isSecureContext) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch (error) {
      /* try the fallback */
    }
  }
  const field = document.createElement("textarea");
  field.value = text;
  field.setAttribute("readonly", "");
  field.className = "copy-field";
  document.body.appendChild(field);
  field.select();
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch (error) {
    ok = false;
  }
  field.remove();
  return ok;
}
document.addEventListener("click", async (event) => {
  const button = event.target.closest("[data-copy]");
  if (!button) return;
  const label = button.querySelector(".copy-label");
  const ok = await copyText(button.dataset.copy);
  if (label) {
    if (button.dataset.original === undefined) button.dataset.original = label.textContent;
    label.textContent = ok ? "Copié ✓" : "Échec";
    button.classList.toggle("done", ok);
    clearTimeout(button.copyTimer);
    button.copyTimer = setTimeout(() => {
      label.textContent = button.dataset.original;
      button.classList.remove("done");
    }, 1600);
  }
});

// Documents: ticking files enables the grouped download; a group's box ticks its files; folders open and close together.
const downloadForm = document.getElementById("download-form");
if (downloadForm) {
  const button = document.getElementById("download-selected");
  const count = document.getElementById("download-count");
  const boxes = () => [...downloadForm.querySelectorAll("input[name=ids]")];
  const refresh = () => {
    const n = boxes().filter((box) => box.checked).length;
    button.disabled = n === 0;
    count.textContent = n ? `(${n})` : "";
    downloadForm.querySelectorAll("[data-check-group]").forEach((all) => {
      const mine = [...all.closest("details").querySelectorAll("input[name=ids]")];
      all.checked = mine.length > 0 && mine.every((box) => box.checked);
    });
  };
  downloadForm.addEventListener("change", (event) => {
    if (event.target.matches("[data-check-group]")) {
      event.target.closest("details").querySelectorAll("input[name=ids]").forEach((box) => (box.checked = event.target.checked));
    }
    refresh();
  });
  // A click on a group's "tout" box must not fold the group.
  downloadForm.querySelectorAll(".doc-all").forEach((label) => label.addEventListener("click", (event) => event.stopPropagation()));
  downloadForm.querySelectorAll("[data-expand]").forEach((control) =>
    control.addEventListener("click", () => downloadForm.querySelectorAll("details").forEach((d) => (d.open = control.dataset.expand === "all"))),
  );
  refresh();
}
