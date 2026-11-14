package com.wacil.grandettoken.sync.widget

import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProviderInfo
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.wacil.grandettoken.sync.GrandettokenTheme
import com.wacil.grandettoken.sync.sync.ChannelKind
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import com.wacil.grandettoken.sync.sync.GroupRepository
import com.wacil.grandettoken.sync.sync.MergedSnapshot
import com.wacil.grandettoken.sync.sync.WindowSnapshot

class WidgetConfigActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val appWidgetId = intent.getIntExtra(
            AppWidgetManager.EXTRA_APPWIDGET_ID,
            AppWidgetManager.INVALID_APPWIDGET_ID,
        )
        if (appWidgetId == AppWidgetManager.INVALID_APPWIDGET_ID) {
            setResult(RESULT_CANCELED)
            finish()
            return
        }
        val info = AppWidgetManager.getInstance(this).getAppWidgetInfo(appWidgetId)
        if (info == null) {
            setResult(RESULT_CANCELED)
            finish()
            return
        }
        val kind = WidgetStore.kind(this, appWidgetId) ?: inferKind(info)
        val repo = GroupRepository(this)
        val cache = if (repo.hasGroup()) repo.cached() else null
        val saved = WidgetStore.load(this, appWidgetId)

        setContent {
            GrandettokenTheme {
                WidgetConfigScreen(
                    kind = kind,
                    cache = cache,
                    saved = saved,
                    onCancel = {
                        setResult(RESULT_CANCELED)
                        finish()
                    },
                    onSave = { selection ->
                        WidgetStore.save(this, appWidgetId, kind, selection)
                        WidgetUpdater.update(this, appWidgetId, kind)
                        // 缓存为空/过旧时立即补拉一次(内部有 5 分钟节流),
                        // 不要让首次配置停在"等待首次同步"直到下个轮询周期。
                        WidgetSyncBridge.onWidgetShown(this)
                        setResult(
                            RESULT_OK,
                            Intent().putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, appWidgetId),
                        )
                        finish()
                    },
                )
            }
        }
    }

    private fun inferKind(info: AppWidgetProviderInfo): WidgetKind =
        when (info.provider?.className) {
            StripWidgetProvider::class.java.name -> WidgetKind.STRIP
            CardWidgetProvider::class.java.name -> WidgetKind.CARD
            else -> WidgetKind.STRIP
        }
}

private data class WidgetOption(
    val entry: WidgetEntry,
    val channel: ChannelSnapshot?,
    val valueText: String,
    val available: Boolean,
    val preferred: Boolean,
)

