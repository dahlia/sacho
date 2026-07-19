import { existsSync } from "node:fs";
import { defineConfig } from "vitepress";
import { withFolderTree } from "vitepress-plugin-folder-tree";
import { withMermaid } from "vitepress-plugin-mermaid";

const philosophyPath = new URL("../../PHILOSOPHY.md", import.meta.url);
if (!existsSync(philosophyPath)) {
  throw new Error("PHILOSOPHY.md is required by docs/philosophy.md");
}

const config = defineConfig({
  base: "/",
  cleanUrls: true,
  description: "An opinionated changelog manager",
  head: [["meta", { name: "theme-color", content: "#7c3aed" }]],
  lang: "en-US",
  title: "Sacho",
  themeConfig: {
    editLink: {
      pattern: ({ filePath }) => {
        const sourcePath =
          filePath === "philosophy.md" ? "PHILOSOPHY.md" : `docs/${filePath}`;
        return `https://github.com/dahlia/sacho/edit/main/${sourcePath}`;
      },
      text: "Edit this page on GitHub",
    },
    footer: {
      message: "Released under the GPL-3.0-only license.",
      copyright: "Copyright © 2026 Hong Minhee",
    },
    nav: [
      { text: "Why Sacho?", link: "/why-sacho" },
      { text: "Guide", link: "/guide/getting-started" },
      { text: "Concepts", link: "/concepts/" },
      { text: "Reference", link: "/reference/commands" },
    ],
    search: {
      provider: "local",
    },
    sidebar: [
      {
        text: "Start here",
        items: [
          { text: "Why Sacho?", link: "/why-sacho" },
          { text: "Philosophy", link: "/philosophy" },
        ],
      },
      {
        text: "Guide",
        items: [
          { text: "Getting started", link: "/guide/getting-started" },
          { text: "Everyday workflow", link: "/guide/everyday-workflow" },
          { text: "Making releases", link: "/guide/releases" },
          { text: "CI and hooks", link: "/guide/ci-and-hooks" },
          { text: "Version control", link: "/guide/version-control" },
        ],
      },
      {
        text: "Concepts",
        items: [
          { text: "How Sacho fits together", link: "/concepts/" },
          { text: "Fragments", link: "/concepts/fragments" },
          {
            text: "The changelog lifecycle",
            link: "/concepts/changelog-lifecycle",
          },
          { text: "Sections", link: "/concepts/sections" },
        ],
      },
      {
        text: "Reference",
        items: [
          { text: "Commands", link: "/reference/commands" },
          { text: "Configuration", link: "/reference/configuration" },
          { text: "Troubleshooting", link: "/troubleshooting" },
        ],
      },
    ],
    socialLinks: [
      { icon: "github", link: "https://github.com/dahlia/sacho" },
    ],
  },
});

export default withMermaid(withFolderTree(config));
