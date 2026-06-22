/**
 * useAnnotationCanvas - 截图编辑模式的 canvas 重绘 hook
 *
 * Business Logic: 编辑模式 canvas 要同时画「桌面快照底图」+「所有标注」（矩形/箭头），
 *   且标注增删/撤销时实时重绘，所见即所得（canvas 内容 = 最终合成图）。
 *
 * Code Logic: 监听 snapshot / annotations 变化的 useEffect，每次重绘：清空 → drawImage(快照)
 *   → 遍历标注 strokeRect 或画箭头（主线 + 终点三角头）。线宽 = 3×dpr 物理清晰。
 */

import { useEffect, type RefObject } from 'react';

/** 单个标注（选区内逻辑坐标） */
export interface Annotation {
  tool: 'rect' | 'arrow';
  color: string; // #RRGGBB
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

/** 画箭头：主线 (x1,y1)→(x2,y2) + 终点三角头（按角度旋转） */
function drawArrow(ctx: CanvasRenderingContext2D, x1: number, y1: number, x2: number, y2: number, headLen: number) {
  const angle = Math.atan2(y2 - y1, x2 - x1);
  ctx.beginPath();
  ctx.moveTo(x1, y1);
  ctx.lineTo(x2, y2);
  // 三角头两条边
  ctx.lineTo(x2 - headLen * Math.cos(angle - Math.PI / 6), y2 - headLen * Math.sin(angle - Math.PI / 6));
  ctx.moveTo(x2, y2);
  ctx.lineTo(x2 - headLen * Math.cos(angle + Math.PI / 6), y2 - headLen * Math.sin(angle + Math.PI / 6));
  ctx.stroke();
}

/** 全量重绘 canvas：快照底图 + 全部标注 */
function redraw(
  ctx: CanvasRenderingContext2D,
  snapshot: HTMLImageElement,
  annotations: Annotation[],
  logicalW: number,
  logicalH: number,
  dpr: number,
) {
  ctx.clearRect(0, 0, logicalW, logicalH);
  ctx.drawImage(snapshot, 0, 0, logicalW, logicalH);
  ctx.lineWidth = 3 * dpr;
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  const headLen = 12 * dpr;
  for (const a of annotations) {
    ctx.strokeStyle = a.color;
    ctx.fillStyle = a.color;
    if (a.tool === 'rect') {
      ctx.strokeRect(
        Math.min(a.x1, a.x2),
        Math.min(a.y1, a.y2),
        Math.abs(a.x2 - a.x1),
        Math.abs(a.y2 - a.y1),
      );
    } else {
      drawArrow(ctx, a.x1, a.y1, a.x2, a.y2, headLen);
    }
  }
}

/**
 * 监听 snapshot/annotations 变化重绘 canvas。
 * canvas 物理缓冲由调用方设置（canvas.width = logicalW*dpr），本 hook 只负责绘制内容。
 */
export function useAnnotationCanvas(
  canvasRef: RefObject<HTMLCanvasElement>,
  snapshot: HTMLImageElement | null,
  annotations: Annotation[],
  logicalW: number,
  logicalH: number,
  dpr: number,
): void {
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !snapshot) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    // canvas 物理缓冲 = 逻辑×dpr；ctx.scale 后用逻辑坐标绘制
    canvas.width = Math.max(1, Math.round(logicalW * dpr));
    canvas.height = Math.max(1, Math.round(logicalH * dpr));
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.scale(dpr, dpr);
    redraw(ctx, snapshot, annotations, logicalW, logicalH, dpr);
  }, [canvasRef, snapshot, annotations, logicalW, logicalH, dpr]);
}
