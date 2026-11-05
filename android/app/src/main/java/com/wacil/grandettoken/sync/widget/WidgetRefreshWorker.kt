package com.wacil.grandettoken.sync.widget

import android.content.Context
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
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
            val request = OneTimeWorkRequestBuilder<WidgetRefreshWorker>()
                .setConstraints(
                    Constraints.Builder()
                        .setRequiredNetworkType(NetworkType.CONNECTED)
                        .build()
                )
                .build()
            WorkManager.getInstance(context).enqueueUniqueWork(
                UNIQUE_NAME,
                ExistingWorkPolicy.KEEP,
                request,
            )
        }
    }
}
