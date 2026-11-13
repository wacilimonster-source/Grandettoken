package com.wacil.grandettoken.sync

import com.wacil.grandettoken.sync.sync.ChannelKind
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull

class ChannelFreshnessTest {
    private val now = 1_700_000_000L

    private fun channel(
        updatedAt: Long = now - 30,
        stale: Boolean = false,
        errorCode: String? = null,
    ) = ChannelSnapshot(
        platformId = "test",
        id = "test",
        name = "test",
        kind = ChannelKind.Money,
        unit = "CNY",
        remaining = "100",
        stale = stale,
        errorCode = errorCode,
        updatedAt = updatedAt,
    )

    @Test
    fun fresh_cards_do_not_emit_a_time_label() {
        val fresh = channel(updatedAt = now - 30)
        assertEquals(ChannelFreshness.State.Fresh, ChannelFreshness.state(fresh, now))
        assertNull(ChannelFreshness.timeLabel(fresh, now))
    }

    @Test
    fun threshold_is_exactly_two_hours() {
        val justUnder = channel(updatedAt = now - ChannelFreshness.STALE_SECONDS + 1)
        assertNull(ChannelFreshness.timeLabel(justUnder, now))

        val atThreshold = channel(updatedAt = now - ChannelFreshness.STALE_SECONDS)
        assertEquals("数据较旧 · 2 小时前", ChannelFreshness.timeLabel(atThreshold, now))
    }

    @Test
    fun stale_flag_under_threshold_shows_status_without_age() {
        val kept = channel(updatedAt = now - 5 * 60, stale = true)
        assertEquals(ChannelFreshness.State.Stale, ChannelFreshness.state(kept, now))
        assertEquals("数据较旧", ChannelFreshness.timeLabel(kept, now))
    }

    @Test
    fun stale_flag_over_threshold_adds_age() {
        val old = channel(updatedAt = now - 3 * 3600, stale = true)
        assertEquals("数据较旧 · 3 小时前", ChannelFreshness.timeLabel(old, now))
    }

    @Test
    fun failure_shows_last_success_only_after_one_minute() {
        val justFailed = channel(updatedAt = now - 30, errorCode = "SYNC_UPSTREAM")
        assertEquals("取数失败", ChannelFreshness.timeLabel(justFailed, now))

        val failedAWhile = channel(updatedAt = now - 25 * 60, errorCode = "SYNC_UPSTREAM")
        assertEquals("取数失败 · 上次 25 分钟前", ChannelFreshness.timeLabel(failedAWhile, now))

        val failedLong = channel(updatedAt = now - 3 * 3600, errorCode = "SYNC_UPSTREAM")
        assertEquals("取数失败 · 上次 3 小时前", ChannelFreshness.timeLabel(failedLong, now))
    }

    @Test
    fun never_missing_and_stale_zero_timestamps_do_not_emit_a_dash() {
        val never = channel(updatedAt = 0)
        assertEquals(ChannelFreshness.State.Never, ChannelFreshness.state(never, now))
        assertEquals("从未更新", ChannelFreshness.timeLabel(never, now))

        val staleWithoutTimestamp = channel(updatedAt = 0, stale = true)
        assertEquals("数据较旧", ChannelFreshness.timeLabel(staleWithoutTimestamp, now))

        val failedWithoutTimestamp = channel(updatedAt = 0, errorCode = "SYNC_UPSTREAM")
        assertEquals("取数失败", ChannelFreshness.timeLabel(failedWithoutTimestamp, now))
    }

    @Test
    fun future_timestamp_is_treated_as_fresh() {
        val skewed = channel(updatedAt = now + 500)
        assertEquals(ChannelFreshness.State.Fresh, ChannelFreshness.state(skewed, now))
        assertNull(ChannelFreshness.timeLabel(skewed, now))
    }

    @Test
    fun card_labels_never_contain_just_now() {
        for (offset in 0..7 * 24 * 3600 step 30) {
            val candidates = listOf(
                channel(updatedAt = now - offset),
                channel(updatedAt = now - offset, stale = true),
                channel(updatedAt = now - offset, errorCode = "SYNC_UPSTREAM"),
            )
            for (candidate in candidates) {
                val label = ChannelFreshness.timeLabel(candidate, now)
                assertFalse(label?.contains("刚刚") == true, "offset=$offset label=$label")
            }
        }
    }

    @Test
    fun generic_relative_time_keeps_existing_vocabulary() {
        assertEquals("刚刚", relativeTime(now - 30, now))
        assertEquals("25 分钟前", relativeTime(now - 25 * 60, now))
        assertEquals("3 小时前", relativeTime(now - 3 * 3600, now))
        assertEquals("2 天前", relativeTime(now - 2 * 86_400, now))
    }
}
