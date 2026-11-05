package com.wacil.grandettoken.sync.sync

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

val SyncJson: Json = Json {
    ignoreUnknownKeys = true
    encodeDefaults = true
    explicitNulls = false
}

@Serializable
enum class ChannelKind {
    @SerialName("money") Money,
    @SerialName("quota") Quota,
    @SerialName("points") Points,
}

@Serializable
enum class CheckinStatus {
    @SerialName("done") Done,
    @SerialName("pending") Pending,
    @SerialName("unsupported") Unsupported,
    @SerialName("unknown") Unknown,
}

@Serializable
data class SyncSnapshot(
    val v: Int,
    val source: String,
    val seq: Long,
    val at: Long,
    val app: String = "",
    val channels: List<ChannelSnapshot> = emptyList(),
    val checkin: CheckinSnapshot? = null,
    val totals: TotalsSnapshot = TotalsSnapshot(),
)

@Serializable
data class ChannelSnapshot(
    val platformId: String,
    val id: String,
    val name: String,
    val kind: ChannelKind,
    val unit: String,
    val remaining: String? = null,
    val total: String? = null,
    val percent: String? = null,
    val windows: List<WindowSnapshot> = emptyList(),
    val expiring: List<ExpiringSnapshot> = emptyList(),
    val day: String? = null,
    val week: String? = null,
    val month: String? = null,
    val estimated: Boolean = false,
    val stale: Boolean = false,
    val errorCode: String? = null,
    val updatedAt: Long,
    val history: List<HistoryPoint> = emptyList(),
)

@Serializable
data class WindowSnapshot(
    val label: String,
    val remainPercent: String? = null,
    val resetAt: Long? = null,
    val resetText: String? = null,
    /** 稳定窗口键；旧电脑端缺省为空。 */
    val key: String = "",
    /** 电脑端主窗；Codex=周、OpenCode Go=月。 */
    val main: Boolean = false,
)

@Serializable
data class ExpiringSnapshot(
    val amount: String,
    val at: Long,
)

@Serializable
data class HistoryPoint(
    val periodStart: Long,
    val periodEnd: Long,
    val timeZone: String,
    val unit: String,
    val balance: String? = null,
    val spent: String? = null,
    val estimated: Boolean = false,
    /** false = 当天采样有缺口；缺省 true 兼容旧载荷。 */
    val complete: Boolean = true,
    val revisedAt: Long,
)

@Serializable
data class CheckinSnapshot(
    val date: String,
    val timeZone: String,
    val items: List<CheckinItem> = emptyList(),
    val done: Int,
    val total: Int,
    val unsupported: Int = 0,
    val unknown: Int = 0,
)

@Serializable
data class CheckinItem(
    val itemId: String,
    val platformId: String,
    val status: CheckinStatus,
    val observedAt: Long,
)

@Serializable
data class TotalsSnapshot(
    val cny: String? = null,
    val others: List<OtherMoney> = emptyList(),
    val today: String? = null,
)

@Serializable
data class OtherMoney(
    val unit: String,
    val amount: String,
)

@Serializable
data class MergedSnapshot(
    val channels: List<ChannelSnapshot> = emptyList(),
    val checkin: CheckinSnapshot? = null,
    val totals: TotalsSnapshot = TotalsSnapshot(),
    val sources: Int = 0,
    val failedSources: List<String> = emptyList(),
    val generatedAt: Long = 0,
    val staleChannels: Int = 0,
)
