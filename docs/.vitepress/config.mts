import { existsSync } from "node:fs";
import { defineConfig } from "vitepress";
import { withFolderTree } from "vitepress-plugin-folder-tree";
import llmstxt from "vitepress-plugin-llms";
import { withMermaid } from "vitepress-plugin-mermaid";

const philosophyPath = new URL("../../PHILOSOPHY.md", import.meta.url);
if (!existsSync(philosophyPath)) {
  throw new Error("PHILOSOPHY.md is required by docs/philosophy.md");
}

const docsHostname = process.env.DOCS_HOSTNAME ?? "https://sacho.dev";

const config = defineConfig({
  base: "/",
  cleanUrls: true,
  description: "An opinionated changelog manager",
  head: [
    ["meta", { name: "theme-color", content: "#b23a24" }],
    ["link", { rel: "icon", type: "image/svg+xml", href: "/favicon.svg" }],
    [
      "link",
      {
        rel: "icon",
        type: "image/png",
        sizes: "32x32",
        href: "/favicon.png",
      },
    ],
    ["link", { rel: "apple-touch-icon", href: "/apple-touch-icon.png" }],
    ["meta", { property: "og:type", content: "website" }],
    [
      "meta",
      {
        property: "og:description",
        content: "An opinionated changelog manager",
      },
    ],
    ["meta", { property: "og:image", content: `${docsHostname}/og.png` }],
    ["meta", { name: "twitter:card", content: "summary_large_image" }],
  ],
  lang: "en-US",
  sitemap: {
    hostname: docsHostname,
  },
  srcExclude: ["DESIGN.md"],
  title: "Sacho",
  transformHead({ pageData }) {
    const title = pageData.title ? `${pageData.title} | Sacho` : "Sacho";
    return [["meta", { property: "og:title", content: title }]];
  },
  themeConfig: {
    logo: {
      light: "/logo.svg",
      dark: "/logo-dark.svg",
      alt: "A cinnabar seal bearing the character 史",
    },
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
  vite: {
    plugins: [llmstxt({ domain: docsHostname })],
  },
});

export default withMermaid(withFolderTree(config));
