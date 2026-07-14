/**
 * ScreenshotToolbar - 截图编辑工具条
 *
 * Business Logic: 用户框选后进入编辑模式，用工具条选标注工具（矩形/箭头）+ 颜色，撤销最后一个标注，
 *   确认合成写剪贴板或取消。布局微信截图风格。
 *
 * Code Logic: 受控组件——当前工具/颜色由父组件管理，本组件只负责展示 + 回调；
 *   title 文案走 common:screenshot i18n。
 */

import { useTranslation } from 'react-i18next';
import styles from './ScreenshotToolbar.module.css';

export type ToolKind = 'rect' | 'arrow';

/** 预设 6 色板（红/黄/绿/蓝/白/黑），固定线宽由 canvas 绘制层控制 */
// COLORS 允许从组件文件导出常量（与 Toolbar 内聚），抑制 react-refresh only-export-components。
// eslint-disable-next-line react-refresh/only-export-components
export const COLORS = ['#FF3B30', '#FFCC00', '#34C759', '#007AFF', '#FFFFFF', '#000000'];

/** 颜色 i18n key 后缀（与 COLORS 同序） */
const COLOR_I18N_KEYS = [
  'colorRed',
  'colorYellow',
  'colorGreen',
  'colorBlue',
  'colorWhite',
  'colorBlack',
] as const;

interface ScreenshotToolbarProps {
  tool: ToolKind;
  onToolChange: (tool: ToolKind) => void;
  color: string;
  onColorChange: (color: string) => void;
  onUndo: () => void;
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   截图编辑需要可访问的工具/颜色/确认操作入口。
 *
 * Code Logic（这个组件做什么）:
 *   渲染工具条按钮并在 title 上使用 t('common:screenshot.*')。
 */
export function ScreenshotToolbar({
  tool,
  onToolChange,
  color,
  onColorChange,
  onUndo,
  onConfirm,
  onCancel,
}: ScreenshotToolbarProps) {
  const { t } = useTranslation(['common']);

  return (
    <div className={styles.toolbar} role="toolbar">
      <button
        type="button"
        className={tool === 'rect' ? styles.toolBtnActive : styles.toolBtn}
        onClick={() => onToolChange('rect')}
        title={t('common:screenshot.toolRect')}
      >
        ▭
      </button>
      <button
        type="button"
        className={tool === 'arrow' ? styles.toolBtnActive : styles.toolBtn}
        onClick={() => onToolChange('arrow')}
        title={t('common:screenshot.toolArrow')}
      >
        →
      </button>
      <span className={styles.divider} />
      <div className={styles.colors}>
        {COLORS.map((c, i) => (
          <button
            key={c}
            type="button"
            className={color === c ? styles.colorBtnActive : styles.colorBtn}
            style={{ backgroundColor: c }}
            onClick={() => onColorChange(c)}
            title={t(`common:screenshot.${COLOR_I18N_KEYS[i]}`)}
          />
        ))}
      </div>
      <span className={styles.divider} />
      <button
        type="button"
        className={styles.toolBtn}
        onClick={onUndo}
        title={t('common:screenshot.undo')}
      >
        ↶
      </button>
      <button
        type="button"
        className={styles.confirmBtn}
        onClick={onConfirm}
        title={t('common:screenshot.confirm')}
      >
        ✓
      </button>
      <button
        type="button"
        className={styles.cancelBtn}
        onClick={onCancel}
        title={t('common:screenshot.cancel')}
      >
        ✕
      </button>
    </div>
  );
}
