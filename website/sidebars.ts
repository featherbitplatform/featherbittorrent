import type {SidebarsConfig} from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docs: [
    'intro',
    {type: 'doc', id: 'install', label: 'Install'},
    {type: 'doc', id: 'desktop', label: 'Desktop app'},
    {type: 'doc', id: 'web-ui', label: 'Web UI'},
    {type: 'doc', id: 'server', label: 'Server'},
    {type: 'doc', id: 'reverse-proxy', label: 'Reverse proxy'},
    {type: 'doc', id: 'streaming', label: 'Streaming'},
    {type: 'doc', id: 'autostart', label: 'Autostart'},
  ],
};

export default sidebars;
