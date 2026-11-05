package com.wacil.grandettoken.sync.widget

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.view.View
import android.widget.RemoteViews
import com.wacil.grandettoken.sync.MainActivity
import com.wacil.grandettoken.sync.R
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import com.wacil.grandettoken.sync.sync.GroupRepository
import com.wacil.grandettoken.sync.sync.MergedSnapshot

/** 只读取合并缓存；不显示同步码、token 或任何写入者标识。 */
object WidgetUpdater {
    const val ACTION_REFRESH = "com.wacil.grandettoken.sync.widget.ACTION_REFRESH"
    const val EXTRA_APPWIDGET_ID = "appWidgetId"
    const val EXTRA_CHANNEL_ID = "channelId"
    const val EXTRA_KIND = "kind"

    private const val TEXT = 0xFFF0F2F4.toInt()
    private const val MUTED = 0xFFA4B2AE.toInt()
    private const val AMBER = 0xFFEDBB77.toInt()
    private const val RED = 0xFFF39393.toInt()
    private const val BLUE = 0xFFAEBEFF.toInt()
    private const val MINT = 0xFF8DE3BC.toInt()

    private val stripCells = intArrayOf(R.id.strip_cell_0, R.id.strip_cell_1, R.id.strip_cell_2)
    private val stripNames = intArrayOf(R.id.strip_name_0, R.id.strip_name_1, R.id.strip_name_2)
    private val stripValues = intArrayOf(R.id.strip_value_0, R.id.strip_value_1, R.id.strip_value_2)
    private val stripTags = intArrayOf(R.id.strip_tag_0, R.id.strip_tag_1, R.id.strip_tag_2)
    private val stripSeps = intArrayOf(R.id.strip_sep_1, R.id.strip_sep_2)
    private val cardCells = intArrayOf(R.id.card_cell_0, R.id.card_cell_1, R.id.card_cell_2, R.id.card_cell_3)
    private val cardIcons = intArrayOf(R.id.card_icon_0, R.id.card_icon_1, R.id.card_icon_2, R.id.card_icon_3)
    private val cardNames = intArrayOf(R.id.card_name_0, R.id.card_name_1, R.id.card_name_2, R.id.card_name_3)
    private val cardNotes = intArrayOf(R.id.card_note_0, R.id.card_note_1, R.id.card_note_2, R.id.card_note_3)
    private val cardValues = intArrayOf(R.id.card_value_0, R.id.card_value_1, R.id.card_value_2, R.id.card_value_3)
    private val cardTags = intArrayOf(R.id.card_tag_0, R.id.card_tag_1, R.id.card_tag_2, R.id.card_tag_3)

    fun updateAll(context: Context) {
        val repo = GroupRepository(context)
        val hasGroup = repo.hasGroup()
        val cache = if (hasGroup) repo.cached() else null
        val manager = AppWidgetManager.getInstance(context)
        updateKind(context, manager, WidgetKind.STRIP, hasGroup, cache)
        updateKind(context, manager, WidgetKind.CARD, hasGroup, cache)
    }

    /** 已选条目在当前缓存里解析不出来（会显示 “—”）：说明缓存缺了这份数据。 */
    fun hasUnresolved(context: Context, cache: MergedSnapshot): Boolean {
        val byId = cache.channels.associateBy { it.platformId }
        return WidgetStore.list(context).any { info ->
            info.selection.any { entry ->
                WidgetFormatter.value(byId[entry.platformId], entry.windowKey) == "—"
            }
        }
    }

    fun update(context: Context, appWidgetId: Int, kind: WidgetKind) {
        val repo = GroupRepository(context)
        val hasGroup = repo.hasGroup()
        render(
            context,
            AppWidgetManager.getInstance(context),
            appWidgetId,
            kind,
            hasGroup,
            if (hasGroup) repo.cached() else null,
        )
    }

