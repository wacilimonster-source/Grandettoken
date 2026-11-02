package com.wacil.grandettoken.sync

import android.content.Context
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
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
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.pm.PackageInfoCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.wacil.grandettoken.sync.sync.ChannelKind
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import com.wacil.grandettoken.sync.sync.Decimal
import com.wacil.grandettoken.sync.sync.MergedSnapshot
import com.wacil.grandettoken.sync.update.AndroidUpdateRepository
import com.wacil.grandettoken.sync.update.UpdateArtifact
import com.wacil.grandettoken.sync.update.UpdateCheckException
import com.wacil.grandettoken.sync.update.UpdateCheckResult
import com.wacil.grandettoken.sync.update.UpdatePayload
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

private val Canvas = Color(0xFF0E1015)
private val Surface1 = Color(0xFF141821)
private val Surface2 = Color(0xFF191E27)
private val Surface3 = Color(0xFF212836)
private val Line = Color(0xFF252B38)
private val Tx = Color(0xFFF3F6FB)
private val Tx2 = Color(0xFFB7C0D0)
private val Tx3 = Color(0xFF8A95A8)
private val Ok = Color(0xFF4FD6A0)
private val Warn = Color(0xFFF3C666)
private val Bad = Color(0xFFFF7B72)
private val Accent = Color(0xFF8AA4FF)
private val WarningText = Color(0xFFF6D58F)

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            GrandettokenTheme {
                SyncApp()
            }
        }
    }
}

@Composable
private fun GrandettokenTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = darkColorScheme(
            primary = Accent,
            onPrimary = Color(0xFF0B1020),
            secondary = Ok,
            onSecondary = Color(0xFF05130D),
            background = Canvas,
            onBackground = Tx,
            surface = Surface1,
            onSurface = Tx,
            surfaceVariant = Surface2,
            onSurfaceVariant = Tx2,
            outline = Line,
            error = Bad,
            onError = Color(0xFF2A0B09),
        ),
        content = content,
    )
}

@Composable
private fun SyncApp(viewModel: SyncViewModel = viewModel()) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val lifecycleOwner = LocalLifecycleOwner.current
    val context = LocalContext.current.applicationContext
    val updateAvailable = remember { mutableStateOf(false) }
    val updateScope = rememberCoroutineScope()
    val updatePrefs = remember(context) {
        context.getSharedPreferences("grandettoken_update", Context.MODE_PRIVATE)
    }
    DisposableEffect(lifecycleOwner, context) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_START -> {
                    viewModel.startPolling()
                    updateScope.launch {
                        if (!updatePrefs.getBoolean("autoCheck", true)) return@launch
                        val now = System.currentTimeMillis()
                        val lastAttempt = updatePrefs.getLong("lastAutoAttempt", 0L)
                        if (now - lastAttempt < 24L * 60L * 60L * 1000L) return@launch
                        updatePrefs.edit().putLong("lastAutoAttempt", now).apply()
                        val result = runCatching {
                            withContext(Dispatchers.IO) { AndroidUpdateRepository.check(context) }
                        }.getOrNull()
                        if (result != null) updatePrefs.edit().putLong("lastSuccessfulCheck", now).apply()
                        if (result is UpdateCheckResult.Available) {
                            updateAvailable.value = updatePrefs.getLong("skippedVersionCode", -1L) != result.payload.versionCode
                        } else if (result == UpdateCheckResult.UpToDate) {
                            updateAvailable.value = false
                        }
                    }
                }
                Lifecycle.Event.ON_STOP -> viewModel.stopPolling()
                else -> Unit
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose {
            lifecycleOwner.lifecycle.removeObserver(observer)
            viewModel.stopPolling()
        }
    }

    if (state.group == null) {
        PairScreen(state, viewModel::join, updateAvailable.value) { updateAvailable.value = false }
    } else {
        MainScaffold(state, viewModel, updateAvailable.value) { updateAvailable.value = false }
    }
}

