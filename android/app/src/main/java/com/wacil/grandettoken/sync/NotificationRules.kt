package com.wacil.grandettoken.sync

import com.wacil.grandettoken.sync.sync.ChannelKind
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import com.wacil.grandettoken.sync.sync.Decimal
import com.wacil.grandettoken.sync.sync.MergedSnapshot

/** 通知规则保持纯函数，便于单测；通知文案不包含具体余额、账号或写入者。 */
internal object NotificationRules {
    const val LOW_MONEY_MICROS = 10_000_000L
    const val LOW_PERCENT_MICROS = 20_000_000L
    const val STALE_SECONDS = 2 * 3600L
    const val FAILURE_STREAK = 3

    fun lowBalance(merged: MergedSnapshot?): List<ChannelSnapshot> =
        merged?.channels.orEmpty().filter(::isLowBalance)

    fun stale(merged: MergedSnapshot?, now: Long): List<ChannelSnapshot> =
        merged?.channels.orEmpty().filter { ch ->
            ch.stale ||
                ch.errorCode != null ||
                (ch.updatedAt > 0 && now - ch.updatedAt >= STALE_SECONDS)
        }

    fun failureReached(streak: Int): Boolean = streak >= FAILURE_STREAK

    private fun isLowBalance(ch: ChannelSnapshot): Boolean {
        val value = when (ch.kind) {
            ChannelKind.Money -> if (ch.unit == "CNY") Decimal.parseMicros(ch.remaining) else null
            ChannelKind.Quota -> Decimal.parseMicros(ch.percent)
            ChannelKind.Points -> null
        } ?: return false
        val threshold = if (ch.kind == ChannelKind.Quota) LOW_PERCENT_MICROS else LOW_MONEY_MICROS
        return value <= threshold
    }
}
