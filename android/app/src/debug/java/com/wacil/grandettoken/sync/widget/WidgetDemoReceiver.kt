package com.wacil.grandettoken.sync.widget

import android.appwidget.AppWidgetManager
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import com.wacil.grandettoken.sync.sync.ChannelKind
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import com.wacil.grandettoken.sync.sync.ExpiringSnapshot
import com.wacil.grandettoken.sync.sync.MergedSnapshot
import com.wacil.grandettoken.sync.sync.SyncCode
import com.wacil.grandettoken.sync.sync.SyncStore
import com.wacil.grandettoken.sync.sync.WindowSnapshot

/** Debug-only fixture seeding for widget visual tests; never included in release builds. */
class WidgetDemoReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val store = SyncStore(context)
        when (intent.action) {
            ACTION_SEED -> {
                store.saveGroup(
                    SyncCode(
                        relayId = ByteArray(16) { it.toByte() },
                        key = ByteArray(32) { (it + 1).toByte() },
                        keyEpoch = 0,
                    )
                )
                store.saveCache(merged())
                store.saveLastRefresh(System.currentTimeMillis() / 1000)
                WidgetStore.setLastResult(context, "ok")
                WidgetUpdater.updateAll(context)
            }
            ACTION_CLEAR -> {
                store.clearGroup()
                WidgetUpdater.updateAll(context)
            }
            ACTION_PIN_STRIP, ACTION_PIN_CARD -> {
                val provider = if (intent.action == ACTION_PIN_STRIP) {
                    ComponentName(context, StripWidgetProvider::class.java)
                } else {
                    ComponentName(context, CardWidgetProvider::class.java)
                }
                val manager = AppWidgetManager.getInstance(context)
                if (manager.isRequestPinAppWidgetSupported) {
                    manager.requestPinAppWidget(provider, null, null)
                }
            }
        }
    }

    private fun merged(): MergedSnapshot {
        val now = System.currentTimeMillis() / 1000
        fun channel(
            id: String,
            name: String,
            kind: ChannelKind,
            unit: String,
            remaining: String? = null,
            percent: String? = null,
            windows: List<WindowSnapshot> = emptyList(),
            expiring: List<ExpiringSnapshot> = emptyList(),
            stale: Boolean = false,
            updatedAt: Long = now,
        ) = ChannelSnapshot(
            platformId = id,
            id = id,
            name = name,
            kind = kind,
            unit = unit,
            remaining = remaining,
            percent = percent,
            windows = windows,
            expiring = expiring,
            day = "1.20",
            week = "8.40",
            month = "23.60",
            estimated = true,
            stale = stale,
            updatedAt = updatedAt,
        )

        return MergedSnapshot(
            channels = listOf(
                channel("4sapi", "4SAPI", ChannelKind.Money, "CNY", "76.50", updatedAt = now - 120),
                channel(
                    "codex",
                    "Codex",
                    ChannelKind.Quota,
                    "%",
                    percent = "65",
                    windows = listOf(
                        WindowSnapshot(
                            label = "周",
                            remainPercent = "65",
                            resetText = "周一 00:00",
                            key = "weekly",
                            main = true,
                        ),
                        WindowSnapshot(
                            label = "5 小时",
                            remainPercent = "78",
                            resetText = "18:00",
                            key = "5h",
                        ),
                    ),
                    updatedAt = now - 300,
                ),
                channel(
                    "qoder",
                    "Qoder",
                    ChannelKind.Points,
                    "credits",
                    "148",
                    expiring = listOf(ExpiringSnapshot("300", now + 86_400L * 4)),
                    updatedAt = now - 400,
                ),
                channel("deepseek", "DeepSeek", ChannelKind.Money, "CNY", "256.00", updatedAt = now - 60),
                channel(
                    "workbuddy",
                    "WorkBuddy",
                    ChannelKind.Points,
                    "credits",
                    "320",
                    stale = true,
                    updatedAt = now - 7_200,
                ),
                channel(
                    "opencode",
                    "OpenCode Go",
                    ChannelKind.Quota,
                    "%",
                    percent = "82",
                    windows = listOf(
                        WindowSnapshot(
                            label = "周期",
                            remainPercent = "82",
                            resetText = "12-03",
                            key = "monthly",
                            main = true,
                        ),
                    ),
                    updatedAt = now - 500,
                ),
            ),
            sources = 2,
            staleChannels = 1,
            generatedAt = now,
        )
    }

    companion object {
        const val ACTION_SEED = "com.wacil.grandettoken.sync.DEMO_SEED"
        const val ACTION_CLEAR = "com.wacil.grandettoken.sync.DEMO_CLEAR"
        const val ACTION_PIN_STRIP = "com.wacil.grandettoken.sync.DEMO_PIN_STRIP"
        const val ACTION_PIN_CARD = "com.wacil.grandettoken.sync.DEMO_PIN_CARD"
    }
}