    private fun updateKind(
        context: Context,
        manager: AppWidgetManager,
        kind: WidgetKind,
        hasGroup: Boolean,
        cache: MergedSnapshot?,
    ) {
        val provider = when (kind) {
            WidgetKind.STRIP -> StripWidgetProvider::class.java
            WidgetKind.CARD -> CardWidgetProvider::class.java
        }
        manager.getAppWidgetIds(ComponentName(context, provider)).forEach { id ->
            render(context, manager, id, kind, hasGroup, cache)
        }
    }

    private fun render(
        context: Context,
        manager: AppWidgetManager,
        appWidgetId: Int,
        kind: WidgetKind,
        hasGroup: Boolean,
        cache: MergedSnapshot?,
    ) {
        val views = RemoteViews(
            context.packageName,
            if (kind == WidgetKind.STRIP) R.layout.widget_strip else R.layout.widget_card,
        )
        val selection = WidgetStore.load(context, appWidgetId).take(kind.limit)
        val byId = cache?.channels?.associateBy { it.platformId }.orEmpty()
        val items = selection.map { it to byId[it.platformId] }

        val empty = when {
            !hasGroup -> EmptyState("先加入同步组", "连接电脑后显示渠道数据", "去连接")
            cache == null -> EmptyState(
                "等待首次同步",
                if (WidgetStore.isRefreshing(context)) "正在刷新…" else "电脑需要先发布数据",
                "打开 App",
            )
            items.isEmpty() -> EmptyState("选择显示的条目", "选择渠道或窗口后在这里查看数据", "选择条目")
            else -> null
        }

        if (empty != null) {
            setEmpty(context, views, appWidgetId, kind, empty)
        } else if (kind == WidgetKind.STRIP) {
            renderStrip(context, views, appWidgetId, items)
        } else {
            renderCard(context, views, appWidgetId, items)
        }
        manager.updateAppWidget(appWidgetId, views)
    }

    private fun setEmpty(
        context: Context,
        views: RemoteViews,
        appWidgetId: Int,
        kind: WidgetKind,
        state: EmptyState,
    ) {
        if (kind == WidgetKind.STRIP) {
            views.setViewVisibility(R.id.strip_content, View.GONE)
            views.setViewVisibility(R.id.strip_empty, View.VISIBLE)
            views.setTextViewText(R.id.strip_empty_title, state.title)
            views.setTextViewText(R.id.strip_empty_sub, state.subtitle)
            views.setTextViewText(R.id.strip_empty_action, state.action)
            views.setOnClickPendingIntent(
                R.id.strip_empty_action,
                emptyAction(context, appWidgetId, kind, state.action),
            )
        } else {
            views.setViewVisibility(R.id.card_content, View.GONE)
            views.setViewVisibility(R.id.card_empty, View.VISIBLE)
            views.setTextViewText(R.id.card_empty_title, state.title)
            views.setTextViewText(R.id.card_empty_sub, state.subtitle)
            views.setTextViewText(R.id.card_empty_action, state.action)
            views.setOnClickPendingIntent(
                R.id.card_empty_action,
                emptyAction(context, appWidgetId, kind, state.action),
            )
        }
    }

