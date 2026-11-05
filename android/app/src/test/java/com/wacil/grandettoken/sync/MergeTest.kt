package com.wacil.grandettoken.sync

import com.wacil.grandettoken.sync.sync.ChannelKind
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import com.wacil.grandettoken.sync.sync.CheckinItem
import com.wacil.grandettoken.sync.sync.CheckinSnapshot
import com.wacil.grandettoken.sync.sync.CheckinStatus
import com.wacil.grandettoken.sync.sync.Decimal
import com.wacil.grandettoken.sync.sync.HistoryPoint
import com.wacil.grandettoken.sync.sync.MergedSnapshot
import com.wacil.grandettoken.sync.sync.SyncMerge
import com.wacil.grandettoken.sync.sync.SyncSnapshot
import com.wacil.grandettoken.sync.sync.TotalsSnapshot
import org.junit.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

class MergeTest {
    private fun channel(
        platform: String,
        updated: Long,
        remaining: String,
        stale: Boolean = false,
        history: List<HistoryPoint> = emptyList(),
    ) = ChannelSnapshot(
        platformId = platform,
        id = platform,
        name = platform,
        kind = ChannelKind.Money,
        unit = "CNY",
        remaining = remaining,
        day = "1.2",
        stale = stale,
        updatedAt = updated,
        history = history,
    )

    private fun snap(
        source: String,
        seq: Long,
        channels: List<ChannelSnapshot>,
        checkin: CheckinSnapshot? = null,
    ) = SyncSnapshot(
        v = 1,
        source = source,
        seq = seq,
        at = 1_793_500_000,
        app = "0.1.0",
        channels = channels,
        checkin = checkin,
        totals = TotalsSnapshot(),
    )

    @Test
    fun newest_whole_record_wins_and_totals_are_recomputed() {
        val a = snap(
            "s_0123456789abcdef0123456789abcdef",
            1,
            listOf(channel("deepseek", 100, "10")),
        )
        val b = snap(
            "s_0123456789abcdef0123456789abcdee",
            1,
            listOf(channel("deepseek", 200, "20")),
        )
        val m = SyncMerge.merge(listOf(a, b))
        assertEquals(1, m.channels.size)
        assertEquals("20", m.channels[0].remaining)
        assertEquals("20", m.totals.cny)
        assertEquals("1.2", m.totals.today)
        assertEquals(2, m.sources)
    }

    @Test
    fun same_time_uses_source_lexicographic() {
        val a = snap(
            "s_0123456789abcdef0123456789abcdef",
            1,
            listOf(channel("deepseek", 100, "10")),
        )
        val b = snap(
            "s_0123456789abcdef0123456789abcdee",
            1,
            listOf(channel("deepseek", 100, "20")),
        )
        val m = SyncMerge.merge(listOf(a, b))
        assertEquals("20", m.channels[0].remaining)
    }

    @Test
    fun stale_newest_is_not_replaced_by_older_fresh() {
        val a = snap(
            "s_0123456789abcdef0123456789abcdef",
            1,
            listOf(channel("deepseek", 100, "10", stale = false)),
        )
        val b = snap(
            "s_0123456789abcdef0123456789abcdee",
            1,
            listOf(channel("deepseek", 200, "20", stale = true)),
        )
        val m = SyncMerge.merge(listOf(a, b))
        assertEquals("20", m.channels[0].remaining)
        assertTrue(m.channels[0].stale)
        assertEquals(1, m.staleChannels)
    }

