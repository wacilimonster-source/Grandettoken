package com.wacil.grandettoken.sync.widget

import android.content.Context
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import com.wacil.grandettoken.sync.sync.GroupRepository

class WidgetRefreshWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val repo = GroupRepository(applicationContext)
        if (!repo.hasGroup()) {
            WidgetSyncBridge.onGroupChanged(applicationContext)
            return Result.success()
        }
        val result = repo.refresh(force = true)
        WidgetSyncBridge.onRefreshResult(applicationContext, result)
        return Result.success()
    }

    companion object {
        private const val UNIQUE_NAME = "grandettoken-widget-refresh"

        fun enqueue(context: Context) {
            // 不加联网约束:离线时任务也要立即执行,把"离线"结果画到小部件上,
            // 而不是挂在队列里让"刷新中"状态悄悄过期。
            val request = OneTimeWorkRequestBuilder<WidgetRefreshWorker>()
                .build()
            WorkManager.getInstance(context).enqueueUniqueWork(
                UNIQUE_NAME,
                ExistingWorkPolicy.REPLACE,
                request,
            )
        }
    }
}
