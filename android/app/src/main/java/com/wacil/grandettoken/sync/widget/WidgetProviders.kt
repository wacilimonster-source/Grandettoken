package com.wacil.grandettoken.sync.widget

import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.Context
import android.content.Intent

abstract class BaseWidgetProvider : AppWidgetProvider() {
    override fun onUpdate(
        context: Context,
        appWidgetManager: AppWidgetManager,
        appWidgetIds: IntArray,
    ) {
        WidgetUpdater.updateAll(context)
        WidgetSyncBridge.onWidgetShown(context)
    }

    override fun onAppWidgetOptionsChanged(
        context: Context,
        appWidgetManager: AppWidgetManager,
        appWidgetId: Int,
        newOptions: android.os.Bundle,
    ) {
        WidgetUpdater.updateAll(context)
        WidgetSyncBridge.onWidgetShown(context)
    }

    override fun onDeleted(context: Context, appWidgetIds: IntArray) {
        appWidgetIds.forEach { WidgetStore.remove(context, it) }
        WidgetUpdater.updateAll(context)
    }

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == WidgetUpdater.ACTION_REFRESH) {
            val now = System.currentTimeMillis()
            if (now - WidgetStore.lastManualRefresh(context) < 30_000L) {
                WidgetStore.setLastResult(context, "rate")
                WidgetUpdater.updateAll(context)
                return
            }
            WidgetStore.markManualRefresh(context, now)
            WidgetStore.markRefreshing(context, now + 45_000L)
            WidgetUpdater.updateAll(context)
            WidgetRefreshWorker.enqueue(context)
            return
        }
        super.onReceive(context, intent)
    }
}

class StripWidgetProvider : BaseWidgetProvider()

class CardWidgetProvider : BaseWidgetProvider()