    @Test
    fun history_and_checkin_are_deduped() {
        val p1 = HistoryPoint(100, 200, "+08:00", "CNY", spent = "1", revisedAt = 10)
        val p2 = HistoryPoint(100, 200, "+08:00", "CNY", spent = "2", revisedAt = 20)
        val ck1 = CheckinSnapshot(
            date = "2026-11-02",
            timeZone = "Asia/Shanghai",
            items = listOf(CheckinItem("deepseek:main", "deepseek", CheckinStatus.Pending, 10)),
            done = 0,
            total = 1,
        )
        val ck2 = CheckinSnapshot(
            date = "2026-11-02",
            timeZone = "Asia/Shanghai",
            items = listOf(CheckinItem("deepseek:main", "deepseek", CheckinStatus.Done, 20)),
            done = 1,
            total = 1,
        )
        val a = snap(
            "s_0123456789abcdef0123456789abcdef",
            1,
            listOf(channel("deepseek", 100, "10", history = listOf(p1))),
            ck1,
        )
        val b = snap(
            "s_0123456789abcdef0123456789abcdee",
            1,
            listOf(channel("deepseek", 200, "20", history = listOf(p2))),
            ck2,
        )
        val m = SyncMerge.merge(listOf(a, b))
        assertEquals(1, m.channels[0].history.size)
        assertEquals("2", m.channels[0].history[0].spent)
        val ck = assertNotNull(m.checkin)
        assertEquals(1, ck.done)
        assertEquals(1, ck.total)
        assertEquals(CheckinStatus.Done, ck.items[0].status)
    }

    @Test
    fun duplicate_source_conflict_is_reported_not_used() {
        val a = snap(
            "s_0123456789abcdef0123456789abcdef",
            1,
            listOf(channel("deepseek", 100, "10")),
        )
        val b = snap(
            "s_0123456789abcdef0123456789abcdef",
            1,
            listOf(channel("deepseek", 100, "20")),
        )
        val m = SyncMerge.merge(listOf(a, b))
        assertEquals(1, m.sources)
        assertTrue(m.failedSources.any { it.contains("SYNC_CONFLICT") })
    }

    @Test
    fun decimal_arithmetic_is_exact() {
        assertEquals("0.3", Decimal.sum(listOf("0.1", "0.2")))
        assertEquals("0.5", Decimal.sum(listOf("1.00", "-0.50")))
        assertEquals("1284.5", Decimal.formatMicros(1_284_500_000))
        assertEquals(null, Decimal.sum(listOf("NaN", "1e3")))
        assertEquals("88", Decimal.fromDouble(88.0))
    }

    @Test
    fun partial_failure_keeps_previous_channel_and_marks_it_stale() {
        val previous = MergedSnapshot(
            channels = listOf(
                channel("codex", 100, "65"),
                channel("deepseek", 100, "10"),
            ),
            totals = TotalsSnapshot(cny = "75", today = "2.4"),
            sources = 2,
        )
        val next = MergedSnapshot(
            channels = listOf(channel("deepseek", 200, "20")),
            totals = TotalsSnapshot(cny = "20", today = "1.2"),
            sources = 1,
            failedSources = listOf("s_0123456789abcdef0123456789abcdef:SYNC_DECRYPT_FAILED"),
        )
        val kept = SyncMerge.retainMissing(next, previous)
        assertEquals(2, kept.channels.size)
        val codex = kept.channels.first { it.platformId == "codex" }
        assertEquals("65", codex.remaining)
        assertTrue(codex.stale)
        assertEquals("20", kept.channels.first { it.platformId == "deepseek" }.remaining)
        assertEquals("85", kept.totals.cny)
        assertEquals("2.4", kept.totals.today)
        assertEquals(1, kept.staleChannels)
    }

    @Test
    fun all_sources_fail_keeps_previous_only() {
        val previous = MergedSnapshot(channels = listOf(channel("codex", 100, "65")))
        val next = MergedSnapshot(
            failedSources = listOf("s_0123456789abcdef0123456789abcdef:SYNC_DECRYPT_FAILED"),
        )
        val kept = SyncMerge.retainMissing(next, previous)
        assertEquals(1, kept.channels.size)
        assertEquals("65", kept.channels[0].remaining)
        assertTrue(kept.channels[0].stale)
    }

    @Test
    fun no_retention_when_caller_has_nothing_to_keep() {
        // 没有来源失败/被跳过时调用方不传 previous：桌面真正删除的渠道应当消失。
        val next = MergedSnapshot(channels = listOf(channel("deepseek", 200, "20")))
        assertEquals(1, SyncMerge.retainMissing(next, null).channels.size)
    }
}
