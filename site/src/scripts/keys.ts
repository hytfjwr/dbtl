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

// The full aria-keyshortcuts list, kept so it can be restored — while keys
// are off only the one-character tokens drop, the modifier combos still work.
const fullShortcuts = new WeakMap<Element, string>();

function sync(): void {
  const on = shortcutsOn();
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-keys-toggle]")) {
    button.setAttribute("aria-pressed", String(on));
    button.textContent = `keys:${on ? "on" : "off"}`;
  }
  for (const el of document.querySelectorAll("[aria-keyshortcuts]")) {
    if (!fullShortcuts.has(el)) fullShortcuts.set(el, el.getAttribute("aria-keyshortcuts")!);
    const full = fullShortcuts.get(el)!;
    const kept = on
      ? full
      : full
          .split(/\s+/)
          .filter((key) => key.length > 1)
          .join(" ");
    if (kept) el.setAttribute("aria-keyshortcuts", kept);
    else el.removeAttribute("aria-keyshortcuts");
  }
}

export function initKeysToggle(): void {
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-keys-toggle]")) {
    button.addEventListener("click", () => setShortcuts(!shortcutsOn()));
  }
  sync();
}