@Composable
private fun PairScreen(
    state: SyncUiState,
    onJoin: (String, () -> Unit) -> Unit,
    updateAvailable: Boolean,
    onUpdateSkipped: () -> Unit,
) {
    var code by rememberSaveable { mutableStateOf("") }
    val context = LocalContext.current
    val version = remember(context) { versionLabel(context) }
    var showAbout by remember { mutableStateOf(false) }
    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(Canvas)
            .padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Spacer(Modifier.height(18.dp))
        Text("加入同步组", fontSize = 24.sp, fontWeight = FontWeight.Bold, color = Tx)
        Text(
            "手机只读。复制电脑端「设置 → 同步到手机」里的同步码，粘贴到这里。",
            color = Tx2,
            fontSize = 13.sp,
            lineHeight = 20.sp,
        )
        OutlinedTextField(
            value = code,
            onValueChange = { code = it },
            label = { Text("同步码") },
            placeholder = { Text("GT1.…") },
            singleLine = false,
            maxLines = 4,
            modifier = Modifier.fillMaxWidth(),
        )
        Button(
            onClick = { onJoin(code) { code = "" } },
            enabled = !state.loading && code.isNotBlank(),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(if (state.loading) "正在校验…" else "加入")
        }
        state.error?.let {
            Text(it, color = Bad, fontSize = 12.sp, lineHeight = 18.sp)
        }
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .background(Surface1, RoundedCornerShape(12.dp))
                .padding(14.dp),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Text("匿名模式", color = Ok, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    "不登录 GitHub 也能用：前台 5 分钟刷新，后台 15 分钟起。二维码扫描待接入，请先复制同步码手动输入。",
                    color = Tx3,
                    fontSize = 12.sp,
                    lineHeight = 18.sp,
                )
            }
        }
        Text(
            "同步码包含解密密钥，请勿分享。手机不会上传任何渠道密钥。",
            color = Tx3,
            fontSize = 11.sp,
            lineHeight = 16.sp,
        )
        TextButton(
            onClick = { showAbout = true },
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(if (updateAvailable) "发现更新 · 关于与更新" else "关于与更新", color = if (updateAvailable) Accent else Tx3, fontSize = 11.sp)
        }
    }
    if (showAbout) UpdateDialog(version, { showAbout = false }, onUpdateSkipped)
}

