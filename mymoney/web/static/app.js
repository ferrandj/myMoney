// No inline scripts (strict Content-Security-Policy): behaviours are attached here.
document.addEventListener("submit", (event) => {
  const message = event.target.getAttribute("data-confirm");
  if (message && !window.confirm(message)) event.preventDefault();
});

function toggleNewAccount(select) {
  const fieldset = select.closest("form").querySelector(".new-account");
  if (fieldset) fieldset.hidden = select.value !== "new";
}
document.querySelectorAll("select[data-toggle-new]").forEach((select) => {
  toggleNewAccount(select);
  select.addEventListener("change", () => toggleNewAccount(select));
});
