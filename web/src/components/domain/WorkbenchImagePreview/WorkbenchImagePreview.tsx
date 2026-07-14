/**
 * WorkbenchImagePreview 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   Workbench 文件查看器需要安全展示图片文件内容，同时让用户确认 MIME 与尺寸等只读元信息。
 *
 * Code Logic（这个组件做什么）:
 *   接收后端生成的图片 data URL 和文件名，渲染图片预览，并在存在尺寸信息时展示本地化元数据行。
 */

import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import type { WorkbenchImagePreview as WorkbenchImagePreviewDto } from '@/lib/types';
import styles from './WorkbenchImagePreview.module.css';

export interface WorkbenchImagePreviewProps {
  preview: WorkbenchImagePreviewDto;
  name: string;
}

/**
 * 渲染 Workbench 图片只读预览
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在工作台打开图片时只需要查看内容和基础属性，不能出现编辑或写入动作。
 *
 * Code Logic（这个组件做什么）:
 *   使用 preview.dataUrl 作为 img src，文件名作为 alt；MIME 始终展示，宽高同时存在时追加尺寸元数据。
 */
export function WorkbenchImagePreview(props: WorkbenchImagePreviewProps): ReactElement {
  const { preview, name } = props;
  const { t } = useTranslation(['workbench']);
  const hasDimensions = preview.width !== null && preview.height !== null;

  return (
    <section className={styles.shell}>
      <dl className={styles.metaLine}>
        <div className={styles.metaItem}>
          <dt>{t('workbench:filePreviews.mime')}</dt>
          <dd>{preview.mime}</dd>
        </div>
        {hasDimensions ? (
          <div className={styles.metaItem}>
            <dt>{t('workbench:filePreviews.dimensions')}</dt>
            <dd>
              {`${preview.width} × ${preview.height}`}
            </dd>
          </div>
        ) : null}
      </dl>
      <div className={styles.imageSurface}>
        <img className={styles.image} src={preview.dataUrl} alt={name} />
      </div>
    </section>
  );
}