@Composable
private fun MainScaffold(
    state: SyncUiState,
    viewModel: SyncViewModel,
    updateAvailable: Boolean,
    onUpdateSkipped: () -> Unit,
) {
    var tab by rememberSaveable { mutableIntStateOf(0) }
    Scaffold(
        containerColor = Canvas,
        bottomBar = {
            NavigationBar(containerColor = Surface1) {
                NavigationBarItem(
                    selected = tab == 0,
                    onClick = { tab = 0 },
                    icon = { Text("◎", color = if (tab == 0) Accent else Tx3) },
                    label = { Text("面板", fontSize = 11.sp) },
                )
                NavigationBarItem(
                    selected = tab == 1,
                    onClick = { tab = 1 },
                    icon = { Text("⚙", color = if (tab == 1) Accent else Tx3) },
                    label = { Text("设置", fontSize = 11.sp) },
                )
            }
        },
    ) { padding ->
        if (tab == 0) {
            DashboardScreen(state, viewModel, Modifier.padding(padding))
        } else {
            SettingsScreen(state, viewModel, Modifier.padding(padding), updateAvailable, onUpdateSkipped)
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DashboardScreen(
    state: SyncUiState,
    viewModel: SyncViewModel,
    modifier: Modifier = Modifier,
) {
    var selected by remember { mutableStateOf<ChannelSnapshot?>(null) }
    val merged = state.merged
    val channels = merged?.channels.orEmpty()
    val risky = channels.filter { it.stale || it.errorCode != null }
    val rest = channels.filter { !(it.stale || it.errorCode != null) }
    val staleCount = merged?.staleChannels ?: 0

    Scaffold(
        modifier = modifier,
        containerColor = Canvas,
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text("同步组", fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
                        Text(
                            when {
                                state.loading -> "正在刷新…"
                                state.lastRefresh > 0 -> "${relativeTime(state.lastRefresh)} · ${channels.size} 个渠道"
                                else -> "等待同步"
                            },
                            fontSize = 10.sp,
                            color = Tx3,
                        )
                    }
                },
                actions = {
                    TextButton(onClick = { viewModel.refresh(true) }) {
                        Text("刷新", color = Accent)
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = Surface1,
                    titleContentColor = Tx,
                ),
            )
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
            contentPadding = PaddingValues(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (state.loading) {
                item {
                    LinearProgressIndicator(
                        modifier = Modifier.fillMaxWidth(),
                        color = Accent,
                        trackColor = Surface2,
                    )
                }
            }
            state.error?.let {
                item { Banner("同步失败", it, Bad) }
            }
            if (staleCount > 0) {
                item {
                    Banner(
                        "$staleCount 个渠道数据较旧",
                        "数字仍保留，但请以最后一次成功取数为准。",
                        Warn,
                    )
                }
            }
            item { SummaryCard(merged) }
            if (risky.isNotEmpty()) {
                item { SectionTitle("需要处理 · ${risky.size}") }
                items(risky, key = { it.platformId }) { ch ->
                    ChannelCard(ch) { selected = ch }
                }
            }
            if (rest.isNotEmpty()) {
                item { SectionTitle("全部渠道 · ${rest.size}") }
                items(rest, key = { it.platformId }) { ch ->
                    ChannelCard(ch) { selected = ch }
                }
            }
            if (channels.isEmpty() && !state.loading) {
                item {
                    Box(
                        Modifier
                            .fillMaxWidth()
                            .background(Surface1, RoundedCornerShape(12.dp))
                            .padding(18.dp)
                    ) {
                        Text(
                            "同步组还没有可展示的渠道。等电脑端发布一次，或检查同步码是否来自另一个组。",
                            color = Tx2,
                            fontSize = 12.sp,
                            lineHeight = 18.sp,
                        )
                    }
                }
            }
            item { Spacer(Modifier.height(8.dp)) }
        }
    }

    selected?.let { ch ->
        ChannelDetailSheet(ch) { selected = null }
    }
}

@Composable
private fun SummaryCard(merged: MergedSnapshot?) {
    val totals = merged?.totals
    val ck = merged?.checkin
    Card(
        colors = CardDefaults.cardColors(containerColor = Surface1),
        shape = RoundedCornerShape(12.dp),
    ) {
        Row(Modifier.padding(14.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            SummaryCell("人民币余额", Decimal.display(totals?.cny, "CNY"), Modifier.weight(1.4f))
            SummaryCell("今日消耗", Decimal.display(totals?.today, "CNY"), Modifier.weight(1.1f))
            SummaryCell(
                "签到",
                if (ck != null) "${ck.done}/${ck.total}" else "—",
                Modifier.weight(0.7f),
            )
        }
    }
}

@Composable
private fun SummaryCell(label: String, value: String, modifier: Modifier = Modifier) {
    Column(modifier) {
        Text(label, color = Tx3, fontSize = 10.sp)
        Text(
            value,
            color = Tx,
            fontSize = 17.sp,
            fontWeight = FontWeight.SemiBold,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun SectionTitle(text: String) {
    Text(
        text,
        color = Tx3,
        fontSize = 10.sp,
        fontWeight = FontWeight.Bold,
        letterSpacing = 1.sp,
        modifier = Modifier.padding(top = 6.dp, start = 2.dp),
    )
}

@Composable
private fun Banner(title: String, body: String, tone: Color) {
    Box(
        Modifier
            .fillMaxWidth()
            .background(tone.copy(alpha = 0.09f), RoundedCornerShape(10.dp))
            .padding(11.dp)
    ) {
        Column {
            Text(title, color = tone, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
            Text(body, color = Tx2, fontSize = 11.sp, lineHeight = 16.sp)
        }
    }
}

@Composable
private fun ChannelCard(ch: ChannelSnapshot, onClick: () -> Unit) {
    val tone = when {
        ch.stale || ch.errorCode != null -> Warn
        ch.kind == ChannelKind.Quota &&
            (Decimal.parseMicros(ch.percent) ?: 100_000_000L) <= 20_000_000L -> Warn
        else -> Ok
    }
    Card(
        colors = CardDefaults.cardColors(containerColor = Surface1),
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
    ) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(
                    Modifier
                        .size(30.dp)
                        .background(Surface3, RoundedCornerShape(8.dp)),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        ch.name.take(2),
                        color = Accent,
                        fontSize = 10.sp,
                        fontWeight = FontWeight.Bold,
                    )
                }
                Spacer(Modifier.width(9.dp))
                Column(Modifier.weight(1f)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            ch.name,
                            color = Tx,
                            fontSize = 12.sp,
                            fontWeight = FontWeight.Medium,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f),
                        )
                        Text(
                            channelValue(ch),
                            color = Tx,
                            fontSize = 19.sp,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1,
                        )
                    }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Box(
                            Modifier
                                .size(6.dp)
                                .background(tone, RoundedCornerShape(99.dp))
                        )
                        Spacer(Modifier.width(5.dp))
                        Text(
                            buildString {
                                append(kindLabel(ch.kind))
                                append(" · ")
                                append(relativeTime(ch.updatedAt))
                                if (ch.stale) append(" · 陈旧")
                                if (ch.estimated) append(" · 推算")
                            },
                            color = Tx3,
                            fontSize = 10.sp,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f),
                        )
                        Text(
                            Decimal.display(ch.day, ch.unit),
                            color = Tx3,
                            fontSize = 10.sp,
                        )
                    }
                }
            }
            channelFraction(ch)?.let { fraction ->
                Spacer(Modifier.height(8.dp))
                LinearProgressIndicator(
                    progress = { fraction },
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(4.dp),
                    color = tone,
                    trackColor = Surface3,
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChannelDetailSheet(ch: ChannelSnapshot, onDismiss: () -> Unit) {
    ModalBottomSheet(
        onDismissRequest = onDismiss,
        containerColor = Surface1,
    ) {
        Column(
            Modifier
                .padding(horizontal = 18.dp)
                .padding(bottom = 26.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(ch.name, color = Tx, fontSize = 18.sp, fontWeight = FontWeight.Bold)
            Text(
                "${kindLabel(ch.kind)} · ${ch.unit} · ${relativeTime(ch.updatedAt)}",
                color = Tx3,
                fontSize = 11.sp,
            )
            Text(
                channelValue(ch),
                color = Tx,
                fontSize = 32.sp,
                fontWeight = FontWeight.Bold,
            )
            if (ch.stale) {
                Banner("数据较旧", "这是最后一次成功取数的快照；不是 0。", Warn)
            }
            DetailRow("今日", Decimal.display(ch.day, ch.unit))
            DetailRow("本周", Decimal.display(ch.week, ch.unit))
            DetailRow("本月", Decimal.display(ch.month, ch.unit))
            if (ch.total != null) DetailRow("总额", Decimal.display(ch.total, ch.unit))
            if (ch.estimated) DetailRow("口径", "本地快照推算")
            if (ch.windows.isNotEmpty()) {
                HorizontalDivider(color = Line)
                Text("限流窗口", color = Tx3, fontSize = 10.sp, fontWeight = FontWeight.Bold)
                ch.windows.forEach { w ->
                    DetailRow(
                        w.label,
                        "${Decimal.display(w.remainPercent, "%")}${w.resetText?.let { " · $it" } ?: ""}",
                    )
                }
            }
            if (ch.expiring.isNotEmpty()) {
                HorizontalDivider(color = Line)
                Text("到期批次", color = Tx3, fontSize = 10.sp, fontWeight = FontWeight.Bold)
                ch.expiring.forEach { e ->
                    DetailRow(formatDate(e.at), Decimal.display(e.amount, ch.unit, ch.kind == ChannelKind.Points))
                }
            }
            if (ch.history.isNotEmpty()) {
                HorizontalDivider(color = Line)
                Text("最近 7 天消耗", color = Tx3, fontSize = 10.sp, fontWeight = FontWeight.Bold)
                ch.history.forEach { h ->
                    DetailRow(formatDate(h.periodStart), Decimal.display(h.spent, h.unit))
                }
            }
            HorizontalDivider(color = Line)
            Text(
                "数据来源：同步组 · 手机只读，不显示是哪台电脑写入的。",
                color = Tx3,
                fontSize = 10.sp,
                lineHeight = 15.sp,
            )
        }
    }
}

@Composable
private fun DetailRow(label: String, value: String) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
        Text(label, color = Tx3, fontSize = 11.sp)
        Text(value, color = Tx2, fontSize = 11.sp, fontWeight = FontWeight.Medium)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SettingsScreen(
    state: SyncUiState,
    viewModel: SyncViewModel,
    modifier: Modifier = Modifier,
    updateAvailable: Boolean,
    onUpdateSkipped: () -> Unit,
) {
    var confirmLogout by remember { mutableStateOf(false) }
    var loginInfo by remember { mutableStateOf(false) }
    var showAbout by remember { mutableStateOf(false) }
    val context = LocalContext.current
    val version = remember(context) { versionLabel(context) }
    val prefs = remember(context) {
        context.getSharedPreferences("grandettoken_update", Context.MODE_PRIVATE)
    }
    var autoCheck by remember { mutableStateOf(prefs.getBoolean("autoCheck", true)) }
    val info = state.group
    Scaffold(
        modifier = modifier,
        containerColor = Canvas,
        topBar = {
            TopAppBar(
                title = { Text("设置", fontSize = 16.sp, fontWeight = FontWeight.SemiBold) },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = Surface1,
                    titleContentColor = Tx,
                ),
            )
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
            contentPadding = PaddingValues(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            item { SectionTitle("同步组") }
            item {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .background(Surface1, RoundedCornerShape(12.dp))
                        .padding(14.dp)
                ) {
                    Column(verticalArrangement = Arrangement.spacedBy(7.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text("已加入同步组", color = Tx, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                            Spacer(Modifier.weight(1f))
                            Text("端到端加密", color = Ok, fontSize = 10.sp)
                        }
                        DetailRow("中继", info?.relayId?.take(10)?.plus("…") ?: "—")
                        DetailRow("密钥代数", info?.keyEpoch?.toString() ?: "—")
                        DetailRow("上次刷新", if (state.lastRefresh > 0) relativeTime(state.lastRefresh) else "—")
                        DetailRow("数据源", "${state.merged?.sources ?: 0} 个")
                        DetailRow("刷新模式", if (state.loggedIn) "GitHub 登录 · 60 秒" else "匿名 · 5 分钟")
                    }
                }
            }
            item {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        onClick = { loginInfo = true },
                        modifier = Modifier.weight(1f),
                    ) {
                        Text(if (state.loggedIn) "已登录 GitHub" else "登录 GitHub 提速")
                    }
                    if (state.loggedIn) {
                        TextButton(onClick = { viewModel.disconnectGitHub() }) {
                            Text("断开", color = Bad)
                        }
                    }
                }
            }
            item { SectionTitle("后台刷新") }
            item {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf(15, 30, 60).forEach { m ->
                        FilterChip(
                            selected = state.backgroundMinutes == m,
                            onClick = { viewModel.setBackgroundMinutes(m) },
                            label = { Text("$m 分钟") },
                        )
                    }
                }
            }
            item {
                Text(
                    "Android 的定时任务最短 15 分钟；前台打开 App 时按匿名 5 分钟 / 登录 60 秒刷新。",
                    color = Tx3,
                    fontSize = 11.sp,
                    lineHeight = 16.sp,
                )
            }
            if (!state.error.isNullOrBlank() || (state.merged?.failedSources?.isNotEmpty() == true)) {
                item { SectionTitle("诊断") }
                item {
                    Box(
                        Modifier
                            .fillMaxWidth()
                            .background(Surface1, RoundedCornerShape(12.dp))
                            .padding(14.dp)
                    ) {
                        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                            state.error?.let {
                                Text(it, color = Bad, fontSize = 11.sp, lineHeight = 16.sp)
                            }
                            state.merged?.failedSources?.take(6)?.forEach {
                                Text(it, color = WarningText, fontSize = 10.sp)
                            }
                            Text(
                                "诊断不含同步密钥、GitHub token、余额或账号信息。",
                                color = Tx3,
                                fontSize = 10.sp,
                            )
                        }
                    }
                }
            }
            item { SectionTitle("关于与更新") }
            item {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .background(Surface1, RoundedCornerShape(12.dp))
                        .padding(14.dp)
                ) {
                    Column(verticalArrangement = Arrangement.spacedBy(7.dp)) {
                        DetailRow("当前版本", version)
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text("自动检查更新", color = Tx2, fontSize = 12.sp)
                            Spacer(Modifier.weight(1f))
                            Switch(
                                checked = autoCheck,
                                onCheckedChange = {
                                    autoCheck = it
                                    prefs.edit().putBoolean("autoCheck", it).apply()
                                },
                            )
                        }
                        DetailRow("最近检查", formatUpdateCheck(prefs.getLong("lastSuccessfulCheck", 0L)))
                        if (updateAvailable) {
                            Text("发现新版本，点击检查更新查看。", color = Accent, fontSize = 11.sp)
                        }
                        TextButton(
                            onClick = { showAbout = true },
                            contentPadding = PaddingValues(0.dp),
                        ) {
                            Text(if (updateAvailable) "检查更新 · 有新版本" else "检查更新", color = Accent, fontSize = 12.sp)
                        }
                        Text("更新会先校验官方签名清单、APK 哈希和签名，再交给 Android 系统确认安装。", color = Tx3, fontSize = 10.sp, lineHeight = 15.sp)
                    }
                }
            }
            item { SectionTitle("数据与关于") }
            item {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    TextButton(onClick = { confirmLogout = true }) {
                        Text("退出同步组", color = Bad)
                    }
                    Text("Grandettoken Sync · 开源 · 手机只读", color = Tx3, fontSize = 10.sp)
                }
            }
            item { Spacer(Modifier.height(14.dp)) }
        }
    }

    if (confirmLogout) {
        AlertDialog(
            onDismissRequest = { confirmLogout = false },
            title = { Text("退出同步组？") },
            text = { Text("会删除本机主密钥、缓存和后台任务；GitHub 上的密文不会自动删除。需要重新扫码才能再次查看。") },
            confirmButton = {
                TextButton(onClick = {
                    confirmLogout = false
                    viewModel.logout()
                }) { Text("退出", color = Bad) }
            },
            dismissButton = {
                TextButton(onClick = { confirmLogout = false }) { Text("取消") }
            },
        )
    }
    if (loginInfo) {
        AlertDialog(
            onDismissRequest = { loginInfo = false },
            title = { Text("GitHub 登录提速") },
            text = {
                Text(
                    "手机专用 Device Flow 尚未接入。当前匿名模式前台 5 分钟刷新；接入后登录可提速到 60 秒，且不请求 gist/repo 写权限。",
                )
            },
            confirmButton = {
                TextButton(onClick = { loginInfo = false }) { Text("知道了") }
            },
        )
    }
    if (showAbout) UpdateDialog(version, { showAbout = false }, onUpdateSkipped)
}

