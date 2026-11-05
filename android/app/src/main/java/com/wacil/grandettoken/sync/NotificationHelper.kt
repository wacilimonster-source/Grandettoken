package com.wacil.grandettoken.sync

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.provider.Settings
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import com.wacil.grandettoken.sync.sync.MergedSnapshot
import com.wacil.grandettoken.sync.sync.RefreshResult

object NotificationHelper {
    const val CHANNEL_ID = "grandettoken_alerts"
    private const val PREFS = "grandettoken_notifications"
    private const val COOLDOWN_MS = 6L * 60L * 60L * 1000L

    fun enabled(context: Context): Boolean =
        prefs(context).getBoolean("master", false)

    fun setEnabled(context: Context, value: Boolean) {
        prefs(context).edit().putBoolean("master", value).apply()
        if (value) ensureChannel(context)
    }

    fun typeEnabled(context: Context, key: String): Boolean =
        prefs(context).getBoolean(key, true)

    fun setTypeEnabled(context: Context, key: String, value: Boolean) {
        prefs(context).edit().putBoolean(key, value).apply()
    }

    fun permissionGranted(context: Context): Boolean {
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            return false
        }
        return NotificationManagerCompat.from(context).areNotificationsEnabled()
    }

    fun ensureChannel(context: Context) {
        if (Build.VERSION.SDK_INT < 26) return
        val manager = context.getSystemService(NotificationManager::class.java)
        val channel = NotificationChannel(
            CHANNEL_ID,
            "同步提醒",
            NotificationManager.IMPORTANCE_DEFAULT,
        ).apply {
            description = "余额偏低、数据过期和同步失败提醒"
        }
        manager.createNotificationChannel(channel)
    }

    fun openSettings(context: Context) {
        val intent = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
            .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        context.startActivity(intent)
    }

    fun onRefreshResult(
        context: Context,
        result: RefreshResult,
        merged: MergedSnapshot?,
    ) {
        val p = prefs(context)
        when (result) {
            is RefreshResult.Loaded,
            is RefreshResult.NotModified,
            -> {
                p.edit().putInt("failureStreak", 0).apply()
                evaluate(context, merged)
            }

            is RefreshResult.Error -> {
                val streak = p.getInt("failureStreak", 0) + 1
                p.edit().putInt("failureStreak", streak).apply()
                if (NotificationRules.failureReached(streak) && typeEnabled(context, "failure")) {
                    val body = if (merged == null) {
                        "已连续失败 $streak 次,请检查网络或 GitHub 授权。"
                    } else {
                        "已连续失败 $streak 次,保留最后可信快照;请检查网络或 GitHub 授权。"
                    }
                    post(context, "failure", "同步失败", body)
                }
            }

            is RefreshResult.RateLimited -> Unit
            RefreshResult.NoGroup -> p.edit().putInt("failureStreak", 0).apply()
        }
    }

    fun onJoin(context: Context, merged: MergedSnapshot?) {
        prefs(context).edit().putInt("failureStreak", 0).apply()
        evaluate(context, merged)
    }

    private fun evaluate(context: Context, merged: MergedSnapshot?) {
        val now = System.currentTimeMillis() / 1000
        if (typeEnabled(context, "balance")) {
            NotificationRules.lowBalance(merged).forEach { ch ->
                val body = when (ch.kind) {
                    com.wacil.grandettoken.sync.sync.ChannelKind.Quota ->
                        "剩余额度低于 ${NotificationRules.LOW_PERCENT_MICROS / 1_000_000}%"
                    else -> "当前余额低于提醒线(¥${NotificationRules.LOW_MONEY_MICROS / 1_000_000})"
                }
                post(context, "balance:${ch.platformId}", "余额提醒 · ${ch.name}", body)
            }
        }
        if (typeEnabled(context, "stale")) {
            val stale = NotificationRules.stale(merged, now)
            if (stale.isNotEmpty()) {
                post(
                    context,
                    "stale",
                    "数据较旧",
                    "${stale.size} 个渠道超过 2 小时未更新,打开 App 查看最后可信数据。",
                )
            }
        }
    }

    private fun post(context: Context, key: String, title: String, body: String) {
        if (!enabled(context) || !permissionGranted(context)) return
        ensureChannel(context)
        val p = prefs(context)
        val last = p.getLong("last:$key", 0L)
        val now = System.currentTimeMillis()
        if (now - last < COOLDOWN_MS) return
        p.edit().putLong("last:$key", now).apply()

        val intent = Intent(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
        val pending = PendingIntent.getActivity(
            context,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setContentIntent(pending)
            .setAutoCancel(true)
            .setPriority(NotificationCompat.PRIORITY_DEFAULT)
            .build()
        NotificationManagerCompat.from(context)
            .notify((key.hashCode() and 0x7FFFFFFF) % 100_000, notification)
    }

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
