/**
 * Overlay - 区域截图选区页（独立于主 AppShell）
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在屏幕上框选区域截图，松手后裁剪写剪贴板，可直接粘贴到 Claude Code。
 *   采用 macOS 原生风格：选区窗口真透明，直接透出真实桌面（不用桌面截图作背景）；
 *   拖动框选时选区外盖半透明遮罩变暗、选区内保持清晰。ESC/右键/点空白取消。
 *   该页独立于 OnboardingGuard/Layout，由 Tauri 选区窗口直接加载
 *   （`/screenshot-overlay?display={i}`），每个显示器一个窗口实例。
 *
 * Code Logic（这个组件做什么）:
 *   - onMount：强制 html/body 背景透明，覆盖全局 reset.css 的 `body { background: var(--bg) }`
 *     （主题底色，浅色=#f5f4ed）。transparent 窗口需 html/body 全链路透明，否则会显示主题底色
 *     而非透出桌面（=白屏）；onUnmount 恢复原值。
 *   - mousedown 记起点，mousemove 实时更新选区矩形（四块遮罩挖洞 + 蓝色虚线边框），
 *     mouseup 确定选区 → invoke('crop_and_copy', { display, x, y, w, h, dpr }) 裁剪写剪贴板。
 *   - ESC/右键 → invoke('cancel_region_capture')。
 *   - 坐标用逻辑像素（CSS px），dpr 一起传给 Rust，Rust ×dpr 换算物理像素裁剪（xcap 帧即物理像素）。
 *   - React hooks 必须在所有 early return 之前（项目规则 20）。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@/api/client';
import styles from './Overlay.module.css';

/** 选区矩形（逻辑像素，相对当前窗口左上角） */
interface Selection {
  startX: number;
  startY: number;
  x: number;
  y: number;
  w: number;
  h: number;
}

/** URL query 中解析 display index；缺省 0（主屏） */
function parseDisplay(): number {
  const params = new URLSearchParams(window.location.search);
  const raw = params.get('display');
  const n = Number(raw);
  return Number.isFinite(n) && n >= 0 ? Math.floor(n) : 0;
}

export function Overlay() {
  // hooks 必须在任何 early return 之前调用（项目规则 20）
  const [selection, setSelection] = useState<Selection | null>(null);
  const displayRef = useRef<number>(parseDisplay());
  const draggingRef = useRef<boolean>(false);

  // 强制页面背景透明：transparent 窗口需 html/body 全链路透明，否则全局 reset.css 的
  // body { background: var(--bg) }（主题底色，浅色=#f5f4ed）会让窗口显示为白屏而非透出桌面。
  // onUnmount 恢复原值（窗口随截图结束销毁，恢复仅为卫生）。
  useEffect(() => {
    const html = document.documentElement;
    const body = document.body;
    const prevHtml = html.style.background;
    const prevBody = body.style.background;
    html.style.background = 'transparent';
    body.style.background = 'transparent';
    return () => {
      html.style.background = prevHtml;
      body.style.background = prevBody;
    };
  }, []);

  // 取消：ESC 触发
  const cancel = useCallback(async () => {
    try {
      await invoke('cancel_region_capture');
    } catch {
      // ignore
    }
  }, []);

  // ESC 键监听
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        void cancel();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [cancel]);

  // 鼠标按下：记录起点
  const onMouseDown = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    if (e.button !== 0) return; // 仅左键开始选区
    draggingRef.current = true;
    setSelection({
      startX: e.clientX,
      startY: e.clientY,
      x: e.clientX,
      y: e.clientY,
      w: 0,
      h: 0,
    });
  }, []);

  // 鼠标移动：实时更新选区
  const onMouseMove = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    if (!draggingRef.current) return;
    setSelection((prev) => {
      if (!prev) return prev;
      const x = Math.min(prev.startX, e.clientX);
      const y = Math.min(prev.startY, e.clientY);
      const w = Math.abs(e.clientX - prev.startX);
      const h = Math.abs(e.clientY - prev.startY);
      return { ...prev, x, y, w, h };
    });
  }, []);

  // 鼠标抬起：确定选区，裁剪写剪贴板（宽高 >=10 才算有效，对照 Python）
  const onMouseUp = useCallback(
    async (e: React.MouseEvent<HTMLDivElement>) => {
      if (e.button !== 0 || !draggingRef.current) return;
      draggingRef.current = false;
      setSelection((prev) => {
        if (prev && prev.w >= 10 && prev.h >= 10) {
          // 异步裁剪写剪贴板，不阻塞渲染
          void invoke('crop_and_copy', {
            display: displayRef.current,
            x: Math.round(prev.x),
            y: Math.round(prev.y),
            w: Math.round(prev.w),
            h: Math.round(prev.h),
            dpr: window.devicePixelRatio,
          }).catch(() => {
            // 失败静默，由 Rust 端 emit 取消或前端兜底
          });
        } else {
          // 选区过小视为取消（对照 Python mouseRelease 无效选区 → cancelled）
          void cancel();
        }
        return prev;
      });
    },
    [cancel],
  );

  // 右键取消
  const onContextMenu = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      e.preventDefault();
      void cancel();
    },
    [cancel],
  );

  return (
    <div
      className={styles.overlay}
      onMouseDown={onMouseDown}
      onMouseMove={onMouseMove}
      onMouseUp={onMouseUp}
      onContextMenu={onContextMenu}
    >
      {/* 选区遮罩 + 高亮矩形（鼠标拖动时显示） */}
      {selection && selection.w > 0 && selection.h > 0 && (
        <>
          {/* 四块半透明遮罩，盖住选区外的区域（挖洞效果） */}
          <div
            className={styles.mask}
            style={{ left: 0, top: 0, right: 0, bottom: `calc(100% - ${selection.y}px)` }}
          />
          <div
            className={styles.mask}
            style={{ left: 0, top: `${selection.y + selection.h}px`, right: 0, bottom: 0 }}
          />
          <div
            className={styles.mask}
            style={{ left: 0, top: `${selection.y}px`, width: `${selection.x}px`, height: `${selection.h}px` }}
          />
          <div
            className={styles.mask}
            style={{
              left: `${selection.x + selection.w}px`,
              top: `${selection.y}px`,
              right: 0,
              height: `${selection.h}px`,
            }}
          />
          {/* 高亮矩形边框 */}
          <div
            className={styles.selection}
            style={{
              left: `${selection.x}px`,
              top: `${selection.y}px`,
              width: `${selection.w}px`,
              height: `${selection.h}px`,
            }}
          />
        </>
      )}
    </div>
  );
}
