package com.wacil.grandettoken.sync

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.Build
import android.os.Bundle
import androidx.activity.compose.BackHandler
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
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
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedButton
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
import androidx.compose.runtime.collectAsState
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
import androidx.core.content.ContextCompat
import androidx.core.content.pm.PackageInfoCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import com.wacil.grandettoken.sync.sync.ChannelKind
import com.wacil.grandettoken.sync.sync.ChannelOrdering
import com.wacil.grandettoken.sync.sync.ChannelSort
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import com.wacil.grandettoken.sync.sync.Decimal
import com.wacil.grandettoken.sync.sync.MergedSnapshot
import com.wacil.grandettoken.sync.update.AndroidUpdateRepository
import com.wacil.grandettoken.sync.update.UpdateArtifact
import com.wacil.grandettoken.sync.update.UpdateCheckException
import com.wacil.grandettoken.sync.update.UpdateCheckResult
import com.wacil.grandettoken.sync.update.UpdatePayload
import com.wacil.grandettoken.sync.widget.WidgetConfigActivity
import com.wacil.grandettoken.sync.widget.WidgetStore
import com.wacil.grandettoken.sync.widget.WidgetUpdater
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import org.json.JSONArray

internal val Canvas = Color(0xFF0E1015)
internal val Surface1 = Color(0xFF141821)
internal val Surface2 = Color(0xFF191E27)
internal val Surface3 = Color(0xFF212836)
internal val Line = Color(0xFF252B38)
internal val Tx = Color(0xFFF3F6FB)
internal val Tx2 = Color(0xFFB7C0D0)
internal val Tx3 = Color(0xFF8A95A8)
internal val Ok = Color(0xFF4FD6A0)
internal val Warn = Color(0xFFF3C666)
internal val Bad = Color(0xFFFF7B72)
internal val Accent = Color(0xFF8AA4FF)
private val WarningText = Color(0xFFF6D58F)