@Composable
private fun UpdateDialog(
    version: String,
    onDismiss: () -> Unit,
    onSkipped: () -> Unit,
) {
    val context = LocalContext.current
    val prefs = remember(context) { context.getSharedPreferences("grandettoken_update", Context.MODE_PRIVATE) }
    val scope = rememberCoroutineScope()
    var status by remember { mutableStateOf("正在检查更新…") }
    var busy by remember { mutableStateOf(true) }
    var update by remember { mutableStateOf<Pair<UpdatePayload, UpdateArtifact>?>(null) }
    var downloadedApk by remember { mutableStateOf<File?>(null) }
    var failedCode by remember { mutableStateOf<String?>(null) }

    suspend fun checkLatest() {
        busy = true
        failedCode = null
        status = "正在检查更新…"
        val now = System.currentTimeMillis()
        prefs.edit().putLong("lastAttempt", now).apply()
        try {
            when (val result = withContext(Dispatchers.IO) { AndroidUpdateRepository.check(context) }) {
                UpdateCheckResult.UpToDate -> {
                    prefs.edit().putLong("lastSuccessfulCheck", now).apply()
                    update = null
                    downloadedApk = null
                    status = "已是最新版本"
                }
                is UpdateCheckResult.Available -> {
                    prefs.edit().putLong("lastSuccessfulCheck", now).apply()
                    update = result.payload to result.artifact
                    downloadedApk = null
                    status = "发现新版本 ${result.payload.versionName}"
                }
            }
        } catch (e: Exception) {
            failedCode = (e as? UpdateCheckException)?.code ?: "UPDATE_CHECK_FAILED"
            status = "检查失败：$failedCode"
        } finally {
            busy = false
        }
    }

    LaunchedEffect(Unit) { checkLatest() }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("关于与更新") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("当前版本 $version", color = Tx2, fontSize = 12.sp)
                Text(status, color = if (failedCode == null) Tx else Bad, fontSize = 12.sp)
                update?.let { (payload, artifact) ->
                    Text("发布时间：${payload.publishedAt}", color = Tx3, fontSize = 10.sp)
                    Text("APK 大小：${formatBytes(artifact.sizeBytes)}", color = Tx3, fontSize = 10.sp)
                    if (payload.notes.isNotBlank()) {
                        Text(payload.notes, color = Tx2, fontSize = 11.sp, lineHeight = 16.sp)
                    }
                }
                if (downloadedApk != null) {
                    Text("下载与安全校验完成。点击安装后由 Android 系统确认。", color = Ok, fontSize = 11.sp)
                }
                if (failedCode == "UPDATE_CERT_NOT_CONFIGURED") {
                    Text("当前安装包没有配置可信签名证书，请安装官方发布版后再检查更新。", color = Tx3, fontSize = 10.sp)
                }
            }
        },
        confirmButton = {
            when {
                busy -> TextButton(onClick = {}, enabled = false) { Text("请稍候…") }
                update != null && downloadedApk == null -> TextButton(onClick = {
                    scope.launch {
                        val target = update ?: return@launch
                        busy = true
                        failedCode = null
                        status = "正在下载并验证 APK…"
                        try {
                            downloadedApk = withContext(Dispatchers.IO) {
                                AndroidUpdateRepository.downloadAndVerify(context, target.first, target.second)
                            }
                            status = "下载和验证完成"
                        } catch (e: Exception) {
                            failedCode = (e as? UpdateCheckException)?.code ?: "UPDATE_DOWNLOAD_FAILED"
                            status = "下载或校验失败：$failedCode"
                        } finally {
                            busy = false
                        }
                    }
                }) { Text("下载更新") }
                downloadedApk != null -> TextButton(onClick = {
                    try {
                        if (!AndroidUpdateRepository.canInstallPackages(context)) {
                            status = "请允许 Grandettoken 安装此更新，再返回点击安装。"
                            AndroidUpdateRepository.requestInstallPermission(context)
                        } else {
                            AndroidUpdateRepository.installApk(context, downloadedApk!!)
                            status = "已交给 Android 安装器，请在系统界面确认。"
                        }
                    } catch (_: Exception) {
                        status = "无法打开系统安装器，请稍后重试。"
                    }
                }) { Text("安装更新") }
                else -> TextButton(onClick = { scope.launch { checkLatest() } }) { Text("重新检查") }
            }
        },
        dismissButton = {
            Row {
                if (update != null) {
                    TextButton(onClick = {
                        prefs.edit().putLong("skippedVersionCode", update!!.first.versionCode).apply()
                        onSkipped()
                        onDismiss()
                    }) { Text("跳过此版本") }
                }
                TextButton(onClick = onDismiss) { Text("稍后") }
            }
        },
    )
}

