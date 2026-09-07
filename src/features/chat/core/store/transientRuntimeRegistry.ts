/**
 * Chat V2 - 瞬态运行时重置注册表
 *
 * 模块级运行时（如审批队列 approvalQueue）挂在 plugins/ 层，core/store 的
 * 流终止路径（abortStream/completeStream）无法直接 import 它们——会经
 * UnifiedNotification 等 UI 模块形成循环依赖。这里提供注册点：持有模块级
 * 运行时的插件在加载时注册 resetter，流终止时统一调用。
 *
 * 上游对应：dbfc7d0de 的 transientRuntimeRegistry（按 fork 模块结构精简）。
 */

type RuntimeResetter = () => void;

const resetters: RuntimeResetter[] = [];

/** 注册一个瞬态运行时重置回调（模块加载时调用，持久有效） */
export function registerTransientRuntime(reset: RuntimeResetter): void {
  resetters.push(reset);
}

/** 重置所有已注册的瞬态运行时；单个失败不阻断其余 */
export function resetTransientRuntimes(): void {
  for (const reset of resetters) {
    try {
      reset();
    } catch (error) {
      console.warn('[TransientRuntime] reset failed:', error);
    }
  }
}
