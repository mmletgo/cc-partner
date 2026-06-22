/**
 * Overlay - 区域截图选区页（独立于主 AppShell）
 *
 * Business Logic: 微信截图风格。三态：
 *   - idle：整屏半透明遮罩
 *   - selecting：拖拽框选（四块遮罩 + 蓝虚线边框）
 *   - editing：选区确定，canvas 画桌面快照 + 标注，工具条选矩形/箭头/颜色/撤销/确认/取消
 *   确认 → canvas.toDataURL 合成 → save_clipboard_image 写剪贴板。ESC/取消 → 关闭。
 *
 * Code Logic: 状态机 + selectionRef（mouseup 读最新选区）+ hiding（进编辑/确认前隐藏遮罩与边框，
 *   让 Rust 抓纯桌面 / canvas 合成不含遮罩）。hooks 在所有 early return 之前（项目规则 20）。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@/api/client';
import { useAnnotationCanvas, type Annotation } from './useAnnotationCanvas';
import { ScreenshotToolbar, COLORS, type ToolKind } from './ScreenshotToolbar';
import styles from './Overlay.module.css';

type Mode = 'idle' | 'selecting' | 'editing';

interface Selection {
  startX: number;
  startY: number;
  x: number;
  y: number;
  w: number;
  h: number;
}

function parseDisplay(): number {
  const params = new URLSearchParams(window.location.search);
  const raw = params.get('display');
  const n = Number(raw);
  return Number.isFinite(n) && n >= 0 ? Math.floor(n) : 0;
}

export function Overlay() {
  const [mode, setMode] = useState<Mode>('idle');
  const [selection, setSelection] = useState<Selection | null>(null);
  const [hiding, setHiding] = useState(false);
  // editing 状态
  const [snapshot, setSnapshot] = useState<HTMLImageElement | null>(null);
  const [annotations, setAnnotations] = useState<Annotation[]>([]);
  const [draft, setDraft] = useState<Annotation | null>(null); // 正在画的标注预览
  const [tool, setTool] = useState<ToolKind>('rect');
  const [color, setColor] = useState<string>(COLORS[0]);
  const [busy, setBusy] = useState(false); // 抓快照/写剪贴板进行中，禁重复触发

  const displayRef = useRef<number>(parseDisplay());
  const draggingRef = useRef<boolean>(false);
  const selectionRef = useRef<Selection | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  // DPR 在 overlay 窗口生命周期内为常量，用 lazy-init state 取代 ref（避免 render 阶段读 ref.current 触发 react-hooks/refs）
  const [dpr] = useState<number>(() => window.devicePixelRatio || 1);

  // 强制 html/body 透明（覆盖全局 reset.css 的 var(--bg)，防 transparent 窗口白屏）
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

  const cancel = useCallback(async () => {
    try {
      await invoke('cancel_region_capture');
    } catch {
      // ignore
    }
  }, []);

  // ESC 取消
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') void cancel();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [cancel]);

  // canvas 重绘（editing 时快照 + 已有标注 + 草稿预览）
  useAnnotationCanvas(
    canvasRef,
    snapshot,
    draft ? [...annotations, draft] : annotations,
    selection?.w ?? 0,
    selection?.h ?? 0,
    dpr,
  );

  // === selecting 阶段：框选 ===
  const onMouseDown = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (mode !== 'idle' || e.button !== 0) return;
      draggingRef.current = true;
      const sel: Selection = { startX: e.clientX, startY: e.clientY, x: e.clientX, y: e.clientY, w: 0, h: 0 };
      selectionRef.current = sel;
      setSelection(sel);
      setMode('selecting');
    },
    [mode],
  );

  const onMouseMoveSelect = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (!draggingRef.current) return;
      setSelection((prev) => {
        if (!prev) return prev;
        const next: Selection = {
          ...prev,
          x: Math.min(prev.startX, e.clientX),
          y: Math.min(prev.startY, e.clientY),
          w: Math.abs(e.clientX - prev.startX),
          h: Math.abs(e.clientY - prev.startY),
        };
        selectionRef.current = next;
        return next;
      });
    },
    [],
  );

  // mouseup 有效选区 → 进 editing（抓快照）
  const enterEditing = useCallback(async () => {
    const sel = selectionRef.current;
    if (!sel || sel.w < 10 || sel.h < 10) {
      void cancel();
      return;
    }
    setBusy(true);
    setHiding(true); // 隐藏遮罩/边框，Rust 抓纯桌面
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    try {
      const dataUrl = await invoke<string>('get_region_snapshot', {
        display: displayRef.current,
        x: Math.round(sel.x),
        y: Math.round(sel.y),
        w: Math.round(sel.w),
        h: Math.round(sel.h),
        dpr,
      });
      const img = new Image();
      await new Promise<void>((resolve, reject) => {
        img.onload = () => resolve();
        img.onerror = () => reject(new Error('快照加载失败'));
        img.src = dataUrl;
      });
      setSnapshot(img);
      setHiding(false);
      setMode('editing');
    } catch {
      setHiding(false);
      setMode('idle'); // 快照失败回 idle 让用户重选
    } finally {
      setBusy(false);
    }
  }, [cancel, dpr]);

  const onMouseUpSelect = useCallback(() => {
    if (!draggingRef.current) return;
    draggingRef.current = false;
    void enterEditing();
  }, [enterEditing]);

  // === editing 阶段：在 canvas 上画标注 ===
  // 用 pointer 事件 + setPointerCapture：拖拽时即使鼠标移出 canvas 边界（到选区外遮罩区域），
  // pointermove/pointerup 仍路由回 canvas，pointerup 必触发——避免 mouseup 丢失导致
  // draggingRef 残留 true、draft 不入栈也不清空（review Minor 1）。
  const onCanvasPointerDown = useCallback(
    (e: React.PointerEvent<HTMLCanvasElement>) => {
      if (!snapshot) return;
      // 捕获指针，保证后续 pointermove/pointerup 都送达 canvas（哪怕移出边界）
      try {
        e.currentTarget.setPointerCapture(e.pointerId);
      } catch {
        // 某些环境/已释放时可能抛异常，忽略——最坏退化为原 mouse 行为
      }
      const rect = e.currentTarget.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const y = e.clientY - rect.top;
      draggingRef.current = true;
      setDraft({ tool, color, x1: x, y1: y, x2: x, y2: y });
    },
    [tool, color, snapshot],
  );

  const onCanvasPointerMove = useCallback(
    (e: React.PointerEvent<HTMLCanvasElement>) => {
      if (!draggingRef.current || !draft) return;
      const rect = e.currentTarget.getBoundingClientRect();
      setDraft({ ...draft, x2: e.clientX - rect.left, y2: e.clientY - rect.top });
    },
    [draft],
  );

  const onCanvasPointerUp = useCallback(() => {
    if (!draggingRef.current || !draft) return;
    draggingRef.current = false;
    // 仅保留尺寸非零的标注
    if (Math.abs(draft.x2 - draft.x1) >= 2 || Math.abs(draft.y2 - draft.y1) >= 2) {
      setAnnotations((prev) => [...prev, draft]);
    }
    setDraft(null);
  }, [draft]);

  // 工具条回调
  const undo = useCallback(() => setAnnotations((prev) => prev.slice(0, -1)), []);

  const confirm = useCallback(async () => {
    if (busy) return;
    setBusy(true);
    const canvas = canvasRef.current;
    try {
      const dataUrl = canvas?.toDataURL('image/png');
      if (!dataUrl) throw new Error('canvas 合成失败');
      await invoke('save_clipboard_image', { dataUrl });
      // 成功后 Rust 已关 overlay；失败抛出
    } catch {
      setBusy(false); // 保留 editing 让用户重试
    }
  }, [busy]);

  const showSelection = selection && selection.w > 0 && selection.h > 0;

  // editing 工具条位置：默认选区下方居中，贴近下边则翻到上方
  const tbH = 44;
  const winH = typeof window !== 'undefined' ? window.innerHeight : 9999;
  const tbBelow = selection && selection.y + selection.h + 8 + tbH <= winH;
  const toolbarStyle: React.CSSProperties = selection
    ? {
        left: selection.x,
        top: tbBelow ? selection.y + selection.h + 8 : Math.max(0, selection.y - tbH - 8),
        width: selection.w,
      }
    : {};

  return (
    <div
      className={styles.overlay}
      onMouseDown={mode === 'idle' ? onMouseDown : undefined}
      onMouseMove={mode === 'selecting' ? onMouseMoveSelect : undefined}
      onMouseUp={mode === 'selecting' ? onMouseUpSelect : undefined}
      onContextMenu={(e) => {
        e.preventDefault();
        void cancel();
      }}
    >
      {/* hiding=true 时只透出桌面（进编辑/确认时不显示遮罩/边框/canvas） */}
      {!hiding &&
        (mode === 'editing' && snapshot ? (
          <>
            {/* 此分支仅在 editing 且有快照时进入（用户已 enterEditing，selection 必非空），取一次收敛非空断言 */}
            {(() => {
              const sel = selection!;
              return (
                <>
                  {/* 选区外四块遮罩 */}
                  <div className={styles.mask} style={{ left: 0, top: 0, right: 0, bottom: `calc(100% - ${sel.y}px)` }} />
                  <div className={styles.mask} style={{ left: 0, top: `${sel.y + sel.h}px`, right: 0, bottom: 0 }} />
                  <div className={styles.mask} style={{ left: 0, top: `${sel.y}px`, width: `${sel.x}px`, height: `${sel.h}px` }} />
                  <div className={styles.mask} style={{ left: `${sel.x + sel.w}px`, top: `${sel.y}px`, right: 0, height: `${sel.h}px` }} />
                  {/* canvas：选区内，画快照 + 标注 */}
                  <canvas
                    ref={canvasRef}
                    className={styles.canvas}
                    style={{ left: sel.x, top: sel.y, width: sel.w, height: sel.h }}
                    onPointerDown={onCanvasPointerDown}
                    onPointerMove={onCanvasPointerMove}
                    onPointerUp={onCanvasPointerUp}
                  />
                  {/* 工具条 */}
                  <div className={styles.toolbarWrap} style={toolbarStyle}>
                    <ScreenshotToolbar
                      tool={tool}
                      onToolChange={setTool}
                      color={color}
                      onColorChange={setColor}
                      onUndo={undo}
                      onConfirm={confirm}
                      onCancel={() => void cancel()}
                    />
                  </div>
                </>
              );
            })()}
          </>
        ) : showSelection ? (
          <>
            {/* showSelection 已保证 selection 非空（w>0 && h>0），取一次收敛非空断言 */}
            {(() => {
              const sel = selection!;
              return (
                <>
                  {/* selecting：四块遮罩 + 蓝虚线边框 */}
                  <div className={styles.mask} style={{ left: 0, top: 0, right: 0, bottom: `calc(100% - ${sel.y}px)` }} />
                  <div className={styles.mask} style={{ left: 0, top: `${sel.y + sel.h}px`, right: 0, bottom: 0 }} />
                  <div className={styles.mask} style={{ left: 0, top: `${sel.y}px`, width: `${sel.x}px`, height: `${sel.h}px` }} />
                  <div className={styles.mask} style={{ left: `${sel.x + sel.w}px`, top: `${sel.y}px`, right: 0, height: `${sel.h}px` }} />
                  <div className={styles.selection} style={{ left: sel.x, top: sel.y, width: sel.w, height: sel.h }} />
                </>
              );
            })()}
          </>
        ) : (
          /* idle：整屏遮罩 */
          <div className={styles.mask} style={{ inset: 0 }} />
        ))}
    </div>
  );
}
