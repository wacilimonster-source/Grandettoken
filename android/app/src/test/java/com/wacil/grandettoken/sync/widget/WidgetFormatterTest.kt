package com.wacil.grandettoken.sync.widget

import com.wacil.grandettoken.sync.sync.ChannelKind
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import com.wacil.grandettoken.sync.sync.WindowSnapshot
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class WidgetFormatterTest {
    private fun channel(
        id: String,
        kind: ChannelKind,
        unit: String,
        remaining: String? = null,
        percent: String? = null,
        windows: List<WindowSnapshot> = emptyList(),
        updatedAt: Long = 1_000,
        stale: Boolean = false,
        errorCode: String? = null,
    ) = ChannelSnapshot(
        platformId = id,
        id = id,
        name = id,
        kind = kind,
        unit = unit,
        remaining = remaining,
        percent = percent,
        windows = windows,
        stale = stale,
        errorCode = errorCode,
        updatedAt = updatedAt,
    )

    private fun quotaWithWindows() = channel(
        "codex",
        ChannelKind.Quota,
        "%",
        percent = "65",
        windows = listOf(
            WindowSnapshot("周", "65", key = "weekly", main = true),
            WindowSnapshot("5 小时", "78", key = "5h"),
            WindowSnapshot("月额度", "82", key = "monthly"),
        ),
    )

    @Test
    fun formats_money_quota_and_points_without_inventing_values() {
        assertEquals(
            "¥76.50",
            WidgetFormatter.value(channel("money", ChannelKind.Money, "CNY", remaining = "76.5")),
        )
        assertEquals(
            "65%",
            WidgetFormatter.value(quotaWithWindows(), "weekly"),
        )
        assertEquals(
            "78%",
            WidgetFormatter.value(quotaWithWindows(), "5h"),
        )
        assertEquals("—", WidgetFormatter.value(quotaWithWindows(), "missing"))
        assertEquals("65%", WidgetFormatter.value(quotaWithWindows()))
        assertEquals(
            "148 分",
            WidgetFormatter.value(channel("points", ChannelKind.Points, "credits", remaining = "148")),
        )
        assertEquals("—", WidgetFormatter.value(null))
        assertEquals("—", WidgetFormatter.value(channel("empty", ChannelKind.Money, "CNY")))
    }

    @Test
    fun emits_short_window_tags_from_stable_keys() {
        val codex = quotaWithWindows()
        assertEquals("周", WidgetFormatter.tag(codex, "weekly"))
        assertEquals("5h", WidgetFormatter.tag(codex, "5h"))
        assertEquals("月", WidgetFormatter.tag(codex, "monthly"))
        assertEquals("周", WidgetFormatter.tag(codex, null))
        assertEquals("周额度", WidgetFormatter.configLabel(codex, "weekly"))
        val openCode = channel(
            "opencode-go",
            ChannelKind.Quota,
            "%",
            percent = "82",
            windows = listOf(WindowSnapshot("周期", "82", key = "monthly", main = true)),
        )
        assertEquals("月", WidgetFormatter.tag(openCode, "monthly"))
        assertEquals("月", WidgetFormatter.tag(openCode, null))
        assertEquals("月额度(按套餐周期)", WidgetFormatter.configLabel(openCode, "monthly"))
        val ag = channel(
            "antigravity",
            ChannelKind.Quota,
            "%",
            percent = "80.4",
            windows = listOf(
                WindowSnapshot("Gemini 本周", "80.4", key = "gemini-weekly", main = true),
                WindowSnapshot("Gemini 5 小时", "32.1", key = "gemini-5h"),
                WindowSnapshot("Claude/GPT 5 小时", "0", key = "3p-5h"),
                WindowSnapshot("Gemini 4 Argon 本周", "30.7", key = "gemini-argon-wtu-weekly"),
            ),
        )
        assertEquals("80.4%", WidgetFormatter.value(ag))
        assertEquals("0%", WidgetFormatter.value(ag, "3p-5h"))
        assertEquals("G周", WidgetFormatter.tag(ag, "gemini-weekly"))
        assertEquals("G周", WidgetFormatter.tag(ag, null))
        assertEquals("G5h", WidgetFormatter.tag(ag, "gemini-5h"))
        assertEquals("AI5h", WidgetFormatter.tag(ag, "3p-5h"))
        assertEquals("Ag周", WidgetFormatter.tag(ag, "gemini-argon-wtu-weekly"))
        assertEquals("Claude/GPT 5 小时", WidgetFormatter.configLabel(ag, "3p-5h"))
    }

    @Test
    fun notes_missing_stale_and_failed_channels() {
        val now = 10_000L
        assertEquals("不可用", WidgetFormatter.note(null, now))
        assertEquals(
            "较旧",
            WidgetFormatter.note(channel("old", ChannelKind.Money, "CNY", "1", updatedAt = 1), now),
        )
        assertEquals(
            "失败",
            WidgetFormatter.note(
                channel("failed", ChannelKind.Money, "CNY", "1", updatedAt = now, errorCode = "X"),
                now,
            ),
        )
        assertEquals(
            null,
            WidgetFormatter.note(channel("fresh", ChannelKind.Money, "CNY", "1", updatedAt = now), now),
        )
    }

    @Test
    fun uses_earliest_collected_time_and_reports_incomplete_data() {
        val a = channel("a", ChannelKind.Money, "CNY", "1", updatedAt = 1_000)
        val b = channel("b", ChannelKind.Money, "CNY", "2", updatedAt = 2_000)
        assertEquals(WidgetFormatter.formatTime(1_000), WidgetFormatter.earliest(listOf(a, b)))
        assertTrue(WidgetFormatter.isIncomplete(listOf("65%", "—")))
        assertFalse(WidgetFormatter.isIncomplete(listOf("65%", "82%")))
    }

    @Test
    fun status_prioritizes_refresh_and_failure_states() {
        assertEquals("刷新中…", WidgetFormatter.status(listOf("65%"), false, null, true))
        assertEquals("离线", WidgetFormatter.status(listOf("65%"), false, "offline", false))
        assertEquals("同步失败", WidgetFormatter.status(listOf("65%"), false, "error", false))
        assertEquals("数据不完整", WidgetFormatter.status(listOf("65%", "—"), false, null, false))
        assertEquals("有渠道较旧", WidgetFormatter.status(listOf("65%"), true, null, false))
        assertEquals("数据正常", WidgetFormatter.status(listOf("65%"), false, "ok", false))
    }
}
