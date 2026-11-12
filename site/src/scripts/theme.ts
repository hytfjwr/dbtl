export type ThemeChoice = "light" | "dark" | "system";

const KEY = "dbtl-theme";

/** The stored choice. Storage can throw (blocked cookies, private modes): that reads as "system". */
export function readTheme(): ThemeChoice {
  try {
    const value = localStorage.getItem(KEY);
    if (value === "light" || value === "dark") return value;
  } catch {
    // fall through
  }
  return "system";
}

export function applyTheme(choice: ThemeChoice): void {
  const root = document.documentElement;
  if (choice === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", choice);
  try {
    if (choice === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, choice);
  } catch {
    // The theme still applies for this page view.
  }
}

function mark(choice: ThemeChoice): void {
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-theme-set]")) {
    button.setAttribute("aria-pressed", String(button.dataset.themeSet === choice));
  }
}

export function initThemeSwitch(): void {
  mark(readTheme());
  for (const button of document.querySelectorAll<HTMLButtonElement>("[data-theme-set]")) {
    button.addEventListener("click", () => {
      const choice = button.dataset.themeSet as ThemeChoice;
      applyTheme(choice);
      mark(choice);
    });
  }
}