private fun versionLabel(context: Context): String {
    return try {
        val info = context.packageManager.getPackageInfo(context.packageName, 0)
        "${info.versionName ?: "—"} (${PackageInfoCompat.getLongVersionCode(info)})"
    } catch (_: Exception) {
        "—"
    }
}

private fun formatUpdateCheck(epochMillis: Long): String = if (epochMillis <= 0L) {
    "尚未成功检查"
} else {
    SimpleDateFormat("MM-dd HH:mm", Locale.getDefault()).format(Date(epochMillis))
}

private fun formatBytes(sizeBytes: Long): String = when {
    sizeBytes >= 1024L * 1024L -> String.format(Locale.getDefault(), "%.1f MB", sizeBytes / (1024.0 * 1024.0))
    sizeBytes >= 1024L -> String.format(Locale.getDefault(), "%.0f KB", sizeBytes / 1024.0)
    else -> "$sizeBytes B"
}

private fun channelValue(ch: ChannelSnapshot): String = when (ch.kind) {
    ChannelKind.Quota -> Decimal.display(ch.percent, "%")
    ChannelKind.Points -> Decimal.display(ch.remaining, ch.unit, points = true)
    ChannelKind.Money -> Decimal.display(ch.remaining, ch.unit)
}

private fun channelFraction(ch: ChannelSnapshot): Float? {
    if (ch.kind == ChannelKind.Quota) {
        val p = Decimal.parseMicros(ch.percent) ?: return null
        return (p.toDouble() / 1_000_000.0 / 100.0).coerceIn(0.0, 1.0).toFloat()
    }
    if (ch.kind == ChannelKind.Money) {
        val remaining = Decimal.parseMicros(ch.remaining) ?: return null
        val total = Decimal.parseMicros(ch.total) ?: return null
        if (total <= 0) return null
        return (remaining.toDouble() / total.toDouble()).coerceIn(0.0, 1.0).toFloat()
    }
    return null
}

private fun kindLabel(kind: ChannelKind): String = when (kind) {
    ChannelKind.Money -> "金额型"
    ChannelKind.Quota -> "配额型"
    ChannelKind.Points -> "积分型"
}

private fun relativeTime(ts: Long): String {
    if (ts <= 0) return "—"
    val delta = (System.currentTimeMillis() / 1000 - ts).coerceAtLeast(0)
    return when {
        delta < 60 -> "刚刚"
        delta < 3600 -> "${delta / 60} 分钟前"
        delta < 86_400 -> "${delta / 3600} 小时前"
        else -> "${delta / 86_400} 天前"
    }
}

private fun formatDate(ts: Long): String =
    if (ts <= 0) "—" else SimpleDateFormat("MM-dd HH:mm", Locale.getDefault()).format(Date(ts * 1000))