@Composable
private fun WidgetConfigScreen(
    kind: WidgetKind,
    cache: MergedSnapshot?,
    saved: List<WidgetEntry>,
    onCancel: () -> Unit,
    onSave: (List<WidgetEntry>) -> Unit,
) {
    val options = buildOptions(cache, saved)
    val byKey = options.associateBy { it.entry.entryKey }
    val normalizedSaved = saved.mapNotNull { old ->
        byKey[old.entryKey]
            ?: if (old.windowKey == null) {
                options.firstOrNull { it.entry.platformId == old.platformId && it.preferred }
                    ?: options.firstOrNull { it.entry.platformId == old.platformId }
            } else {
                null
            }
    }.distinctBy { it.entry.entryKey }
    val defaultOrder = if (normalizedSaved.isNotEmpty()) {
        normalizedSaved.map { it.entry.entryKey }
    } else {
        options.filter { it.preferred }
            .distinctBy { it.entry.platformId }
            .take(kind.limit)
            .map { it.entry.entryKey }
    }
    val separator = '\u0001'
    var orderText by rememberSaveable {
        mutableStateOf(defaultOrder.take(kind.limit).joinToString(separator.toString()))
    }
    var message by rememberSaveable { mutableStateOf<String?>(null) }
    val selectedKeys = orderText
        .split(separator)
        .filter { it.isNotBlank() }
        .distinct()
        .filter { byKey.containsKey(it) }
    val selectedOptions = selectedKeys.mapNotNull { byKey[it] }
    val groups = options.groupBy { it.entry.platformId }

    fun currentKeys(): MutableList<String> =
        orderText.split(separator).filter { it.isNotBlank() }.toMutableList()

    fun toggle(key: String, checked: Boolean) {
        message = null
        val current = currentKeys()
        if (checked) {
            if (current.size >= kind.limit) {
                message = "最多选择 ${kind.limit} 个条目,请先移除一个。"
                return
            }
            if (key !in current) current.add(key)
        } else {
            current.remove(key)
        }
        orderText = current.joinToString(separator.toString())
    }

    fun move(key: String, delta: Int) {
        val current = currentKeys()
        val from = current.indexOf(key)
        val to = from + delta
        if (from < 0 || to !in current.indices) return
        val tmp = current[from]
        current[from] = current[to]
        current[to] = tmp
        orderText = current.joinToString(separator.toString())
    }

    Surface(
        modifier = Modifier.fillMaxSize(),
        color = MaterialTheme.colorScheme.background,
    ) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(
                "选择显示的条目",
                color = MaterialTheme.colorScheme.onBackground,
                fontSize = 23.sp,
                fontWeight = FontWeight.Bold,
            )
            Text(
                "${kind.label} · 最多 ${kind.limit} 个 · 已选 ${selectedOptions.size} / ${kind.limit}",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                fontSize = 12.sp,
            )
            Text(
                "勾选渠道或窗口后用箭头调整显示顺序。同一 Codex 的周和 5 小时可以分别选择;同一窗口不能重复。",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                fontSize = 12.sp,
                lineHeight = 18.sp,
            )

            if (options.isEmpty()) {
                Spacer(Modifier.height(8.dp))
                Surface(
                    color = MaterialTheme.colorScheme.surfaceVariant,
                    shape = RoundedCornerShape(12.dp),
                ) {
                    Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                        Text(
                            "还没有可选择的渠道",
                            color = MaterialTheme.colorScheme.onSurface,
                            fontSize = 13.sp,
                            fontWeight = FontWeight.SemiBold,
                        )
                        Text(
                            "先打开 App 加入同步组并完成一次同步;保存后小部件会显示「等待首次同步」,之后可回来编辑。",
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            fontSize = 11.sp,
                            lineHeight = 17.sp,
                        )
                    }
                }
            } else {
                if (selectedOptions.isNotEmpty()) {
                    Text(
                        "已选 · 按此顺序显示",
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        fontSize = 10.sp,
                        fontWeight = FontWeight.SemiBold,
                    )
                    selectedOptions.forEachIndexed { index, option ->
                        val entry = option.entry
                        Surface(
                            color = MaterialTheme.colorScheme.surface,
                            shape = RoundedCornerShape(11.dp),
                        ) {
                            Row(
                                modifier = Modifier.fillMaxWidth().padding(horizontal = 10.dp, vertical = 5.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Text(
                                    "${index + 1}",
                                    color = MaterialTheme.colorScheme.primary,
                                    fontSize = 10.sp,
                                    modifier = Modifier.padding(end = 8.dp),
                                )
                                Column(Modifier.weight(1f)) {
                                    Text(
                                        entry.name,
                                        color = MaterialTheme.colorScheme.onSurface,
                                        fontSize = 12.sp,
                                        fontWeight = FontWeight.SemiBold,
                                    )
                                    Text(
                                        "${optionLabel(option)} · ${option.valueText}",
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        fontSize = 10.sp,
                                    )
                                }
                                TextButton(
                                    onClick = { move(entry.entryKey, -1) },
                                    enabled = index > 0,
                                    contentPadding = PaddingValues(horizontal = 5.dp),
                                ) { Text("↑", fontSize = 13.sp) }
                                TextButton(
                                    onClick = { move(entry.entryKey, 1) },
                                    enabled = index < selectedOptions.lastIndex,
                                    contentPadding = PaddingValues(horizontal = 5.dp),
                                ) { Text("↓", fontSize = 13.sp) }
                                TextButton(
                                    onClick = { toggle(entry.entryKey, false) },
                                    contentPadding = PaddingValues(horizontal = 5.dp),
                                ) { Text("×", color = MaterialTheme.colorScheme.error, fontSize = 14.sp) }
                            }
                        }
                    }
                    Spacer(Modifier.height(4.dp))
                }

                Text(
                    "可添加",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    fontSize = 10.sp,
                    fontWeight = FontWeight.SemiBold,
                )
                groups.entries.forEach { (platformId, group) ->
                    val title = group.first().entry.name
                    val type = when {
                        platformId == "codex" || platformId == "antigravity" -> "配额型 · 可多选窗口"
                        platformId == "opencode-go" -> "配额型 · 月额度"
                        group.first().channel?.kind == ChannelKind.Quota -> "配额型"
                        group.first().channel?.kind == ChannelKind.Points -> "积分型"
                        else -> "金额型"
                    }
                    Text(
                        "$title · $type",
                        color = MaterialTheme.colorScheme.onSurface,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier.padding(top = 4.dp),
                    )
                    group.forEach { option ->
                        val entry = option.entry
                        val checked = entry.entryKey in selectedKeys
                        val disabled = !checked && (
                            selectedKeys.size >= kind.limit || !option.available
                            )
                        Surface(
                            color = if (checked) {
                                MaterialTheme.colorScheme.primary.copy(alpha = 0.10f)
                            } else {
                                MaterialTheme.colorScheme.surface
                            },
                            shape = RoundedCornerShape(10.dp),
                        ) {
                            Row(
                                modifier = Modifier.fillMaxWidth().padding(horizontal = 6.dp, vertical = 1.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Checkbox(
                                    checked = checked,
                                    onCheckedChange = {
                                        if (!disabled) toggle(entry.entryKey, it)
                                    },
                                    enabled = !disabled || checked,
                                )
                                Text(
                                    optionLabel(option),
                                    modifier = Modifier.weight(1f),
                                    color = MaterialTheme.colorScheme.onSurface,
                                    fontSize = 12.sp,
                                )
                                Text(
                                    option.valueText,
                                    color = if (option.available) {
                                        MaterialTheme.colorScheme.onSurface
                                    } else {
                                        MaterialTheme.colorScheme.onSurfaceVariant
                                    },
                                    fontSize = 11.sp,
                                    fontWeight = FontWeight.Medium,
                                )
                            }
                        }
                    }
                }
            }

            message?.let {
                Text(it, color = MaterialTheme.colorScheme.error, fontSize = 11.sp)
            }
            Spacer(Modifier.height(6.dp))
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                TextButton(
                    onClick = onCancel,
                    modifier = Modifier.weight(1f),
                ) { Text("取消") }
                Button(
                    onClick = {
                        val selection = selectedOptions.map { it.entry }
                        if (options.isNotEmpty() && selection.isEmpty()) {
                            message = "请至少选择一个条目。"
                        } else {
                            onSave(selection)
                        }
                    },
                    modifier = Modifier.weight(2f),
                    enabled = options.isEmpty() || selectedOptions.isNotEmpty(),
                ) { Text("保存") }
            }
            Text(
                "小部件只显示所选渠道/窗口的同步结果,不显示同步码、Token 或密钥。",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                fontSize = 10.sp,
                lineHeight = 15.sp,
            )
        }
    }
}