    private fun renderStrip(
        context: Context,
        views: RemoteViews,
        appWidgetId: Int,
        items: List<Pair<WidgetEntry, ChannelSnapshot?>>,
    ) {
        val now = System.currentTimeMillis() / 1000
        val refreshing = WidgetStore.isRefreshing(context)
        val lastResult = WidgetStore.lastResult(context)
        val channels = items.map { it.second }
        val values = items.map { WidgetFormatter.value(it.second, it.first.windowKey) }
        val stale = items.any { it.second != null && WidgetFormatter.note(it.second, now) != null }
        views.setViewVisibility(R.id.strip_content, View.VISIBLE)
        views.setViewVisibility(R.id.strip_empty, View.GONE)

        val visibleCount = minOf(items.size, 3)
        for (i in 0..2) {
            val visible = i < visibleCount
            views.setViewVisibility(stripCells[i], if (visible) View.VISIBLE else View.GONE)
            if (i > 0) {
                views.setViewVisibility(stripSeps[i - 1], if (visible) View.VISIBLE else View.GONE)
            }
            if (!visible) continue
            val (entry, channel) = items[i]
            val note = WidgetFormatter.note(channel, now)
            val unavailable = values[i] == "—"
            val name = (channel?.name ?: entry.name) + if (note != null || unavailable) " !" else ""
            views.setTextViewText(stripNames[i], name)
            views.setTextColor(stripNames[i], if (note != null || unavailable) AMBER else MUTED)
            views.setTextViewText(stripValues[i], values[i])
            views.setTextColor(stripValues[i], if (unavailable) MUTED else TEXT)
            setTag(views, stripTags[i], WidgetFormatter.tag(channel, entry.windowKey), unavailable)
            views.setOnClickPendingIntent(
                stripCells[i],
                channelIntent(context, entry.platformId, appWidgetId * 100 + i),
            )
        }

        val label = WidgetFormatter.sideLabel(values, stale, lastResult, refreshing)
        val earliest = WidgetFormatter.earliest(channels)
        views.setTextViewText(R.id.strip_time, "$label\n$earliest")
        views.setTextColor(
            R.id.strip_time,
            when (WidgetFormatter.tone(values, stale, lastResult, refreshing)) {
                WidgetFormatter.Tone.ERROR -> RED
                WidgetFormatter.Tone.WARN -> AMBER
                WidgetFormatter.Tone.NORMAL -> MUTED
            },
        )
        views.setOnClickPendingIntent(
            R.id.strip_refresh,
            refreshIntent(context, appWidgetId, WidgetKind.STRIP),
        )
        views.setContentDescription(R.id.strip_refresh, "刷新同步组")
    }

    private fun renderCard(
        context: Context,
        views: RemoteViews,
        appWidgetId: Int,
        items: List<Pair<WidgetEntry, ChannelSnapshot?>>,
    ) {
        val now = System.currentTimeMillis() / 1000
        val refreshing = WidgetStore.isRefreshing(context)
        val lastResult = WidgetStore.lastResult(context)
        val channels = items.map { it.second }
        val values = items.map { WidgetFormatter.value(it.second, it.first.windowKey) }
        val stale = items.any { it.second != null && WidgetFormatter.note(it.second, now) != null }
        views.setViewVisibility(R.id.card_content, View.VISIBLE)
        views.setViewVisibility(R.id.card_empty, View.GONE)
        views.setViewVisibility(R.id.card_row_0, if (items.isNotEmpty()) View.VISIBLE else View.GONE)
        views.setViewVisibility(R.id.card_row_1, if (items.size > 2) View.VISIBLE else View.GONE)

        for (i in 0..3) {
            val visible = i < items.size
            views.setViewVisibility(cardCells[i], if (visible) View.VISIBLE else View.GONE)
            if (!visible) {
                views.setViewVisibility(cardNotes[i], View.GONE)
                views.setViewVisibility(cardTags[i], View.GONE)
                continue
            }
            val (entry, channel) = items[i]
            val name = channel?.name ?: entry.name
            val note = WidgetFormatter.note(channel, now)
            val unavailable = values[i] == "—"
            views.setTextViewText(cardIcons[i], initials(name))
            views.setTextViewText(cardNames[i], name)
            views.setTextViewText(cardValues[i], values[i])
            views.setTextColor(cardValues[i], if (unavailable) MUTED else TEXT)
            setTag(views, cardTags[i], WidgetFormatter.tag(channel, entry.windowKey), unavailable)
            val displayNote = if (unavailable) "不可用" else note
            if (displayNote == null) {
                views.setViewVisibility(cardNotes[i], View.GONE)
            } else {
                views.setViewVisibility(cardNotes[i], View.VISIBLE)
                views.setTextViewText(cardNotes[i], displayNote)
                views.setTextColor(cardNotes[i], if (displayNote == "不可用") RED else AMBER)
            }
            views.setOnClickPendingIntent(
                cardCells[i],
                channelIntent(context, entry.platformId, appWidgetId * 100 + i),
            )
        }

        views.setTextViewText(R.id.card_earliest, "最早 ${WidgetFormatter.earliest(channels)}")
        views.setTextViewText(
            R.id.card_status,
            WidgetFormatter.status(values, stale, lastResult, refreshing),
        )
        views.setTextColor(
            R.id.card_status,
            when (WidgetFormatter.tone(values, stale, lastResult, refreshing)) {
                WidgetFormatter.Tone.ERROR -> RED
                WidgetFormatter.Tone.WARN -> AMBER
                WidgetFormatter.Tone.NORMAL -> MUTED
            },
        )
        views.setOnClickPendingIntent(
            R.id.card_refresh,
            refreshIntent(context, appWidgetId, WidgetKind.CARD),
        )
        views.setContentDescription(R.id.card_refresh, "刷新同步组")
    }

