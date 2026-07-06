import type { LanFirewallDependencyStatus } from './types';
import {
  buildLanFirewallCommandPreview,
  lanFirewallStatusTone,
  platformLabelKey,
} from './lanFirewallDependency';

/**
 * Business Logic（为什么需要这个函数）:
 *   局域网防火墙依赖测试需要快速构造后端 DTO，覆盖不同系统的展示映射。
 *
 * Code Logic（这个函数做什么）:
 *   合并默认 macOS 样本和调用方 patch，返回完整 LanFirewallDependencyStatus。
 */
function dependency(
  patch: Partial<LanFirewallDependencyStatus>,
): LanFirewallDependencyStatus {
  return {
    platform: 'macos',
    platformLabel: 'macOS',
    lanIp: '192.168.1.12',
    httpPort: 62116,
    mdnsPort: 5353,
    appPath: '/Applications/cc-partner.app/Contents/MacOS/cc-partner',
    checks: [
      { id: 'httpListener', ok: true, detail: 'TCP 62116' },
      { id: 'lanIp', ok: true, detail: '192.168.1.12' },
      { id: 'tcpFirewall', ok: true, detail: 'TCP 62116' },
      { id: 'mdnsFirewall', ok: true, detail: 'UDP 5353' },
    ],
    guidance: {
      summaryKey: 'settings:lanFirewall.guidance.macos.summary',
      steps: [],
      commands: [
        {
          labelKey: 'settings:lanFirewall.guidance.macos.allowAppCommand',
          command:
            'sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add "/Applications/cc-partner.app/Contents/MacOS/cc-partner"',
        },
      ],
    },
    ...patch,
  };
}

if (lanFirewallStatusTone(dependency({ httpPort: 62116, lanIp: '192.168.1.12' })) !== 'success') {
  throw new Error('all LAN firewall checks passing should use success tone');
}

if (
  lanFirewallStatusTone(
    dependency({
      checks: [
        { id: 'httpListener', ok: true, detail: 'TCP 62116' },
        { id: 'lanIp', ok: true, detail: '192.168.1.12' },
        { id: 'tcpFirewall', ok: false, detail: 'TCP 62116' },
        { id: 'mdnsFirewall', ok: true, detail: 'UDP 5353' },
      ],
    }),
  ) !== 'danger'
) {
  throw new Error('blocked TCP firewall check should use danger tone');
}

if (lanFirewallStatusTone(dependency({ httpPort: 0 })) !== 'danger') {
  throw new Error('missing HTTP listener should use danger tone');
}

if (lanFirewallStatusTone(dependency({ lanIp: null })) !== 'danger') {
  throw new Error('missing LAN IP should use danger tone');
}

if (platformLabelKey('windows') !== 'settings:lanFirewall.platform.windows') {
  throw new Error('windows platform should map to settings platform i18n key');
}

const windowsPreview = buildLanFirewallCommandPreview(
  dependency({
    platform: 'windows',
    platformLabel: 'Windows',
    guidance: {
      summaryKey: 'settings:lanFirewall.guidance.windows.summary',
      steps: [],
      commands: [
        {
          labelKey: 'settings:lanFirewall.guidance.windows.tcpCommand',
          command:
            'netsh advfirewall firewall add rule name="cc-partner P2P TCP 62116" dir=in action=allow protocol=TCP localport=62116',
        },
        {
          labelKey: 'settings:lanFirewall.guidance.windows.mdnsCommand',
          command:
            'netsh advfirewall firewall add rule name="cc-partner mDNS UDP 5353" dir=in action=allow protocol=UDP localport=5353',
        },
      ],
    },
  }),
);
if (!windowsPreview.includes('protocol=TCP localport=62116')) {
  throw new Error('windows command preview should include TCP port rule');
}
if (!windowsPreview.includes('protocol=UDP localport=5353')) {
  throw new Error('windows command preview should include UDP 5353 rule');
}

const linuxPreview = buildLanFirewallCommandPreview(
  dependency({
    platform: 'linux',
    platformLabel: 'Linux',
    guidance: {
      summaryKey: 'settings:lanFirewall.guidance.linux.summary',
      steps: [],
      commands: [
        { labelKey: 'settings:lanFirewall.guidance.linux.ufwTcp', command: 'sudo ufw allow 62116/tcp' },
        { labelKey: 'settings:lanFirewall.guidance.linux.ufwMdns', command: 'sudo ufw allow 5353/udp' },
        {
          labelKey: 'settings:lanFirewall.guidance.linux.firewalldTcp',
          command: 'sudo firewall-cmd --permanent --add-port=62116/tcp',
        },
      ],
    },
  }),
);
if (!linuxPreview.includes('sudo ufw allow 62116/tcp')) {
  throw new Error('linux command preview should include ufw TCP rule');
}
if (!linuxPreview.includes('sudo firewall-cmd --permanent --add-port=62116/tcp')) {
  throw new Error('linux command preview should include firewalld TCP rule');
}

const macPreview = buildLanFirewallCommandPreview(dependency({}));
if (!macPreview.includes('socketfilterfw --add')) {
  throw new Error('macOS command preview should include socketfilterfw allow-app command');
}

console.log('lanFirewallDependency.test.ts passed');
