/**
 * RouteErrorBoundary — 路由级错误隔离
 *
 * Business Logic（为什么需要这个组件）:
 *   单个页面 render 抛错不应白屏整应用；AppShell 侧栏、providers 与独立 overlay
 *   （截图/健康遮罩）必须继续可用。用户需要本地化摘要、重试当前路由与返回首页；
 *   生产环境不得泄漏 stack。
 *
 * Code Logic（这个组件做什么）:
 *   函数组件读取 navigate/i18n，内部 class boundary（React 19 捕获 render 错误仍需 class）
 *   捕获子树异常；resetKey 变化或重试时清除 error 并 remount children；
 *   开发环境可显示 error.message，永不渲染 error.stack。
 */

import {
  Component,
  useCallback,
  type ErrorInfo,
  type ReactNode,
} from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';

import { Button } from '@/components/primitives/Button';

import styles from './RouteErrorBoundary.module.css';

/**
 * Business Logic（为什么需要这个接口）:
 *   路由层需要用 pathname 作为复位键，并可选通知外层做额外刷新。
 *
 * Code Logic（字段说明）:
 *   resetKey 变化时清除错误；onRetry 在用户点击重试后回调；children 为受保护子树。
 */
export interface RouteErrorBoundaryProps {
  resetKey: string;
  onRetry?: () => void;
  children: ReactNode;
}

/**
 * Business Logic（为什么需要这个接口）:
 *   class boundary 不能直接用 hooks，需由函数 wrapper 注入导航与文案。
 *
 * Code Logic（字段说明）:
 *   文案与 showDetails 由 wrapper 传入；onGoHome 触发返回首页导航。
 */
interface RouteErrorBoundaryClassProps {
  resetKey: string;
  onRetry?: () => void;
  onGoHome: () => void;
  title: string;
  description: string;
  retryLabel: string;
  goHomeLabel: string;
  showDetails: boolean;
  children: ReactNode;
}

/**
 * Business Logic（为什么需要这个状态）:
 *   捕获错误后展示 fallback，并通过 remountKey 在重试时强制重建子树。
 *
 * Code Logic（字段说明）:
 *   error 非空时进入 fallback；remountKey 作为 children 的 React key。
 */
interface RouteErrorBoundaryClassState {
  error: Error | null;
  remountKey: number;
}

/**
 * Business Logic（为什么需要这个 class）:
 *   React 19 仍只能通过 class 组件的 getDerivedStateFromError / componentDidCatch
 *   捕获子树 render 阶段错误。
 *
 * Code Logic（这个 class 做什么）:
 *   捕获 error → 渲染本地化 fallback；resetKey 变化或 retry 清除 error 并 remount。
 */
class RouteErrorBoundaryClass extends Component<
  RouteErrorBoundaryClassProps,
  RouteErrorBoundaryClassState
> {
  /**
   * Business Logic（为什么需要这个静态方法）:
   *   render 抛错时需要同步切到 fallback，避免半渲染树。
   *
   * Code Logic（这个方法做什么）:
   *   把抛出的 Error 写入 state.error。
   */
  static getDerivedStateFromError(error: Error): Partial<RouteErrorBoundaryClassState> {
    return { error };
  }

  state: RouteErrorBoundaryClassState = {
    error: null,
    remountKey: 0,
  };

  /**
   * Business Logic（为什么需要这个生命周期）:
   *   调试时保留错误上下文；不把 stack 暴露给用户。
   *
   * Code Logic（这个方法做什么）:
   *   开发环境 console.error 记录 error 与 componentStack。
   */
  componentDidCatch(error: Error, info: ErrorInfo): void {
    if (import.meta.env.DEV) {
      console.error('[RouteErrorBoundary]', error, info.componentStack);
    }
  }

  /**
   * Business Logic（为什么需要这个生命周期）:
   *   路由 pathname 变化后旧错误态必须清空，否则新页面仍显示 fallback。
   *
   * Code Logic（这个方法做什么）:
   *   resetKey 变化且当前有 error 时清除 error 并 bump remountKey。
   */
  componentDidUpdate(prevProps: RouteErrorBoundaryClassProps): void {
    if (prevProps.resetKey !== this.props.resetKey && this.state.error !== null) {
      this.setState((prev) => ({
        error: null,
        remountKey: prev.remountKey + 1,
      }));
    }
  }

  /**
   * Business Logic（为什么需要这个方法）:
   *   用户点击「重试当前页」时应重新挂载失败路由，而不是只隐藏 fallback。
   *
   * Code Logic（这个方法做什么）:
   *   清空 error、递增 remountKey，并调用可选 onRetry。
   */
  private handleRetry = (): void => {
    // 先回调 onRetry，让调用方有机会关闭「继续 throw」开关，再 remount 子树
    this.props.onRetry?.();
    this.setState((prev) => ({
      error: null,
      remountKey: prev.remountKey + 1,
    }));
  };

  /**
   * Business Logic（为什么需要这个 render）:
   *   有错误时展示隔离 UI，无错误时渲染 children。
   *
   * Code Logic（这个方法做什么）:
   *   error → role=alert fallback；否则用 remountKey 包裹 children。
   */
  render(): ReactNode {
    const {
      children,
      title,
      description,
      retryLabel,
      goHomeLabel,
      showDetails,
      onGoHome,
    } = this.props;
    const { error, remountKey } = this.state;

    if (error !== null) {
      return (
        <div
          className={styles.root}
          role="alert"
          data-testid="route-error-boundary"
        >
          <h1 className={styles.title}>{title}</h1>
          <p className={styles.description}>{description}</p>
          {showDetails && error.message ? (
            <p className={styles.detail} data-testid="route-error-message">
              {error.message}
            </p>
          ) : null}
          <div className={styles.actions}>
            <Button variant="primary" onClick={this.handleRetry}>
              {retryLabel}
            </Button>
            <Button variant="secondary" onClick={onGoHome}>
              {goHomeLabel}
            </Button>
          </div>
        </div>
      );
    }

    return <div key={remountKey} className={styles.children}>{children}</div>;
  }
}

/**
 * Business Logic（为什么需要这个函数组件）:
 *   业务路由需要 navigate 回首页与 i18n 文案；hooks 只能在函数组件使用。
 *
 * Code Logic（这个组件做什么）:
 *   读取 useNavigate/useTranslation，把文案与 onGoHome 注入 class boundary；
 *   开发环境 showDetails=true 展示 message，生产不展示 stack。
 */
export function RouteErrorBoundary({
  resetKey,
  onRetry,
  children,
}: RouteErrorBoundaryProps): ReactNode {
  const navigate = useNavigate();
  const { t } = useTranslation(['common']);

  /**
   * Business Logic（为什么需要这个函数）:
   *   fallback「返回首页」需离开当前失败路由，回到稳定入口。
   *
   * Code Logic（这个函数做什么）:
   *   navigate('/', { replace: true })。
   */
  const handleGoHome = useCallback((): void => {
    navigate('/', { replace: true });
  }, [navigate]);

  return (
    <RouteErrorBoundaryClass
      resetKey={resetKey}
      onRetry={onRetry}
      onGoHome={handleGoHome}
      title={t('common:routeError.title')}
      description={t('common:routeError.description')}
      retryLabel={t('common:routeError.retry')}
      goHomeLabel={t('common:routeError.goHome')}
      showDetails={import.meta.env.DEV}
    >
      {children}
    </RouteErrorBoundaryClass>
  );
}
