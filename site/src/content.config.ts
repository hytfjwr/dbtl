import { defineCollection } from "astro:content";
import { glob } from "astro/loaders";
import { z } from "astro/zod";

// Hand-written pages; both frontmatter fields are required.
const docs = defineCollection({
  loader: glob({ base: "./src/content/docs", pattern: "**/*.md" }),
  schema: z.object({
    title: z.string().min(1),
    description: z.string().min(1),
  }),
});

// Pages generated from the source; no frontmatter, the title is the first H1.
const reference = defineCollection({
  loader: glob({ base: "../docs/reference", pattern: "*.md" }),
});

export const collections = { docs, reference };
