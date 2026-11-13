package com.wacil.grandettoken.sync

import com.wacil.grandettoken.sync.sync.ChannelSnapshot

/**
 * 渠道卡与详情页共用的新鲜度规则。
 *
 * 卡片只在「数据已经不可信」时开口:正常数据不报时,避免每张卡都写一次「刚刚」。
 * [now] 作为入参而不是直接取系统时间,是为了让边界(恰好 2 小时、59 秒)可被单测钉住。
 */
internal object ChannelFreshness {
    /** 与通知、小组件同一口径:超过 2 小时才算「较旧」。 */
    const val STALE_SECONDS = 2 * 60 * 60L

    /** 失败态下,成功快照不足 1 分钟时不写年龄,避免又出现「刚刚」。 */
    private const val FAILED_AGE_MIN_SECONDS = 60L

    enum class State { Fresh, Stale, Failed, Never }

    fun state(ch: ChannelSnapshot, now: Long): State = when {
        ch.errorCode != null -> State.Failed
        // 先用 stale 短路:被标记为「本轮没更新」时,即使时间戳异常也按较旧处理。
        ch.stale || (ch.updatedAt > 0 && age(ch, now) >= STALE_SECONDS) -> State.Stale
        ch.updatedAt <= 0 -> State.Never
        else -> State.Fresh
    }

    /** 卡片副标题的追加项；null 表示正常,不显示任何时间。 */
    fun timeLabel(ch: ChannelSnapshot, now: Long): String? = when (state(ch, now)) {
        State.Fresh -> null
        State.Never -> "从未更新"
        State.Stale -> ageLabel(ch, now, STALE_SECONDS)?.let { "数据较旧 · $it" } ?: "数据较旧"
        State.Failed -> ageLabel(ch, now, FAILED_AGE_MIN_SECONDS)?.let { "取数失败 · 上次 $it" } ?: "取数失败"
    }

    /** 成功快照的年龄文案；不足 [minimumSeconds] 或时间戳缺失时返回 null。 */
    fun ageLabel(ch: ChannelSnapshot, now: Long, minimumSeconds: Long = FAILED_AGE_MIN_SECONDS): String? {
        if (ch.updatedAt <= 0) return null
        if (age(ch, now) < minimumSeconds) return null
        return relativeTime(ch.updatedAt, now)
    }

    private fun age(ch: ChannelSnapshot, now: Long): Long = (now - ch.updatedAt).coerceAtLeast(0)
}

/** 相对时间；[now] 可注入,便于单测边界。 */
internal fun relativeTime(ts: Long, now: Long = System.currentTimeMillis() / 1000): String {
    if (ts <= 0) return "—"
    val delta = (now - ts).coerceAtLeast(0)
    return when {
        delta < 60 -> "刚刚"
        delta < 3600 -> "${delta / 60} 分钟前"
        delta < 86_400 -> "${delta / 3600} 小时前"
        else -> "${delta / 86_400} 天前"
    }
}
