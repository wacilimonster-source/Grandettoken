package com.wacil.grandettoken.sync.widget

import android.appwidget.AppWidgetManager
import android.content.ComponentName
import android.content.Context
import org.json.JSONArray
import org.json.JSONObject

enum class WidgetKind(val label: String, val limit: Int) {
    STRIP("4×1 横条", 3),
    CARD("4×2 卡片", 4),
}

/**
 * 一个可显示条目：渠道 + 可选窗口。Codex 的周/5 小时是同一渠道的两个条目，
 * 所以 key 必须带上 windowKey，不能只按 platformId 去重。
 */
data class WidgetEntry(
    val platformId: String,
    val name: String,
    val windowKey: String? = null,
    val windowLabel: String? = null,
) {
    val entryKey: String get() = keyOf(platformId, windowKey)
    val displayName: String
        get() = if (windowLabel.isNullOrBlank()) name else "$name · $windowLabel"
}

fun keyOf(platformId: String, windowKey: String?): String =
    if (windowKey.isNullOrBlank()) platformId else "$platformId|$windowKey"

data class WidgetInfo(
    val id: Int,
    val kind: WidgetKind,
    val selection: List<WidgetEntry>,
)

/** 每个小部件实例独立保存渠道/窗口选择、顺序和尺寸；状态是组级共享的。 */
object WidgetStore {
    private const val PREFS = "grandettoken_widgets"
    private const val SELECTION = "selection:"
    private const val KIND = "kind:"
    private const val REFRESHING_UNTIL = "refreshingUntil"
    private const val LAST_RESULT = "lastResult"
    private const val MANUAL_REFRESH_AT = "manualRefreshAt"
    private const val AUTO_REFRESH_AT = "autoRefreshAt"

    fun load(context: Context, appWidgetId: Int): List<WidgetEntry> {
        val raw = prefs(context).getString(SELECTION + appWidgetId, null) ?: return emptyList()
        return runCatching {
            val array = JSONArray(raw)
            buildList {
                for (i in 0 until array.length()) {
                    val obj = array.optJSONObject(i) ?: continue
                    val platformId = obj.optString("platformId").ifBlank {
                        obj.optString("id")
                    }.trim()
                    if (platformId.isBlank()) continue
                    val name = obj.optString("name").trim().ifBlank { platformId }
                    val windowKey = obj.stringOrNull("windowKey")
                    val windowLabel = obj.stringOrNull("windowLabel")
                    add(WidgetEntry(platformId, name, windowKey, windowLabel))
                }
            }.distinctBy { it.entryKey }
        }.getOrDefault(emptyList())
    }

    fun save(
        context: Context,
        appWidgetId: Int,
        kind: WidgetKind,
        selection: List<WidgetEntry>,
    ) {
        val clean = selection
            .filter { it.platformId.isNotBlank() }
            .distinctBy { it.entryKey }
            .take(kind.limit)
        val array = JSONArray()
        clean.forEach { entry ->
            val obj = JSONObject()
                .put("platformId", entry.platformId)
                .put("id", entry.platformId)
                .put("name", entry.name.trim().ifBlank { entry.platformId })
            entry.windowKey?.takeIf { it.isNotBlank() }?.let { obj.put("windowKey", it) }
            entry.windowLabel?.takeIf { it.isNotBlank() }?.let { obj.put("windowLabel", it) }
            array.put(obj)
        }
        prefs(context).edit()
            .putString(SELECTION + appWidgetId, array.toString())
            .putString(KIND + appWidgetId, kind.name)
            .apply()
    }

    fun kind(context: Context, appWidgetId: Int): WidgetKind? {
        prefs(context).getString(KIND + appWidgetId, null)?.let { raw ->
            WidgetKind.entries.firstOrNull { it.name == raw }?.let { return it }
        }
        val provider = AppWidgetManager.getInstance(context)
            .getAppWidgetInfo(appWidgetId)
            ?.provider
            ?.className
        return when (provider) {
            StripWidgetProvider::class.java.name -> WidgetKind.STRIP
            CardWidgetProvider::class.java.name -> WidgetKind.CARD
            else -> null
        }
    }

    fun remove(context: Context, appWidgetId: Int) {
        prefs(context).edit()
            .remove(SELECTION + appWidgetId)
            .remove(KIND + appWidgetId)
            .apply()
    }

    fun list(context: Context): List<WidgetInfo> {
        val manager = AppWidgetManager.getInstance(context)
        val ids = LinkedHashSet<Int>()
        ids.addAll(manager.getAppWidgetIds(ComponentName(context, StripWidgetProvider::class.java)).toList())
        ids.addAll(manager.getAppWidgetIds(ComponentName(context, CardWidgetProvider::class.java)).toList())
        return ids.mapNotNull { id ->
            val kind = kind(context, id) ?: return@mapNotNull null
            WidgetInfo(id, kind, load(context, id))
        }.sortedWith(compareBy<WidgetInfo> { it.kind.ordinal }.thenBy { it.id })
    }

    fun markRefreshing(context: Context, untilMs: Long) {
        prefs(context).edit().putLong(REFRESHING_UNTIL, untilMs).apply()
    }

    fun isRefreshing(context: Context): Boolean =
        System.currentTimeMillis() < prefs(context).getLong(REFRESHING_UNTIL, 0L)

    fun clearRefreshing(context: Context) {
        prefs(context).edit().remove(REFRESHING_UNTIL).apply()
    }

    fun setLastResult(context: Context, result: String) {
        prefs(context).edit().putString(LAST_RESULT, result).apply()
    }

    fun lastResult(context: Context): String? =
        prefs(context).getString(LAST_RESULT, null)

    fun markManualRefresh(context: Context, atMs: Long) {
        prefs(context).edit().putLong(MANUAL_REFRESH_AT, atMs).apply()
    }

    fun lastManualRefresh(context: Context): Long =
        prefs(context).getLong(MANUAL_REFRESH_AT, 0L)

    fun markAutoRefresh(context: Context, atMs: Long) {
        prefs(context).edit().putLong(AUTO_REFRESH_AT, atMs).apply()
    }

    fun lastAutoRefresh(context: Context): Long =
        prefs(context).getLong(AUTO_REFRESH_AT, 0L)

    private fun JSONObject.stringOrNull(key: String): String? {
        if (!has(key) || isNull(key)) return null
        return optString(key).trim().takeIf { it.isNotBlank() }
    }

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
