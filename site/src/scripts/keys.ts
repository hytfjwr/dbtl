// Single-character keyboard shortcuts (/, t, 1-3) can be switched off here.
// Voice-control and switch users need a way to stop plain keys from firing
// commands; the choice is stored like the theme choice. Modifier combos
// (Cmd/Ctrl-K) and keys inside focused widgets always keep working.
const KEY = "dbtl-keys";
const root = document.documentElement;

/** True while single-character shortcuts fire. The default is on. */
export function shortcutsOn(): boolean {
  return root.getAttribute("data-keys") !== "off";
}

export function setShortcuts(on: boolean): void {
  if (on === shortcutsOn()) return;
  try {
    if (on) localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, "off");
  } catch {
    // The choice still applies for this page view.
  }
  if (on) root.removeAttribute("data-keys");
  else root.setAttribute("data-keys", "off");
  sync();
}

function sync(): void {
  const on = shortcutsOn();
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-keys-toggle]")) {
    button.setAttribute("aria-pressed", String(on));
    button.textContent = `keys:${on ? "on" : "off"}`;
  }
}

export function initKeysToggle(): void {
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-keys-toggle]")) {
    button.addEventListener("click", () => setShortcuts(!shortcutsOn()));
  }
  sync();
}
