/// First-version self-drawn workbench panels. Not a WebView of `/mobile`.
const kFirstVersionPanels = <String>[
  'projects',
  'attention',
  'terminal',
  'files',
  'worktrees',
  'git',
  'transfer',
  'provider',
  'settings',
];

const kDeferredPanels = <String>['automation', 'browser', 'notes'];

bool isFirstVersionPanel(String panel) => kFirstVersionPanels.contains(panel);
