import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'featherbittorrent',
  tagline: 'A BitTorrent client for the terminal, the browser and the desktop, built on a from-scratch Rust library.',
  favicon: 'img/featherbit-favicon.png',

  future: {
    v4: true,
  },

  url: 'https://featherbitplatform.github.io',
  baseUrl: '/featherbittorrent/',
  organizationName: 'featherbitplatform',
  projectName: 'featherbittorrent',
  trailingSlash: false,

  onBrokenLinks: 'throw',
  onBrokenAnchors: 'throw',

  markdown: {
    // The user docs are plain CommonMark that GitHub renders too; do not read them as MDX.
    format: 'detect',
  },

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          // The user docs are read in place from the repository's docs/ folder (also linked from
          // README.md and CLAUDE.md); the design notes and example folders are not user docs.
          path: '../docs',
          exclude: ['superpowers/**', 'examples/**'],
          routeBasePath: '/',
          sidebarPath: './sidebars.ts',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/featherbit-mark.png',
    colorMode: {
      defaultMode: 'dark',
      respectPrefersColorScheme: false,
    },
    navbar: {
      title: 'featherbittorrent',
      logo: {
        alt: 'featherbit logo',
        src: 'img/featherbit-mark.png',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docs',
          position: 'left',
          label: 'Docs',
        },
        {to: '/changelog', label: 'Changelog', position: 'left'},
        {
          href: 'https://github.com/featherbitplatform/featherbittorrent',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [],
      copyright: `Apache-2.0 License · featherbit ${new Date().getFullYear()}`,
    },
    prism: {
      theme: prismThemes.oneLight,
      darkTheme: prismThemes.oneDark,
      additionalLanguages: ['toml', 'bash'],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
