package com.wacil.grandettoken.sync.sync

import android.content.Context
import androidx.work.CoroutineWorker
import androidx.work.WorkerParameters

class SyncWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val repo = GroupRepository(applicationContext)
        if (!repo.hasGroup()) return Result.success()
        return when (val r = repo.refresh()) {
            is RefreshResult.Loaded,
            is RefreshResult.NotModified,
            RefreshResult.NoGroup,
            -> Result.success()

            is RefreshResult.RateLimited -> Result.retry()

            is RefreshResult.Error -> when (r.code) {
                SyncCodes.NETWORK,
                SyncCodes.UPSTREAM,
                -> Result.retry()

                else -> Result.success()
            }
        }
    }
}