    private fun setTag(views: RemoteViews, id: Int, text: String?, unavailable: Boolean) {
        if (text.isNullOrBlank()) {
            views.setViewVisibility(id, View.GONE)
            return
        }
        views.setViewVisibility(id, View.VISIBLE)
        views.setTextViewText(id, text)
        views.setTextColor(
            id,
            when {
                unavailable -> MUTED
                text == "周" -> AMBER
                text == "5h" -> BLUE
                text == "月" -> MINT
                else -> MUTED
            },
        )
    }

    private fun emptyAction(
        context: Context,
        appWidgetId: Int,
        kind: WidgetKind,
        action: String,
    ): PendingIntent {
        return if (action == "选择条目") {
            configIntent(context, appWidgetId, kind, appWidgetId * 100 + 90)
        } else {
            appIntent(context, null, appWidgetId * 100 + 91)
        }
    }

    private fun channelIntent(
        context: Context,
        platformId: String,
        requestCode: Int,
    ): PendingIntent = appIntent(context, platformId, requestCode)

    private fun appIntent(
        context: Context,
        platformId: String?,
        requestCode: Int,
    ): PendingIntent {
        val intent = Intent(context, MainActivity::class.java).apply {
            addFlags(
                Intent.FLAG_ACTIVITY_NEW_TASK or
                    Intent.FLAG_ACTIVITY_CLEAR_TOP or
                    Intent.FLAG_ACTIVITY_SINGLE_TOP
            )
            if (platformId != null) putExtra(EXTRA_CHANNEL_ID, platformId)
        }
        return PendingIntent.getActivity(
            context,
            requestCode,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun configIntent(
        context: Context,
        appWidgetId: Int,
        kind: WidgetKind,
        requestCode: Int,
    ): PendingIntent {
        val intent = Intent(context, WidgetConfigActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            putExtra(EXTRA_APPWIDGET_ID, appWidgetId)
            putExtra(EXTRA_KIND, kind.name)
        }
        return PendingIntent.getActivity(
            context,
            requestCode,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun refreshIntent(
        context: Context,
        appWidgetId: Int,
        kind: WidgetKind,
    ): PendingIntent {
        val provider = when (kind) {
            WidgetKind.STRIP -> StripWidgetProvider::class.java
            WidgetKind.CARD -> CardWidgetProvider::class.java
        }
        val intent = Intent(context, provider).apply {
            action = ACTION_REFRESH
            putExtra(EXTRA_APPWIDGET_ID, appWidgetId)
        }
        return PendingIntent.getBroadcast(
            context,
            appWidgetId * 100 + 99,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun initials(name: String): String {
        val text = name.trim()
        if (text.isEmpty()) return "?"
        val first = text.first()
        return if (first.isDigit()) text.take(2) else text.take(1).uppercase()
    }

    private data class EmptyState(
        val title: String,
        val subtitle: String,
        val action: String,
    )
}
