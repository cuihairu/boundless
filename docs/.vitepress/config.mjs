/**
 * VitePress configuration for the Boundless documentation site.
 *
 * Layout follows the target tree recorded in todo.md (BW-19):
 *   docs/guide/        install, FantasyDemo, standalone service, scripting, SDK
 *   docs/architecture/ process topology, protocol compatibility
 *   docs/reference/    configuration, CLI, script and SDK API
 *   docs/operations/   database, load testing, testing and release
 *   docs/public/       public static resources
 *
 * Locales are deliberately left out of `locales` so the site builds as a
 * single English tree at the site root. Adding the Chinese mirror is then a
 * matter of dropping `docs/zh/` in place and declaring a `zh` entry with
 * `link: '/zh/'`, as done in the reference repository.
 */

const siteTitle = "Boundless";

const nav = [
  { text: "Overview", link: "/" },
  { text: "Guide", link: "/guide/" },
  { text: "Architecture", link: "/architecture/" },
  { text: "Reference", link: "/reference/" },
  { text: "Operations", link: "/operations/" }
];

const guideSidebar = [
  { text: "Guide", link: "/guide/" },
  {
    text: "Installation",
    items: [
      { text: "Installing on three platforms", link: "/guide/install" }
    ]
  },
  {
    text: "Getting started",
    items: [
      { text: "FantasyDemo quick start", link: "/guide/fantasy-demo" }
    ]
  },
  {
    text: "Deployment",
    items: [
      { text: "Running a standalone server", link: "/guide/standalone-server" }
    ]
  },
  {
    text: "Scripting",
    items: [
      { text: "Entity scripting", link: "/guide/entity-authoring" }
    ]
  },
  {
    text: "Client",
    items: [
      { text: "Client SDK", link: "/guide/client-sdk" },
      { text: "Engine integration", link: "/guide/engine-integration" }
    ]
  }
];

const architectureSidebar = [
  { text: "Architecture", link: "/architecture/" },
  {
    text: "Topology",
    items: [
      { text: "Process topology", link: "/architecture/process-topology" }
    ]
  },
  {
    text: "Compatibility",
    items: [
      { text: "Compatibility matrix", link: "/architecture/compatibility-matrix" }
    ]
  }
];

const referenceSidebar = [
  { text: "Reference", link: "/reference/" },
  {
    text: "Configuration",
    items: [
      { text: "Configuration reference", link: "/reference/configuration" }
    ]
  },
  {
    text: "Command line",
    items: [
      { text: "CLI reference", link: "/reference/cli" }
    ]
  },
  {
    text: "API",
    items: [
      { text: "Script and SDK API", link: "/reference/api" }
    ]
  }
];

const operationsSidebar = [
  { text: "Operations", link: "/operations/" },
  {
    text: "Data",
    items: [
      { text: "Database migration and recovery", link: "/operations/database" }
    ]
  }
];

const sidebar = {
  "/guide/": guideSidebar,
  "/architecture/": architectureSidebar,
  "/reference/": referenceSidebar,
  "/operations/": operationsSidebar
};

export default {
  base: "/",
  lang: "en-US",
  title: siteTitle,
  description:
    "Boundless is a Rust re-implementation of the BigWorld MMO server. " +
    "Installation, scripting, deployment and operations documentation.",
  cleanUrls: false,
  // A broken internal link must fail the production build, not just the
  // link checker: this is the second gate behind scripts/check-docs-links.py.
  ignoreDeadLinks: false,
  lastUpdated: true,
  head: [
    [
      "meta",
      {
        name: "viewport",
        content: "width=device-width, initial-scale=1.0"
      }
    ]
  ],
  themeConfig: {
    siteTitle,
    search: {
      // Pure client-side local search index; no external search service.
      provider: "local"
    },
    nav,
    sidebar,
    outline: { level: [2, 3], label: "On this page" },
    docFooter: { prev: "Previous page", next: "Next page" },
    footer: {
      message: "Source-evidence driven documentation for the Boundless MMO server.",
      copyright: "Boundless project documentation"
    }
  },
  markdown: {
    theme: "github-dark",
    lineNumbers: true,
    container: {
      tipLabel: "Tip",
      warningLabel: "Warning",
      dangerLabel: "Danger",
      infoLabel: "Info",
      detailsLabel: "Details"
    }
  }
};