private fun buildOptions(
    cache: MergedSnapshot?,
    saved: List<WidgetEntry>,
): List<WidgetOption> {
    val result = mutableListOf<WidgetOption>()
    cache?.channels.orEmpty().forEach { channel ->
        when {
            channel.kind == ChannelKind.Quota &&
                (channel.platformId == "codex" || channel.platformId == "antigravity") &&
                channel.windows.any { it.key.isNotBlank() } -> {
                val sorted = channel.windows
                    .filter { it.key.isNotBlank() }
                    .sortedWith(compareBy(::windowOrder).thenBy { it.key })
                val weeklyKey = sorted.firstOrNull { it.main }?.key
                    ?: sorted.firstOrNull { it.key == "weekly" || it.key == "gemini-weekly" }?.key
                    ?: sorted.first().key
                sorted.forEach { window ->
                    val entry = WidgetEntry(
                        platformId = channel.platformId,
                        name = channel.name,
                        windowKey = window.key,
                        windowLabel = WidgetFormatter.configLabel(channel, window.key),
                    )
                    val value = WidgetFormatter.value(channel, window.key)
                    result += WidgetOption(
                        entry = entry,
                        channel = channel,
                        valueText = value,
                        available = value != "—",
                        preferred = window.key == weeklyKey,
                    )
                }
            }

            channel.kind == ChannelKind.Quota && channel.platformId == "opencode-go" -> {
                val window = channel.windows.firstOrNull { it.key == "monthly" }
                    ?: channel.windows.firstOrNull { it.main }
                    ?: channel.windows.firstOrNull()
                val key = window?.key?.takeIf { it.isNotBlank() }
                val entry = WidgetEntry(
                    platformId = channel.platformId,
                    name = channel.name,
                    windowKey = key,
                    windowLabel = window?.let { WidgetFormatter.configLabel(channel, key) } ?: "月额度",
                )
                val value = WidgetFormatter.value(channel, key)
                result += WidgetOption(entry, channel, value, value != "—", true)
            }

            channel.kind == ChannelKind.Quota -> {
                val window = channel.windows.firstOrNull { it.main }
                    ?: channel.windows.firstOrNull()
                val entry = WidgetEntry(
                    platformId = channel.platformId,
                    name = channel.name,
                    windowKey = null,
                    windowLabel = window?.let { WidgetFormatter.configLabel(channel, null) } ?: "主额度",
                )
                val value = WidgetFormatter.value(channel, null)
                result += WidgetOption(entry, channel, value, value != "—", true)
            }

            else -> {
                val entry = WidgetEntry(channel.platformId, channel.name)
                val value = WidgetFormatter.value(channel)
                result += WidgetOption(entry, channel, value, value != "—", true)
            }
        }
    }
    // 旧选择里存在、当前快照里没有的条目：保留为不可用，让用户能移除。
    saved.forEach { entry ->
        val samePlatform = result.any { it.entry.platformId == entry.platformId }
        if (result.none { it.entry.entryKey == entry.entryKey } &&
            (entry.windowKey != null || !samePlatform)
        ) {
            result += WidgetOption(entry, null, "—", false, false)
        }
    }
    return result
}

private fun optionLabel(option: WidgetOption): String = when {
    !option.entry.windowLabel.isNullOrBlank() -> option.entry.windowLabel
    option.channel?.kind == ChannelKind.Money -> "金额"
    option.channel?.kind == ChannelKind.Points -> "积分"
    option.channel?.kind == ChannelKind.Quota -> "主额度"
    else -> "不可用"
}

private fun windowOrder(window: WindowSnapshot): Int = when (window.key) {
    "weekly", "gemini-weekly" -> 0
    "5h", "gemini-5h" -> 1
    "3p-weekly" -> 2
    "3p-5h" -> 3
    "gemini-argon-wtu-weekly" -> 4
    "monthly" -> 5
    "daily" -> 6
    else -> 9
}
