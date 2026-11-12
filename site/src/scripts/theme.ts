import { transition } from "./motion.ts";

export type ThemeChoice = "light" | "dark" | "system";

const KEY = "dbtl-theme";
const root = document.documentElement;
const listeners: Array<(choice: ThemeChoice) => void> = [];

/**
 * The active choice, read from `data-theme` on <html>. The inline script in the
 * page head applied the stored value before first paint; when storage is
 * unavailable it leaves the attribute off, which reads as "system".
 */
export function readTheme(): ThemeChoice {
  const attr = root.getAttribute("data-theme");
  return attr === "light" || attr === "dark" ? attr : "system";
}

function apply(choice: ThemeChoice): void {
  if (choice === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", choice);
}

function store(choice: ThemeChoice): void {
  try {
    if (choice === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, choice);
  } catch {
    // The theme still applies for this page view.
  }
}

function sync(): void {
  const choice = readTheme();
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-theme-set]")) {
    button.setAttribute("aria-pressed", String(button.dataset.themeSet === choice));
  }
  for (const listener of listeners) listener(choice);
}

export function setTheme(choice: ThemeChoice): void {
  if (choice === readTheme()) return;
  store(choice);
  void transition("theme", () => {
    apply(choice);
    sync();
  });
}

export function cycleTheme(): void {
  const order: ThemeChoice[] = ["system", "light", "dark"];
  setTheme(order[(order.indexOf(readTheme()) + 1) % order.length]!);
}

/** Called with the current choice now and after every change. */
export function onThemeChange(listener: (choice: ThemeChoice) => void): void {
  listeners.push(listener);
  listener(readTheme());
}

export function initThemeSwitch(): void {
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-theme-set]")) {
    button.addEventListener("click", () => setTheme(button.dataset.themeSet as ThemeChoice));
  }
  sync();
}
