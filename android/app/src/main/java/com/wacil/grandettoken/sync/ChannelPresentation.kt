package com.wacil.grandettoken.sync

import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.Canvas as DrawCanvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.wacil.grandettoken.sync.sync.ChannelKind
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import com.wacil.grandettoken.sync.sync.Decimal
import com.wacil.grandettoken.sync.sync.ExpiringSnapshot
import com.wacil.grandettoken.sync.sync.HistoryPoint
import java.text.SimpleDateFormat
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.ZoneOffset
import java.util.Date
import java.util.Locale

@Composable
internal fun ChannelCard(
    ch: ChannelSnapshot,
    expanded: Boolean,
    customSort: Boolean,
    canMoveUp: Boolean,
    canMoveDown: Boolean,
    onToggle: () -> Unit,
    onDetail: () -> Unit,
    onMove: (Int) -> Unit,
) {
    val tone = when {
        ch.stale || ch.errorCode != null -> Warn
        ch.kind == ChannelKind.Quota && (Decimal.parseMicros(ch.percent) ?: 100_000_000L) <= 20_000_000L -> Warn
        else -> Ok
    }
    Card(colors = CardDefaults.cardColors(containerColor = Surface1), shape = RoundedCornerShape(12.dp), modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.animateContentSize()) {
            Column(
                Modifier.fillMaxWidth()
                    .semantics { stateDescription = if (expanded) "已展开" else "已收起" }
                    .clickable(role = Role.Button, onClickLabel = if (expanded) "收起渠道摘要" else "展开渠道摘要", onClick = onToggle)
                    .padding(horizontal = 12.dp, vertical = 12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    ChannelLogo(ch)
                    Spacer(Modifier.width(10.dp))
                    Column(Modifier.weight(1f)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(ch.name, color = Tx, fontSize = 13.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
                            Text(channelValue(ch), color = Tx, fontSize = 18.sp, fontWeight = FontWeight.SemiBold, maxLines = 1)
                            Text(if (expanded) "  ▴" else "  ▾", color = Tx3, fontSize = 14.sp)
                        }
                        Spacer(Modifier.height(3.dp))
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Box(Modifier.size(6.dp).background(tone, RoundedCornerShape(99.dp)))
                            Spacer(Modifier.width(5.dp))
                            Text(
                                buildString {
                                    append(kindLabel(ch.kind))
                                    append(" · ")
                                    append(if (ch.errorCode != null) "取数失败" else if (ch.stale) "数据较旧" else relativeTime(ch.updatedAt))
                                    if (ch.kind == ChannelKind.Points) {
                                        nearestExpiry(ch)?.let { append(" · 最近到期 ${shortDate(it.at)}") }
                                    }
                                    if (ch.estimated) append(" · 推算")
                                },
                                color = Tx3, fontSize = 11.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.weight(1f),
                            )
                            if (ch.day != null) Text("今日 ${Decimal.display(ch.day, ch.unit, ch.kind == ChannelKind.Points)}", color = Tx3, fontSize = 10.sp)
                        }
                    }
                }
                channelFraction(ch)?.let { fraction ->
                    Spacer(Modifier.height(9.dp))
                    ChannelProgress(fraction, tone)
                }
            }
            if (expanded) {
                HorizontalDivider(color = Line, modifier = Modifier.padding(horizontal = 12.dp))
                Column(Modifier.padding(start = 12.dp, end = 12.dp, top = 10.dp, bottom = 4.dp)) {
                    if (ch.kind == ChannelKind.Quota && ch.windows.isNotEmpty()) {
                        ch.windows.forEach { window ->
                            DetailRow(window.label, listOfNotNull(Decimal.display(window.remainPercent, "%"), window.resetText).joinToString(" · "))
                            Spacer(Modifier.height(6.dp))
                        }
                    } else {
                        if (ch.kind == ChannelKind.Points) {
                            nearestExpiry(ch)?.let { expiry ->
                                DetailRow("最近到期", "${shortDate(expiry.at)} · ${Decimal.display(expiry.amount, ch.unit, points = true)}")
                                Spacer(Modifier.height(6.dp))
                            }
                        }
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            MiniMetric("今日", Decimal.display(ch.day, ch.unit, ch.kind == ChannelKind.Points), Modifier.weight(1f))
                            MiniMetric("本周", Decimal.display(ch.week, ch.unit, ch.kind == ChannelKind.Points), Modifier.weight(1f))
                            MiniMetric("本月", Decimal.display(ch.month, ch.unit, ch.kind == ChannelKind.Points), Modifier.weight(1f))
                        }
                    }
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        TextButton(onClick = onDetail) { Text("查看详情 →", fontSize = 12.sp) }
                        Spacer(Modifier.weight(1f))
                        if (customSort) {
                            TextButton(onClick = { onMove(-1) }, enabled = canMoveUp, contentPadding = PaddingValues(horizontal = 6.dp)) { Text("↑ 上移", fontSize = 11.sp) }
                            TextButton(onClick = { onMove(1) }, enabled = canMoveDown, contentPadding = PaddingValues(horizontal = 6.dp)) { Text("↓ 下移", fontSize = 11.sp) }
                        }
                    }
                }
            }
        }
    }
}

