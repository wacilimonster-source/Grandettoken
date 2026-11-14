package com.wacil.grandettoken.sync.widget

import com.wacil.grandettoken.sync.ChannelFreshness
import com.wacil.grandettoken.sync.sync.ChannelKind
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import com.wacil.grandettoken.sync.sync.Decimal
import com.wacil.grandettoken.sync.sync.WindowSnapshot
import java.text.SimpleDateFormat
import java.util.Calendar
import java.util.Date
import java.util.Locale

/** 小部件文案与可信度判断；不补齐缺失值，不把旧值写成“刚刚”。 */
internal object WidgetFormatter {
    /**
     * windowKey = null 时是旧选择/旧电脑端：优先主窗，再退回第一个窗口，
     * 保持“OpenCode 月、Codex 周”的新默认；没有 main 的旧载荷仍按旧行为显示。
     */
    fun value(channel: ChannelSnapshot?, windowKey: String? = null): String {
        if (channel == null) return "—"
        return when (channel.kind) {
            ChannelKind.Money -> channel.remaining
                ?.let { Decimal.display(it, channel.unit) }
                ?: "—"
            ChannelKind.Quota -> quotaValue(channel, windowKey)
            ChannelKind.Points -> pointsValue(channel)
        }
    }

    fun note(channel: ChannelSnapshot?, now: Long): String? {
        if (channel == null) return "不可用"
        return when (ChannelFreshness.state(channel, now)) {
            ChannelFreshness.State.Fresh -> null
            ChannelFreshness.State.Failed -> "失败"
            ChannelFreshness.State.Stale, ChannelFreshness.State.Never -> "较旧"
        }
    }

    fun isIncomplete(values: List<String?>): Boolean = values.any { it == "—" }

    fun hasStale(channels: List<ChannelSnapshot?>, now: Long): Boolean =
        channels.filterNotNull().any { note(it, now) != null }

    fun earliest(channels: List<ChannelSnapshot?>): String {
        val oldest = channels
            .filterNotNull()
            .filter { value(it) != "—" }
            .minOfOrNull { it.updatedAt }
            ?: return "—"
        return formatTime(oldest)
    }

    fun status(
        values: List<String?>,
        stale: Boolean,
        lastResult: String?,
        refreshing: Boolean,
    ): String {
        if (refreshing) return "刷新中…"
        return when (lastResult) {
            "offline" -> "离线"
            "error" -> "同步失败"
            "rate" -> "稍后重试"
            else -> when {
                isIncomplete(values) -> "数据不完整"
                stale -> "有渠道较旧"
                else -> "数据正常"
            }
        }
    }

    fun sideLabel(
        values: List<String?>,
        stale: Boolean,
        lastResult: String?,
        refreshing: Boolean,
    ): String = when {
        refreshing -> "刷新中"
        lastResult == "offline" || lastResult == "error" -> "失败"
        lastResult == "rate" -> "限流"
        isIncomplete(values) -> "不完整"
        stale -> "较旧"
        else -> "最早"
    }

    fun tone(
        values: List<String?>,
        stale: Boolean,
        lastResult: String?,
        refreshing: Boolean,
    ): Tone {
        if (refreshing || lastResult == "rate") return Tone.WARN
        if (lastResult == "offline" || lastResult == "error") return Tone.ERROR
        if (isIncomplete(values)) return Tone.WARN
        if (stale) return Tone.WARN
        return Tone.NORMAL
    }

    fun formatTime(timestamp: Long): String {
        if (timestamp <= 0 || timestamp > System.currentTimeMillis() / 1000 + 3600) {
            return "时间异常"
        }
        val now = Calendar.getInstance()
        val then = Calendar.getInstance().apply { timeInMillis = timestamp * 1000 }
        val sameDay = now.get(Calendar.YEAR) == then.get(Calendar.YEAR) &&
            now.get(Calendar.DAY_OF_YEAR) == then.get(Calendar.DAY_OF_YEAR)
        val pattern = if (sameDay) "HH:mm" else "MM-dd HH:mm"
        return SimpleDateFormat(pattern, Locale.getDefault()).format(Date(timestamp * 1000))
    }

    /** 小部件上的短标签：只表达窗口，不重复“剩/额度”。 */
    fun tag(channel: ChannelSnapshot?, windowKey: String?): String? {
        if (channel == null) return tagForKey(windowKey)
        val window = windowFor(channel, windowKey)
        val key = windowKey?.takeIf { it.isNotBlank() } ?: window?.key.orEmpty()
        tagForKey(key)?.let { return it }
        return when {
            window?.label == "本周" || window?.label == "周" -> "周"
            window?.label == "5 小时" -> "5h"
            window?.label == "周期" || window?.label == "本月" ||
                window?.label?.contains("月") == true -> "月"
            window?.label == "当日" -> "日"
            else -> null
        }
    }

    private fun tagForKey(key: String?): String? = when {
        key.isNullOrBlank() -> null
        key == "monthly" -> "月"
        key == "weekly" -> "周"
        key == "5h" || key == "rolling" -> "5h"
        key == "daily" -> "日"
        key == "gemini-weekly" -> "G周"
        key == "gemini-5h" -> "G5h"
        key == "3p-weekly" -> "AI周"
        key == "3p-5h" -> "AI5h"
        key.contains("argon") && key.endsWith("-weekly") -> "Ag周"
        key.endsWith("-weekly") -> "周"
        key.endsWith("-5h") -> "5h"
        key.endsWith("-daily") -> "日"
        key.endsWith("-monthly") -> "月"
        else -> null
    }

    fun windowFor(channel: ChannelSnapshot, windowKey: String?): WindowSnapshot? {
        if (!windowKey.isNullOrBlank()) {
            return channel.windows.firstOrNull { it.key == windowKey }
        }
        return channel.windows.firstOrNull { it.main }
            ?: channel.windows.firstOrNull { !it.remainPercent.isNullOrBlank() }
            ?: channel.windows.firstOrNull()
    }

    fun configLabel(channel: ChannelSnapshot, windowKey: String?): String {
        val window = windowFor(channel, windowKey)
        val key = windowKey?.takeIf { it.isNotBlank() } ?: window?.key.orEmpty()
        return when {
            key == "weekly" || window?.label == "本周" -> "周额度"
            key == "5h" || key == "rolling" || window?.label == "5 小时" -> "5 小时"
            key == "monthly" || window?.label == "周期" || window?.label == "本月" ->
                "月额度(按套餐周期)"
            key == "daily" || window?.label == "当日" -> "当日额度"
            window?.label != null -> window.label
            else -> "主额度"
        }
    }

    private fun quotaValue(channel: ChannelSnapshot, windowKey: String?): String {
        if (windowKey.isNullOrBlank()) {
            val window = windowFor(channel, null)
            if (window != null) {
                return percentText(window.remainPercent) ?: "—"
            }
            return percentText(channel.percent) ?: "—"
        }
        val window = windowFor(channel, windowKey) ?: return "—"
        return percentText(window.remainPercent) ?: "—"
    }

    private fun percentText(value: String?): String? {
        val text = Decimal.display(value, "%")
        return when {
            text.endsWith(".0%") -> text.removeSuffix(".0%") + "%"
            else -> text
        }
    }

    private fun pointsValue(channel: ChannelSnapshot): String {
        if (channel.remaining == null) return "—"
        // 与 PC 主数字、App 内一致:积分型数值后缀统一「分」,不再在小组件上改写为「积分」
        return Decimal.display(channel.remaining, channel.unit, points = true)
            .replace(" 积分", " 分")
    }

    enum class Tone { NORMAL, WARN, ERROR }
}
