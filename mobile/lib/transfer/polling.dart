import 'dart:async';

import 'package:flutter/widgets.dart';

/// 周期定时器工厂签名；测试注入假 timer 以便手动驱动 tick。
typedef PollerTimerFactory = Timer Function(Duration interval, void Function() onTick);

/// 默认工厂：包一层把 Timer.periodic 的 Timer 参数适配成无参回调。
Timer _defaultTimerFactory(Duration interval, void Function() onTick) =>
    Timer.periodic(interval, (_) => onTick());

/// Business Logic: 传输页设备/任务列表需要对齐 web useVisibilityPolling——
/// 仅在 App 可见（resumed）时轮询，后台不得空转网络，回前台立即补拉一次。
/// Code Logic: 持有 periodic Timer + WidgetsBindingObserver；resumed → 立即执行一次并
/// 启动 timer，其余状态停 timer；runNow single-flight，轮询错误不向外冒泡。
class VisibilityPoller with WidgetsBindingObserver {
  VisibilityPoller({
    required this.interval,
    required this.task,
    this.timerFactory = _defaultTimerFactory,
  });

  /// 轮询周期（任务 3s / 设备 5s）。
  final Duration interval;

  /// 每次 tick 执行的刷新任务；异常由调用方任务内部消化，poller 吞掉避免崩溃。
  final Future<void> Function() task;

  /// 可注入的定时器工厂（测试假 timer）。
  final PollerTimerFactory timerFactory;

  bool _started = false;
  bool _resumed = true;
  bool _running = false;
  Timer? _timer;

  /// 是否正在执行刷新任务（single-flight 标记）。
  bool get isRunning => _running;

  /// Business Logic: 页面 initState 后启动；启动即视为 resumed（回前台语义由生命周期接管）。
  /// Code Logic: 注册 observer；resumed 时可选立即执行一次并启动周期 timer。
  void start({bool runImmediately = true}) {
    if (_started) {
      return;
    }
    _started = true;
    WidgetsBinding.instance.addObserver(this);
    if (_resumed) {
      if (runImmediately) {
        runNow();
      }
      _ensureTimer();
    }
  }

  /// Business Logic: 页面 dispose 或后台切换时必须停掉定时器，避免后台空转。
  /// Code Logic: 停 timer 并移除 observer；stop 后可再次 start。
  void stop() {
    _started = false;
    _cancelTimer();
    WidgetsBinding.instance.removeObserver(this);
  }

  /// Business Logic: 页面销毁后不得残留任何 timer。
  /// Code Logic: stop 的别名，语义上表示永久结束。
  void dispose() => stop();

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    handleLifecycle(state);
  }

  /// Business Logic: 仅 resumed 视为可见（对齐 web visibilityState === 'visible'），
  /// 回前台立即拉一次，离开前台立即停表。
  /// Code Logic: 公开便于单测直接注入生命周期状态；hidden/inactive/paused/detached 全部停表。
  void handleLifecycle(AppLifecycleState state) {
    if (!_started) {
      _resumed = state == AppLifecycleState.resumed;
      return;
    }
    _resumed = state == AppLifecycleState.resumed;
    if (_resumed) {
      runNow();
      _ensureTimer();
    } else {
      _cancelTimer();
    }
  }

  /// Business Logic: mutation 后需要立即刷新，且同一 poll 最多一个 in-flight。
  /// Code Logic: single-flight 门闩；任务异常吞掉（轮询不允许打崩页面）。
  Future<void> runNow() async {
    if (_running) {
      return;
    }
    _running = true;
    try {
      await task();
    } catch (_) {
      // 轮询失败由页面按“保留旧列表”策略处理，不打断下一次 tick。
    } finally {
      _running = false;
    }
  }

  void _ensureTimer() {
    if (_timer != null || !_started || !_resumed) {
      return;
    }
    _timer = timerFactory(interval, _onTick);
  }

  void _cancelTimer() {
    _timer?.cancel();
    _timer = null;
  }

  void _onTick() {
    if (!_started || !_resumed) {
      return;
    }
    runNow();
  }
}