private fun nearestExpiry(ch: ChannelSnapshot): ExpiringSnapshot? =
    ch.expiring.minByOrNull { it.at }

private fun shortDate(ts: Long): String =
    SimpleDateFormat("MM-dd", Locale.getDefault()).format(Date(ts * 1000))

@Composable
private fun ChannelProgress(fraction: Float, color: Color) {    Box(Modifier.fillMaxWidth().height(4.dp).background(Surface3, RoundedCornerShape(99.dp))) {
        if (fraction > 0f) Box(Modifier.fillMaxWidth(fraction.coerceIn(0f, 1f)).height(4.dp).background(color, RoundedCornerShape(99.dp)))
    }
}

@Composable
private fun ChannelLogo(ch: ChannelSnapshot) {
    val logo = when (ch.platformId) {
        "deepseek" -> R.drawable.channel_deepseek
        "codex" -> R.drawable.channel_codex
        "trae" -> R.drawable.channel_trae
        "opencode-go" -> R.drawable.channel_opencode
        "workbuddy", "workbuddy-ai" -> R.drawable.channel_workbuddy
        "qoder", "qoder-cn" -> R.drawable.channel_qoder
        else -> null
    }
    Box(Modifier.size(34.dp).background(Surface3, RoundedCornerShape(9.dp)), contentAlignment = Alignment.Center) {
        if (logo != null) Image(painterResource(logo), contentDescription = null, modifier = Modifier.size(28.dp))
        else Text(ch.name.take(2), color = Accent, fontSize = 10.sp, fontWeight = FontWeight.Bold)
    }
}

