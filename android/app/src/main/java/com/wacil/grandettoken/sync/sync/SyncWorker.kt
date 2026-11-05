package com.wacil.grandettoken.sync.sync

import android.content.Context
import androidx.work.CoroutineWorker
import androidx.work.WorkerParameters
import com.wacil.grandettoken.sync.NotificationHelper
import com.wacil.grandettoken.sync.widget.WidgetSyncBridge

class SyncWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val repo = GroupRepository(applicationContext)
        if (!repo.hasGroup()) return Result.success()
        val result = repo.refresh()
        NotificationHelper.onRefreshResult(applicationContext, result, repo.cached())
        WidgetSyncBridge.onRefreshResult(applicationContext, result)
        return when (result) {
            is RefreshResult.Loaded,
            is RefreshResult.NotModified,
            RefreshResult.NoGroup,
            -> Result.success()

            is RefreshResult.RateLimited -> Result.retry()

            is RefreshResult.Error -> when (result.code) {
                SyncCodes.NETWORK,
                SyncCodes.UPSTREAM,
                -> Result.retry()

                else -> Result.success()
            }
        }
    }
}
