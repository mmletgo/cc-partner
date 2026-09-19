import 'nav.dart';

export 'nav.dart';

/// Self-drawn workbench; never a WebView of `/mobile`.
const kEmbedsMobileSpa = false;

const kDeferredPanels = <String>['notes'];

bool isFirstVersionPanel(String panel) => isWorkbenchPanel(panel);