@Composable
private fun MiniMetric(label: String, value: String, modifier: Modifier = Modifier) {
    Column(modifier) {
        Text(label, color = Tx3, fontSize = 10.sp)
        Text(value, color = Tx2, fontSize = 12.sp, fontWeight = FontWeight.Medium, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ChannelDetailScreen(
    ch: ChannelSnapshot,
    modifier: Modifier = Modifier,
    loggedIn: Boolean = false,
    backgroundMinutes: Int = 15,
    onBack: () -> Unit,
) {
    var period by rememberSaveable(ch.platformId) { mutableIntStateOf(0) }
    val periods = listOf("今日" to ch.day, "本周" to ch.week, "本月" to ch.month)
    Scaffold(
        modifier = modifier,
        containerColor = Canvas,
        topBar = {
            TopAppBar(
                title = { Column { Text(ch.name, fontSize = 16.sp, maxLines = 1); Text("${kindLabel(ch.kind)} · 同步组", color = Tx3, fontSize = 10.sp) } },
                navigationIcon = { TextButton(onClick = onBack) { Text("‹ 返回", fontSize = 13.sp) } },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = Surface1, titleContentColor = Tx),
            )
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier.padding(padding).fillMaxSize(),
            contentPadding = PaddingValues(12.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            if (ch.stale || ch.errorCode != null) {
                item { Banner("数据较旧", "保留最后一次成功的快照,以更新时间为准。", Warn) }
            }
            item {
                Card(colors = CardDefaults.cardColors(containerColor = Surface1), shape = RoundedCornerShape(12.dp), modifier = Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp)) {
                        Text(when (ch.kind) { ChannelKind.Money -> "当前余额"; ChannelKind.Quota -> "剩余额度"; ChannelKind.Points -> "当前积分" }, color = Tx3, fontSize = 12.sp)
                        Spacer(Modifier.height(5.dp))
                        Text(channelValue(ch), color = Tx, fontSize = 30.sp, fontWeight = FontWeight.Bold)
                        Spacer(Modifier.height(5.dp))
                        Text("更新于 ${relativeTime(ch.updatedAt)}${if (ch.estimated) " · 含推算数据" else ""}", color = Tx3, fontSize = 11.sp)
                        channelFraction(ch)?.let { Spacer(Modifier.height(12.dp)); ChannelProgress(it, Accent) }
                    }
                }
            }
            if (ch.kind != ChannelKind.Quota) {
                item {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        periods.forEachIndexed { index, entry -> FilterChip(selected = period == index, onClick = { period = index }, label = { Text(entry.first) }) }
                    }
                }
                item {
                    Card(colors = CardDefaults.cardColors(containerColor = Surface1), shape = RoundedCornerShape(12.dp), modifier = Modifier.fillMaxWidth()) {
                        Column(Modifier.padding(14.dp)) {
                            Text("${periods[period].first}${if (ch.kind == ChannelKind.Points) "数据" else "消耗"}", color = Tx3, fontSize = 11.sp)
                            Text(Decimal.display(periods[period].second, ch.unit, ch.kind == ChannelKind.Points), color = Tx, fontSize = 21.sp, fontWeight = FontWeight.SemiBold)
                            if (ch.estimated) Text("该值由快照推算,未记录时段可能有缺口。", color = Tx3, fontSize = 10.sp)
                        }
                    }
                }
            }
            if (ch.windows.isNotEmpty()) {
                item {
                    DetailCard("额度窗口") {
                        ch.windows.forEach { window ->
                            DetailRow(window.label, listOfNotNull(Decimal.display(window.remainPercent, "%"), window.resetText).joinToString(" · "))
                            Spacer(Modifier.height(8.dp))
                        }
                    }
                }
            }
            if (ch.expiring.isNotEmpty()) {
                item {
                    DetailCard("到期批次") {
                        ch.expiring.forEach { expiry ->
                            DetailRow(formatDate(expiry.at), Decimal.display(expiry.amount, ch.unit, ch.kind == ChannelKind.Points))
                            Spacer(Modifier.height(8.dp))
                        }
                    }
                }
            }
            item { HistoryCard(ch.history, ch.unit) }
            item {
                DetailCard("数据说明") {
                    DetailRow("数据来源", "同步组")
                    Spacer(Modifier.height(8.dp))
                    DetailRow("合并方式", "同一渠道取最新")
                    Spacer(Modifier.height(8.dp))
                    DetailRow("最后成功更新", formatDate(ch.updatedAt))
                    Spacer(Modifier.height(8.dp))
                    DetailRow("前台刷新", if (loggedIn) "60 秒" else "5 分钟")
                    Spacer(Modifier.height(8.dp))
                    DetailRow("后台刷新", "$backgroundMinutes 分钟")
                    Spacer(Modifier.height(8.dp))
                    DetailRow("数据状态", if (ch.stale || ch.errorCode != null) "可能已过期" else "正常")
                    Spacer(Modifier.height(8.dp))
                    DetailRow("是否推算", if (ch.estimated) "是" else "否")
                    if (ch.errorCode != null) { Spacer(Modifier.height(8.dp)); DetailRow("错误码", ch.errorCode) }
                    Spacer(Modifier.height(8.dp))
                    Text("手机展示只读快照,不会修改渠道配置。", color = Tx3, fontSize = 10.sp)
                }
            }
        }
    }
}

