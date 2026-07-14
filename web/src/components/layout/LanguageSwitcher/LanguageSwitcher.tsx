/**
 * LanguageSwitcher（中英文语言切换器）
 *
 * Business Logic（为什么需要这个组件）:
 *   Sidebar 底部提供全局语言切换入口，让用户在任意页面随手切换中英文。
 *
 * Code Logic（这个组件做什么）:
 *   - useLanguage() 获取 language / setLanguage
 *   - 渲染紧凑的 EN / 中 两段式切换器，高亮当前语言
 *   - 可见标签保留语言代码缩写；group aria-label 走 i18n
 */
import { useTranslation } from 'react-i18next';
import { useLanguage } from '../../../hooks/useLanguage';
import type { AppLanguage } from '../../../i18n';
import styles from './LanguageSwitcher.module.css';

export interface LanguageSwitcherProps {
  /** 透传的自定义 className */
  className?: string;
}

const OPTIONS: ReadonlyArray<{ value: AppLanguage; label: string }> = [
  { value: 'en', label: 'EN' },
  { value: 'zh', label: '中' },
];

/**
 * Business Logic（为什么需要这个组件）:
 *   用户需要在侧栏一键切换界面语言。
 *
 * Code Logic（这个组件做什么）:
 *   读取当前语言并渲染两个 toggle 按钮，aria-label 使用 common 命名空间。
 */
export function LanguageSwitcher({ className }: LanguageSwitcherProps) {
  const { t } = useTranslation(['common']);
  const { language, setLanguage } = useLanguage();
  const cls = [styles.switcher, className].filter(Boolean).join(' ');

  return (
    <div className={cls} role="group" aria-label={t('common:languageSwitcher.label')}>
      {OPTIONS.map((opt) => {
        const active = language === opt.value;
        return (
          <button
            key={opt.value}
            type="button"
            className={active ? styles.optionActive : styles.option}
            onClick={() => setLanguage(opt.value)}
            aria-pressed={active}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}