class MainActivity : ComponentActivity() {
    private var pendingChannelId by mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        pendingChannelId = intent?.getStringExtra(WidgetUpdater.EXTRA_CHANNEL_ID)
        // 上次把 APK 交给系统安装器后，只有真的装上新版才清理旧包与跳过记录。
        AndroidUpdateRepository.confirmInstalled(this)
        NotificationHelper.ensureChannel(this)
        setContent {
            GrandettokenTheme {
                SyncApp(
                    initialChannelId = pendingChannelId,
                    onChannelConsumed = { pendingChannelId = null },
                )
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        pendingChannelId = intent.getStringExtra(WidgetUpdater.EXTRA_CHANNEL_ID)
    }
}

@Composable
internal fun GrandettokenTheme(content: @Composable () -> Unit) {
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
private fun SyncApp(
    viewModel: SyncViewModel = viewModel(),
    initialChannelId: String? = null,
    onChannelConsumed: () -> Unit = {},
) {
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
                    // 回前台立刻刷一次,不要等轮询首轮(60s/300s)才更新;
                    // 非 force,刚刷过时只是一次 304。
                    viewModel.refresh()
                    updateScope.launch {
                        if (!updatePrefs.getBoolean("autoCheck", true)) return@launch
                        val now = System.currentTimeMillis()
                        val lastAttempt = updatePrefs.getLong("lastAutoAttempt", 0L)
                        // 成功检查后 24 小时不再查;上次失败(尝试晚于成功)按 1 小时退避重试,
                        // 不要一次断网就把自动检查挡住一整天。
                        val gap = if (updatePrefs.getLong("lastSuccessfulCheck", 0L) >= lastAttempt) {
                            24L * 60L * 60L * 1000L
                        } else {
                            60L * 60L * 1000L
                        }
                        if (now - lastAttempt < gap) return@launch
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
        MainScaffold(
            state,
            viewModel,
            updateAvailable.value,
            initialChannelId,
            onChannelConsumed,
        ) { updateAvailable.value = false }
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
    var scanHint by rememberSaveable { mutableStateOf("") }
    val context = LocalContext.current
    val version = remember(context) { versionLabel(context) }
    var showAbout by remember { mutableStateOf(false) }
    val hasCamera = remember(context) {
        context.packageManager.hasSystemFeature(PackageManager.FEATURE_CAMERA_ANY)
    }
    val scanOptions = remember {
        ScanOptions().apply {
            setDesiredBarcodeFormats(ScanOptions.QR_CODE)
            setPrompt("对准电脑端「当前同步码」下的二维码")
            setBeepEnabled(false)
            setOrientationLocked(false)
            setBarcodeImageEnabled(false)
        }
    }
    val scanLauncher = rememberLauncherForActivityResult(ScanContract()) { result ->
        when (val outcome = interpretPairingScan(result.contents)) {
            is ScanOutcome.Accepted -> {
                code = outcome.code
                scanHint = "已读取二维码,点「加入」完成连接。"
            }
            is ScanOutcome.Rejected -> scanHint = outcome.message
        }
    }
    val cameraPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        if (granted) scanLauncher.launch(scanOptions)
        else scanHint = "未获得相机权限,可继续手动输入同步码。"
    }
    val startScan = {
        when {
            !hasCamera -> scanHint = "此设备没有可用相机,请手动输入同步码。"
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED -> scanLauncher.launch(scanOptions)
            else -> cameraPermission.launch(Manifest.permission.CAMERA)
        }
    }
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
            "手机只读。在电脑端「设置 → 同步到手机」点「显示同步码」,用相机扫二维码或手动粘贴。",
            color = Tx2,
            fontSize = 13.sp,
            lineHeight = 20.sp,
        )
        OutlinedButton(
            onClick = startScan,
            enabled = !state.loading,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("扫描二维码")
        }
        OutlinedTextField(
            value = code,
            onValueChange = { code = it; scanHint = "" },
            label = { Text("同步码") },
            placeholder = { Text("GT1.…") },
            singleLine = false,
            maxLines = 4,
            modifier = Modifier.fillMaxWidth(),
        )
        if (scanHint.isNotBlank()) {
            Text(scanHint, color = Tx3, fontSize = 12.sp, lineHeight = 18.sp)
        }
        Button(
            onClick = { onJoin(code) { code = ""; scanHint = "" } },
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
                    "不登录 GitHub 也能用:前台 5 分钟、后台 15 分钟起。加入后可登录 GitHub 提速到 60 秒;二维码只在本机解析,不上传图像。",
                    color = Tx3,
                    fontSize = 12.sp,
                    lineHeight = 18.sp,
                )
            }
        }
        Text(
            "同步码包含解密密钥,请勿分享。手机不会上传任何渠道密钥。",
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
    initialChannelId: String?,
    onChannelConsumed: () -> Unit,
    onUpdateSkipped: () -> Unit,
) {
    var tab by rememberSaveable { mutableIntStateOf(0) }
    LaunchedEffect(initialChannelId) {
        if (initialChannelId != null) tab = 0
    }
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
            DashboardScreen(
                state,
                viewModel,
                initialChannelId,
                onChannelConsumed,
                Modifier.padding(padding),
            )
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
    initialChannelId: String?,
    onChannelConsumed: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val prefs = remember(context) { context.getSharedPreferences("grandettoken_display", Context.MODE_PRIVATE) }
    var sortName by rememberSaveable { mutableStateOf(prefs.getString("channelSort", ChannelSort.Urgency.name) ?: ChannelSort.Urgency.name) }
    var customOrder by remember { mutableStateOf(readChannelOrder(prefs.getString("channelOrder", null))) }
    var sortMenuOpen by remember { mutableStateOf(false) }
    var selectedId by rememberSaveable { mutableStateOf<String?>(null) }
    var expandedId by rememberSaveable { mutableStateOf<String?>(null) }
    val merged = state.merged
    val channels = merged?.channels.orEmpty()
    val sortMode = ChannelSort.entries.firstOrNull { it.name == sortName } ?: ChannelSort.Urgency
    val sorted = remember(channels, sortMode, customOrder) { ChannelOrdering.sort(channels, sortMode, customOrder) }
    val listState = rememberLazyListState()
    val risky = if (sortMode == ChannelSort.Urgency) sorted.filter(ChannelOrdering::needsAttention) else emptyList()
    val rest = if (sortMode == ChannelSort.Urgency) sorted.filterNot(ChannelOrdering::needsAttention) else sorted
    val staleCount = merged?.staleChannels ?: 0

    LaunchedEffect(initialChannelId, merged) {
        val pending = initialChannelId ?: return@LaunchedEffect
        if (merged != null) {
            selectedId = pending
            onChannelConsumed()
        }
    }

    if (selectedId != null) {
        BackHandler { selectedId = null }
        val selected = channels.firstOrNull { it.platformId == selectedId }
        if (selected != null) {
            ChannelDetailScreen(
                selected,
                modifier,
                loggedIn = state.loggedIn,
                backgroundMinutes = state.backgroundMinutes,
            ) { selectedId = null }
        } else {
            Column(modifier.padding(18.dp)) {
                TextButton(onClick = { selectedId = null }) { Text("‹ 返回面板") }
                Banner("渠道暂不可用", "刷新后该渠道已不在同步组中。", Warn)
            }
        }
        return
    }

    fun moveChannel(id: String, delta: Int) {
        val ids = sorted.map { it.platformId }.toMutableList()
        val from = ids.indexOf(id)
        val to = from + delta
        if (from < 0 || to !in ids.indices) return
        java.util.Collections.swap(ids, from, to)
        customOrder = ids + customOrder.filterNot { it in ids }
        prefs.edit().putString("channelOrder", JSONArray(customOrder).toString()).apply()
    }

    Scaffold(
        modifier = modifier,
        containerColor = Canvas,
        topBar = {
            TopAppBar(
                title = {
                    val ck = merged?.checkin
                    SummaryCell("签到", if (ck != null) "${ck.done}/${ck.total}" else "—")
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
            state = listState,
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
                        "数字仍是最后一次成功取数的结果。",
                        Warn,
                    )
                }
            }
            item {
                Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                    SectionTitle("渠道 · ${channels.size}")
                    Spacer(Modifier.weight(1f))
                    Box {
                        TextButton(onClick = { sortMenuOpen = true }) { Text("排序: ${sortMode.label} ▾", fontSize = 12.sp) }
                        DropdownMenu(expanded = sortMenuOpen, onDismissRequest = { sortMenuOpen = false }) {
                            ChannelSort.entries.forEach { option ->
                                DropdownMenuItem(
                                    text = { Text(option.label) },
                                    onClick = {
                                        if (option == ChannelSort.Custom && customOrder.isEmpty()) {
                                            customOrder = sorted.map { it.platformId }
                                            prefs.edit().putString("channelOrder", JSONArray(customOrder).toString()).apply()
                                        }
                                        sortName = option.name
                                        prefs.edit().putString("channelSort", option.name).apply()
                                        sortMenuOpen = false
                                    },
                                )
                            }
                        }
                    }
                }
            }
            if (sortMode == ChannelSort.Balance || sortMode == ChannelSort.DayUsage) {
                item { Text("不同类型和币种分组排序;无数据排在组末。", color = Tx3, fontSize = 11.sp) }
            }
            if (sortMode == ChannelSort.Custom) {
                item { Text("展开渠道后可调整顺序;排序只保存在这台手机。", color = Tx3, fontSize = 11.sp) }
            }
            if (risky.isNotEmpty()) {
                item { SectionTitle("需要关注 · ${risky.size}") }
                items(risky, key = { it.platformId }) { ch ->
                    ChannelCard(ch, expandedId == ch.platformId, sortMode == ChannelSort.Custom, false, false,
                        onToggle = { expandedId = if (expandedId == ch.platformId) null else ch.platformId },
                        onDetail = { selectedId = ch.platformId }, onMove = {})
                }
            }
            if (rest.isNotEmpty()) {
                if (risky.isNotEmpty()) item { SectionTitle("其他渠道 · ${rest.size}") }
                items(rest, key = { it.platformId }) { ch ->
                    val index = sorted.indexOfFirst { it.platformId == ch.platformId }
                    ChannelCard(ch, expandedId == ch.platformId, sortMode == ChannelSort.Custom,
                        index > 0, index >= 0 && index < sorted.lastIndex,
                        onToggle = { expandedId = if (expandedId == ch.platformId) null else ch.platformId },
                        onDetail = { selectedId = ch.platformId }, onMove = { moveChannel(ch.platformId, it) })
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
                            "同步组还没有可展示的渠道。等电脑端发布一次,或检查同步码是否来自另一个组。",
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

}

private fun readChannelOrder(raw: String?): List<String> = runCatching {
    val array = JSONArray(raw ?: "[]")
    (0 until array.length()).mapNotNull { array.optString(it).takeIf(String::isNotBlank) }.distinct()
}.getOrDefault(emptyList())

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
        fontSize = 11.sp,
        fontWeight = FontWeight.Bold,
        letterSpacing = 1.sp,
        modifier = Modifier.padding(top = 8.dp, start = 2.dp),
    )
}