@Composable
private fun DetailCard(title: String, content: @Composable () -> Unit) {
    Card(colors = CardDefaults.cardColors(containerColor = Surface1), shape = RoundedCornerShape(12.dp), modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(14.dp)) {
            Text(title, color = Tx, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(12.dp))
            content()
        }
    }
}

@Composable
private fun HistoryCard(history: List<HistoryPoint>, unit: String) {
    val usable = history.filter { it.unit == unit }
    val useBalance = usable.any { Decimal.parseMicros(it.balance) != null }
    val zone: ZoneId = usable.maxByOrNull { it.periodStart }?.let { runCatching { ZoneOffset.of(it.timeZone) }.getOrNull() } ?: ZoneId.systemDefault()
    val days = (6 downTo 0).map { LocalDate.now(zone).minusDays(it.toLong()) }
    val byDay = usable.mapNotNull { point ->
        val pointZone = runCatching { ZoneOffset.of(point.timeZone) }.getOrElse { zone }
        runCatching { Instant.ofEpochSecond(point.periodStart).atZone(pointZone).toLocalDate() to point }.getOrNull()
    }.groupBy({ it.first }, { it.second }).mapValues { (_, points) -> points.maxByOrNull { it.revisedAt } }
    val values = days.map { day -> byDay[day]?.let { Decimal.parseMicros(if (useBalance) it.balance else it.spent)?.toDouble() } }
    val existing = values.filterNotNull()
    DetailCard(if (useBalance) "近 7 天余额" else "近 7 天消耗") {
        if (existing.isEmpty()) {
            Text("暂无可展示的历史记录", color = Tx3, fontSize = 12.sp)
        } else {
            val min = existing.minOrNull() ?: 0.0
            val max = existing.maxOrNull() ?: 0.0
            val span = (max - min).takeIf { it > 0.0 } ?: 1.0
            val missing = values.count { it == null }
            val incomplete = usable.count { !it.complete }
            Text(
                buildString {
                    if (missing > 0) append("$missing 天无数据,曲线在缺口处断开")
                    else append("按每日记录展示")
                    if (incomplete > 0) append(";$incomplete 天采样不完整")
                },
                color = Tx3,
                fontSize = 10.sp,
            )
            Spacer(Modifier.height(8.dp))
            DrawCanvas(Modifier.fillMaxWidth().height(96.dp).semantics { contentDescription = "近七天趋势,$missing 天没有数据" }) {
                val pad = 6.dp.toPx()
                val width = size.width - pad * 2
                val height = size.height - pad * 2
                drawLine(Line, Offset(pad, size.height - pad), Offset(size.width - pad, size.height - pad), strokeWidth = 1.dp.toPx())
                fun position(index: Int, value: Double): Offset = Offset(pad + width * index / 6f, (pad + height * (1.0 - (value - min) / span)).toFloat())
                values.forEachIndexed { index, value ->
                    if (value != null) {
                        val here = position(index, value)
                        if (index > 0 && values[index - 1] != null) drawLine(Accent, position(index - 1, values[index - 1]!!), here, strokeWidth = 2.dp.toPx(), cap = StrokeCap.Round)
                        drawCircle(Accent, radius = 3.dp.toPx(), center = here)
                    }
                }
            }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                days.forEach { Text("${it.monthValue}/${it.dayOfMonth}", color = Tx3, fontSize = 9.sp) }
            }
            Spacer(Modifier.height(10.dp))
            usable.sortedByDescending { it.periodStart }.take(3).forEach { point ->
                DetailRow(formatDate(point.periodStart), Decimal.display(if (useBalance) point.balance else point.spent, unit))
                Spacer(Modifier.height(6.dp))
            }
        }
    }
}
