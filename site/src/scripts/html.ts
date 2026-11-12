// Escape text for use in HTML, including attribute values.
const ENTITIES: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#39;",
};

export const esc = (text: string): string => String(text).replace(/[&<>"']/g, (c) => ENTITIES[c]!);