@Composable
internal fun Banner(title: String, body: String, tone: Color) {
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
internal fun DetailRow(label: String, value: String) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
        Text(label, color = Tx3, fontSize = 12.sp)
        Text(value, color = Tx2, fontSize = 12.sp, fontWeight = FontWeight.Medium)
    }
}

@Composable
private fun SettingsSwitchRow(
    label: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(label, color = Tx, fontSize = 13.sp)
        Spacer(Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = onCheckedChange)
    }
}

@Composable
private fun SettingsActionRow(label: String, color: Color, onClick: () -> Unit) {
    TextButton(
        onClick = onClick,
        modifier = Modifier.fillMaxWidth(),
        contentPadding = PaddingValues(horizontal = 2.dp, vertical = 2.dp),
    ) {
        Text(label, color = color, fontSize = 13.sp, modifier = Modifier.fillMaxWidth())
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
    var confirmClearCache by remember { mutableStateOf(false) }
    var showLogin by remember { mutableStateOf(false) }
    var showAbout by remember { mutableStateOf(false) }
    val context = LocalContext.current
    val version = remember(context) { versionLabel(context) }
    val prefs = remember(context) {
        context.getSharedPreferences("grandettoken_update", Context.MODE_PRIVATE)
    }
    var autoCheck by remember { mutableStateOf(prefs.getBoolean("autoCheck", true)) }
    var notifyMaster by remember { mutableStateOf(NotificationHelper.enabled(context)) }
    var notifyBalance by remember { mutableStateOf(NotificationHelper.typeEnabled(context, "balance")) }
    var notifyStale by remember { mutableStateOf(NotificationHelper.typeEnabled(context, "stale")) }
    var notifyFailure by remember { mutableStateOf(NotificationHelper.typeEnabled(context, "failure")) }
    val notifyPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        notifyMaster = granted
        NotificationHelper.setEnabled(context, granted)
        if (granted) NotificationHelper.ensureChannel(context)
    }
    val widgetLifecycle = LocalLifecycleOwner.current
    var widgetRevision by remember { mutableIntStateOf(0) }
    DisposableEffect(widgetLifecycle) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) widgetRevision++
        }
        widgetLifecycle.lifecycle.addObserver(observer)
        onDispose { widgetLifecycle.lifecycle.removeObserver(observer) }
    }
    val widgets = remember(widgetRevision) { WidgetStore.list(context) }
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
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            item { SectionTitle("同步组") }
            item {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .background(Surface1, RoundedCornerShape(12.dp))
                        .padding(14.dp)
                ) {
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text("已加入同步组", color = Tx, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                            Spacer(Modifier.weight(1f))
                            Text("端到端加密", color = Ok, fontSize = 11.sp)
                        }
                        DetailRow("中继", info?.relayId?.take(10)?.plus("…") ?: "—")
                        DetailRow("密钥代数", info?.keyEpoch?.toString() ?: "—")
                        DetailRow("上次刷新", if (state.lastRefresh > 0) relativeTime(state.lastRefresh) else "—")
                        DetailRow("数据源", "${state.merged?.sources ?: 0} 个")
                        DetailRow("刷新模式", if (state.loggedIn) "GitHub 登录 · 60 秒" else "匿名 · 5 分钟")
                        HorizontalDivider(color = Line)
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            Button(
                                onClick = { showLogin = true },
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
                }
            }
            item { SectionTitle("后台刷新") }
            item {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .background(Surface1, RoundedCornerShape(12.dp))
                        .padding(14.dp)
                ) {
                    Column(verticalArrangement = Arrangement.spacedBy(9.dp)) {
                        Text("刷新间隔", color = Tx, fontSize = 13.sp, fontWeight = FontWeight.SemiBold)
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            listOf(15, 30, 60).forEach { m ->
                                FilterChip(
                                    selected = state.backgroundMinutes == m,
                                    onClick = { viewModel.setBackgroundMinutes(m) },
                                    label = { Text("$m 分钟") },
                                )
                            }
                        }
                        Text(
                            "Android 的定时任务最短 15 分钟;前台打开 App 时按匿名 5 分钟 / 登录 60 秒刷新。",
                            color = Tx3,
                            fontSize = 11.sp,
                            lineHeight = 16.sp,
                        )
                    }
                }
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
                            summarizeFailures(state.merged?.failedSources.orEmpty()).forEach {
                                Text(
                                    "${it.count} 个来源未通过校验 · ${it.code}",
                                    color = WarningText,
                                    fontSize = 10.sp,
                                )
                            }
                            Text(
                                "最近成功读取 ${relativeTime(state.lastRefresh)};诊断不含同步密钥、GitHub token、余额、账号或写入者标识。",
                                color = Tx3,
                                fontSize = 10.sp,
                                lineHeight = 15.sp,
                            )
                        }
                    }
                }
            }
            item { SectionTitle("通知") }
            item {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .background(Surface1, RoundedCornerShape(12.dp))
                        .padding(14.dp)
                ) {
                    Column(verticalArrangement = Arrangement.spacedBy(7.dp)) {
                        SettingsSwitchRow("开启通知", notifyMaster) { checked ->
                            if (!checked) {
                                notifyMaster = false
                                NotificationHelper.setEnabled(context, false)
                            } else if (NotificationHelper.permissionGranted(context)) {
                                notifyMaster = true
                                NotificationHelper.setEnabled(context, true)
                            } else if (Build.VERSION.SDK_INT >= 33 &&
                                ContextCompat.checkSelfPermission(
                                    context,
                                    Manifest.permission.POST_NOTIFICATIONS,
                                ) != PackageManager.PERMISSION_GRANTED
                            ) {
                                notifyPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
                            } else {
                                NotificationHelper.openSettings(context)
                            }
                        }
                        Text(
                            "余额偏低、数据超 2 小时未更新、连续 3 次同步失败时提醒;同一提醒 6 小时内不重复,不含余额或账号。",
                            color = Tx3,
                            fontSize = 10.sp,
                            lineHeight = 15.sp,
                        )
                        if (notifyMaster) {
                            HorizontalDivider(color = Line)
                            SettingsSwitchRow("余额提醒", notifyBalance) {
                                notifyBalance = it
                                NotificationHelper.setTypeEnabled(context, "balance", it)
                            }
                            SettingsSwitchRow("数据过期提醒", notifyStale) {
                                notifyStale = it
                                NotificationHelper.setTypeEnabled(context, "stale", it)
                            }
                            SettingsSwitchRow("同步失败提醒", notifyFailure) {
                                notifyFailure = it
                                NotificationHelper.setTypeEnabled(context, "failure", it)
                            }
                            if (!NotificationHelper.permissionGranted(context)) {
                                Text("系统通知权限未开启,提醒不会送达。", color = Warn, fontSize = 10.sp)
                                TextButton(
                                    onClick = { NotificationHelper.openSettings(context) },
                                    contentPadding = PaddingValues(0.dp),
                                ) {
                                    Text("打开系统通知设置", color = Accent, fontSize = 11.sp)
                                }
                            }
                        }
                    }
                }
            }
            item { SectionTitle("桌面小部件") }
            item {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .background(Surface1, RoundedCornerShape(12.dp))
                        .padding(14.dp)
                ) {
                    Column(verticalArrangement = Arrangement.spacedBy(7.dp)) {
                        if (widgets.isEmpty()) {
                            Text(
                                "还没有添加桌面小部件。长按桌面 → 小部件 → Grandettoken Sync,选择 4×1 横条或 4×2 卡片。",
                                color = Tx2,
                                fontSize = 11.sp,
                                lineHeight = 17.sp,
                            )
                        } else {
                            widgets.forEach { widget ->
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Column(Modifier.weight(1f)) {
                                        Text(
                                            "${widget.kind.label} · #${widget.id}",
                                            color = Tx,
                                            fontSize = 12.sp,
                                            fontWeight = FontWeight.SemiBold,
                                        )
                                        Text(
                                            widget.selection.joinToString(" · ") { it.displayName }
                                                .ifBlank { "未选择条目" },
                                            color = Tx3,
                                            fontSize = 10.sp,
                                            maxLines = 1,
                                            overflow = TextOverflow.Ellipsis,
                                        )
                                    }
                                    TextButton(
                                        onClick = {
                                            context.startActivity(
                                                Intent(context, WidgetConfigActivity::class.java)
                                                    .putExtra(WidgetUpdater.EXTRA_APPWIDGET_ID, widget.id)
                                                    .putExtra(WidgetUpdater.EXTRA_KIND, widget.kind.name)
                                                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                                            )
                                        },
                                        contentPadding = PaddingValues(horizontal = 8.dp),
                                    ) {
                                        Text("编辑", color = Accent, fontSize = 11.sp)
                                    }
                                }
                            }
                        }
                        Text(
                            "每一份小部件独立选择渠道和顺序;App 首页排序不会改变小部件。",
                            color = Tx3,
                            fontSize = 10.sp,
                            lineHeight = 15.sp,
                        )
                    }
                }
            }
            item { SectionTitle("关于") }
            item {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .background(Surface1, RoundedCornerShape(12.dp))
                        .padding(14.dp)
                ) {
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        DetailRow("当前版本", version)
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text("自动检查更新", color = Tx, fontSize = 13.sp)
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
                            Text("发现新版本,点「检查更新」查看。", color = Accent, fontSize = 11.sp)
                        }
                        TextButton(
                            onClick = { showAbout = true },
                            contentPadding = PaddingValues(0.dp),
                        ) {
                            Text(if (updateAvailable) "检查更新 · 有新版本" else "检查更新", color = Accent, fontSize = 13.sp)
                        }
                        Text("更新会先校验官方签名清单、APK 哈希和签名,再交给 Android 系统确认安装。", color = Tx3, fontSize = 10.sp, lineHeight = 15.sp)
                    }
                }
            }
            item { SectionTitle("数据") }
            item {
                Box(
                    Modifier
                        .fillMaxWidth()
                        .background(Surface1, RoundedCornerShape(12.dp))
                        .padding(start = 12.dp, end = 12.dp, top = 4.dp, bottom = 4.dp)
                ) {
                    Column {
                        SettingsActionRow("清除本地缓存", Warn) { confirmClearCache = true }
                        HorizontalDivider(color = Line)
                        SettingsActionRow("退出同步组", Bad) { confirmLogout = true }
                    }
                }
            }
            item {
                Text(
                    "Grandettoken Sync · 开源 · 手机只读",
                    color = Tx3,
                    fontSize = 10.sp,
                    modifier = Modifier.padding(start = 2.dp),
                )
            }
            item { Spacer(Modifier.height(14.dp)) }
        }
    }

    if (confirmClearCache) {
        AlertDialog(
            onDismissRequest = { confirmClearCache = false },
            title = { Text("清除本地缓存？") },
            text = { Text("只删除本机缓存的合并结果和序号水位,不动电脑端和 GitHub 上的密文;下次刷新会重新读取。") },
            confirmButton = {
                TextButton(onClick = {
                    confirmClearCache = false
                    viewModel.clearCache()
                    viewModel.refresh(true)
                }) { Text("清除", color = Warn) }
            },
            dismissButton = {
                TextButton(onClick = { confirmClearCache = false }) { Text("取消") }
            },
        )
    }
    if (confirmLogout) {
        AlertDialog(
            onDismissRequest = { confirmLogout = false },
            title = { Text("退出同步组？") },
            text = { Text("会删除本机主密钥、缓存、后台任务和 GitHub 登录;GitHub 上的密文不会自动删除。需要重新扫码才能再次查看;重新扫另一个同步码即可更换同步组。") },
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
    if (showLogin) {
        GitHubLoginDialog(viewModel, state) { showLogin = false }
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
    val progressFlow = remember { MutableStateFlow<Pair<Long, Long>?>(null) }
    val progress by progressFlow.collectAsState()
    var downloadJob by remember { mutableStateOf<Job?>(null) }
    var confirmMobile by remember { mutableStateOf(false) }
    var pendingMobileDownload by remember { mutableStateOf<Pair<UpdatePayload, UpdateArtifact>?>(null) }

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
            status = updateFailText(failedCode, downloading = false)
        } finally {
            busy = false
        }
    }

    fun startDownload(target: Pair<UpdatePayload, UpdateArtifact>) {
        downloadJob?.cancel()
        downloadedApk = null
        failedCode = null
        progressFlow.value = 0L to target.second.sizeBytes
        downloadJob = scope.launch {
            busy = true
            status = "正在下载并验证 APK…"
            try {
                downloadedApk = withContext(Dispatchers.IO) {
                    AndroidUpdateRepository.downloadAndVerify(
                        context,
                        target.first,
                        target.second,
                    ) { read, total ->
                        progressFlow.value = read to total
                    }
                }
                status = "下载和验证完成"
            } catch (_: CancellationException) {
                status = "已取消下载"
            } catch (e: Exception) {
                failedCode = (e as? UpdateCheckException)?.code ?: "UPDATE_DOWNLOAD_FAILED"
                status = updateFailText(failedCode, downloading = true)
            } finally {
                busy = false
                downloadJob = null
                progressFlow.value = null
            }
        }
    }

    fun requestDownload() {
        val target = update ?: return
        if (isMetered(context)) {
            pendingMobileDownload = target
            confirmMobile = true
        } else {
            startDownload(target)
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
                    Text("发布时间: ${payload.publishedAt}", color = Tx3, fontSize = 10.sp)
                    Text("APK 大小: ${formatBytes(artifact.sizeBytes)}", color = Tx3, fontSize = 10.sp)
                    if (payload.notes.isNotBlank()) {
                        Text(payload.notes, color = Tx2, fontSize = 11.sp, lineHeight = 16.sp)
                    }
                }
                progress?.let { (read, total) ->
                    LinearProgressIndicator(
                        progress = { if (total > 0) read.toFloat() / total else 0f },
                        modifier = Modifier.fillMaxWidth(),
                        color = Accent,
                        trackColor = Surface3,
                    )
                    Text(
                        "${formatBytes(read)} / ${formatBytes(total)}",
                        color = Tx3,
                        fontSize = 10.sp,
                    )
                }
                if (downloadedApk != null) {
                    Text("下载与安全校验完成。点击安装后由 Android 系统确认。", color = Ok, fontSize = 11.sp)
                }
                if (failedCode == "UPDATE_CERT_NOT_CONFIGURED") {
                    Text("当前安装包没有配置可信签名证书,请安装官方发布版后再检查更新。", color = Tx3, fontSize = 10.sp)
                }
            }
        },
        confirmButton = {
            when {
                busy && downloadJob != null -> TextButton(onClick = { downloadJob?.cancel() }) {
                    Text("取消下载")
                }
                busy -> TextButton(onClick = {}, enabled = false) { Text("请稍候…") }
                update != null && downloadedApk == null -> TextButton(onClick = { requestDownload() }) {
                    Text("下载更新")
                }
                downloadedApk != null -> TextButton(onClick = {
                    try {
                        if (!AndroidUpdateRepository.canInstallPackages(context)) {
                            status = "请在系统弹窗允许 Grandettoken Sync 安装,然后返回点「安装更新」。"
                            AndroidUpdateRepository.requestInstallPermission(context)
                        } else {
                            update?.first?.let { payload ->
                                AndroidUpdateRepository.markInstalling(context, payload.versionCode, downloadedApk!!)
                            }
                            AndroidUpdateRepository.installApk(context, downloadedApk!!)
                            status = "已交给 Android 安装器,请在系统界面确认。"
                        }
                    } catch (_: Exception) {
                        status = "无法打开系统安装器,请稍后重试。"
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

    if (confirmMobile) {
        val target = pendingMobileDownload
        AlertDialog(
            onDismissRequest = { confirmMobile = false },
            title = { Text("使用移动数据下载？") },
            text = {
                Text(
                    "当前网络可能是移动数据,APK 约 ${formatBytes(target?.second?.sizeBytes ?: 0L)}。下载会消耗流量。",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirmMobile = false
                    target?.let { startDownload(it) }
                    pendingMobileDownload = null
                }) { Text("继续下载") }
            },
            dismissButton = {
                TextButton(onClick = {
                    confirmMobile = false
                    pendingMobileDownload = null
                }) { Text("取消") }
            },
        )
    }
}

private fun isMetered(context: Context): Boolean {
    val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
        ?: return false
    val network = cm.activeNetwork ?: return false
    val caps = cm.getNetworkCapabilities(network) ?: return false
    return !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
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

// UPDATE_* 错误码对用户隐藏:状态行只给中文短语,码保留在 failedCode 里(诊断与测试仍可读)
private fun updateFailText(code: String?, downloading: Boolean): String {
    val reason = when (code) {
        "UPDATE_NETWORK_ERROR" -> "连不上更新服务器,检查网络后重试"
        "UPDATE_SIGNATURE_INVALID", "UPDATE_KEY_INVALID", "UPDATE_KEY_UNKNOWN" -> "签名校验没过,安装包可能不完整"
        "UPDATE_APK_HASH_MISMATCH", "UPDATE_HASH_INVALID" -> "安装包哈希校验没过"
        "UPDATE_CERT_NOT_CONFIGURED" -> "签名证书未配置,见下方说明"
        "UPDATE_HOST_NOT_TRUSTED" -> "下载地址不受信任"
        "UPDATE_NO_COMPATIBLE_APK", "UPDATE_UNSUPPORTED_SDK", "UPDATE_ABI_UNSUPPORTED" -> "没有适配本机的安装包"
        "UPDATE_STORAGE_ERROR" -> "本机存储不可用"
        "UPDATE_MANIFEST_INVALID", "UPDATE_MANIFEST_EMPTY", "UPDATE_PAYLOAD_INVALID", "UPDATE_SCHEMA_UNSUPPORTED" -> "更新清单格式不对"
        else -> "稍后再试"
    }
    return if (downloading) "下载或校验没成: $reason" else "检查更新没成: $reason"
}

internal fun channelValue(ch: ChannelSnapshot): String = when (ch.kind) {
    ChannelKind.Quota -> Decimal.display(ch.percent, "%")
    ChannelKind.Points -> Decimal.display(ch.remaining, ch.unit, points = true)
    ChannelKind.Money -> Decimal.display(ch.remaining, ch.unit)
}

internal fun channelFraction(ch: ChannelSnapshot): Float? {
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

internal fun kindLabel(kind: ChannelKind): String = when (kind) {
    ChannelKind.Money -> "金额型"
    ChannelKind.Quota -> "配额型"
    ChannelKind.Points -> "积分型"
}

internal fun relativeTime(ts: Long): String {
    if (ts <= 0) return "—"
    val delta = (System.currentTimeMillis() / 1000 - ts).coerceAtLeast(0)
    return when {
        delta < 60 -> "刚刚"
        delta < 3600 -> "${delta / 60} 分钟前"
        delta < 86_400 -> "${delta / 3600} 小时前"
        else -> "${delta / 86_400} 天前"
    }
}

internal fun formatDate(ts: Long): String =
    if (ts <= 0) "—" else SimpleDateFormat("MM-dd HH:mm", Locale.getDefault()).format(Date(ts * 1000))
