/**
 * GitHub Trending API - 通过 Tauri invoke 调用 Rust 后端周热门缓存与 Claude CLI 解说命令
 *
 * Business Logic:
 *   首页展示 GitHub Trending Weekly Top 25，设置页管理 Claude CLI 解说配置。
 *
 * Code Logic:
 *   封装 list/config/default/update/test invoke，组件层只消费类型化 Promise。
 */

import { invoke } from './client';
import type {
  ClaudeCliTestResult,
  GithubTrendingConfig,
  GithubTrendingResponse,
} from '@/lib/types';

export interface GithubTrendingConfigUpdate {
  aiEnabled?: boolean;
  claudeCliPath?: string;
  claudeModel?: string;
  cacheTtlHours?: number;
}

export const githubTrendingApi = {
  /** 获取 GitHub Weekly Top 25（后端按天缓存） */
  list: () => invoke<GithubTrendingResponse>('list_github_trending_repos'),

  /** 获取 Claude CLI 解说配置 */
  getConfig: () => invoke<GithubTrendingConfig>('get_github_trending_config'),

  /** 获取 Claude CLI 解说默认配置 */
  getDefaultConfig: () => invoke<GithubTrendingConfig>('get_default_github_trending_config'),

  /** 更新 Claude CLI 解说配置 */
  updateConfig: (payload: GithubTrendingConfigUpdate) =>
    invoke<GithubTrendingConfig>(
      'update_github_trending_config',
      payload as unknown as Record<string, unknown>,
    ),

  /** 测试 Claude CLI 路径是否可用（只跑 --version） */
  testClaudeCli: (claudeCliPath?: string) =>
    invoke<ClaudeCliTestResult>('test_claude_cli', { claudeCliPath }),
};
