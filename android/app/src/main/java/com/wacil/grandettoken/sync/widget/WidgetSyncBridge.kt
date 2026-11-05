package com.wacil.grandettoken.sync.widget

import android.content.Context
import com.wacil.grandettoken.sync.sync.GroupRepository
import com.wacil.grandettoken.sync.sync.RefreshResult
import com.wacil.grandettoken.sync.sync.SyncCodes

/** App 前台、WorkManager 和小部件手动刷新共用同一份状态更新。 */
object WidgetSyncBridge {
    /** 自动补刷的最小间隔：小部件被恢复/改尺寸频繁触发时不要每次都联网。 */
    private const val AUTO_REFRESH_GAP_MS = 5 * 60_000L

    /** 快照发布超过这个时间就认为该补刷（后台任务可能被系统延迟）。 */
    private const val FRESH_AGE_SECONDS = 15 * 60L

    fun onRefreshResult(context: Context, result: RefreshResult) {
        val status = when (result) {
            is RefreshResult.Loaded,
            is RefreshResult.NotModified,
            -> "ok"
            is RefreshResult.RateLimited -> "rate"
            RefreshResult.NoGroup -> "none"
            is RefreshResult.Error -> when (result.code) {
                SyncCodes.NETWORK -> "offline"
                else -> "error"
            }
        }
        WidgetStore.setLastResult(context, status)
        WidgetStore.clearRefreshing(context)
        WidgetUpdater.updateAll(context)
    }

    fun onGroupChanged(context: Context) {
        WidgetStore.clearRefreshing(context)
        WidgetUpdater.updateAll(context)
    }

    /**
     * 小部件本身只画缓存。被添加、系统恢复、改尺寸时，如果缓存为空、已选条目
     * 缺数据或快照太旧，就节流触发一次后台刷新，避免一直停在旧/不完整快照上。
     */
    fun onWidgetShown(context: Context) {
        val repo = GroupRepository(context)
        if (!repo.hasGroup()) return
        if (WidgetStore.isRefreshing(context)) return
        val now = System.currentTimeMillis()
        if (now - WidgetStore.lastAutoRefresh(context) < AUTO_REFRESH_GAP_MS) return
        val cache = repo.cached()
        val needsRefresh = cache == null ||
            now / 1000 - cache.generatedAt >= FRESH_AGE_SECONDS ||
            WidgetUpdater.hasUnresolved(context, cache)
        if (!needsRefresh) return
        WidgetStore.markAutoRefresh(context, now)
        WidgetRefreshWorker.enqueue(context)
    }
}
