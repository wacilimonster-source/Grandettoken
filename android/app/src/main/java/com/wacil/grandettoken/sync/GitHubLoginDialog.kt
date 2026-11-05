package com.wacil.grandettoken.sync

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * 手机 GitHub 登录。Client ID 是公开值；支持填桌面 OAuth App 的 Client ID（b），
 * 也支持填独立手机 OAuth App 的 Client ID（a）。device_code 只留在内存。
 */
@Composable
internal fun GitHubLoginDialog(
    viewModel: SyncViewModel,
    state: SyncUiState,
    onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var clientId by rememberSaveable { mutableStateOf(viewModel.loginClientId().orEmpty()) }
    var device by remember { mutableStateOf<GitHubDeviceCode?>(null) }
    var busy by remember { mutableStateOf(false) }
    var error by rememberSaveable { mutableStateOf<String?>(null) }
    var job by remember { mutableStateOf<Job?>(null) }

    fun dismiss() {
        job?.cancel()
        job = null
        busy = false
        device = null
        onDismiss()
    }

    fun start() {
        val clean = clientId.trim()
        if (!GitHubLoginPolicy.validClientId(clean)) {
            error = "Client ID 格式不正确,请粘贴公开的 Client ID"
            return
        }
        viewModel.saveLoginClientId(clean)
        busy = true
        error = null
        job = scope.launch {
            try {
                val code = GitHubDeviceFlow.start(clean)
                device = code
                var interval = code.interval
                val deadline = System.currentTimeMillis() + code.expiresIn * 1000
                while (isActive && System.currentTimeMillis() < deadline) {
                    delay(interval * 1000)
                    when (val result = GitHubDeviceFlow.pollOnce(clean, code.deviceCode)) {
                        GitHubPollResult.Pending -> Unit
                        GitHubPollResult.SlowDown -> interval += 5
                        is GitHubPollResult.Success -> {
                            if (!GitHubLoginPolicy.isZeroScope(result.scope)) {
                                error = "授权返回了额外权限(${result.scope}),未保存 token。请到 GitHub 撤销该应用授权后重试。"
                                busy = false
                                device = null
                                return@launch
                            }
                            if (!isActive) return@launch
                            val login = GitHubDeviceFlow.verifyUser(result.token)
                            viewModel.verifyTokenRead(result.token)
                            if (!isActive) return@launch
                            viewModel.completeLogin(result.token, login)
                            busy = false
                            device = null
                            onDismiss()
                            return@launch
                        }
                        is GitHubPollResult.Failure -> {
                            error = result.message
                            busy = false
                            device = null
                            return@launch
                        }
                    }
                }
                if (isActive) {
                    error = "授权已过期,请重新开始"
                    busy = false
                    device = null
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                error = e.message ?: "登录失败,请稍后重试"
                busy = false
                device = null
            }
        }
    }

    AlertDialog(
        onDismissRequest = ::dismiss,
        title = {
            Text(
                when {
                    state.loggedIn -> "GitHub 已登录"
                    device != null -> "等待 GitHub 授权"
                    else -> "登录 GitHub 提速"
                }
            )
        },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                if (state.loggedIn) {
                    Text(
                        "已登录 @${viewModel.githubLogin() ?: "GitHub"}。前台按 60 秒刷新,后台仍按 15/30/60 分钟;手机不会写入 Gist。",
                        color = Tx2,
                        fontSize = 12.sp,
                        lineHeight = 18.sp,
                    )
                } else if (device != null) {
                    Text(
                        "在 GitHub 授权页输入下面的授权码:",
                        color = Tx2,
                        fontSize = 12.sp,
                    )
                    Text(
                        device!!.userCode,
                        color = Tx,
                        fontSize = 22.sp,
                        fontWeight = FontWeight.Bold,
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        TextButton(
                            onClick = {
                                val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                                clipboard.setPrimaryClip(
                                    ClipData.newPlainText("GitHub 授权码", device!!.userCode)
                                )
                            },
                            contentPadding = androidx.compose.foundation.layout.PaddingValues(0.dp),
                        ) { Text("复制授权码", fontSize = 11.sp) }
                        TextButton(
                            onClick = {
                                runCatching {
                                    context.startActivity(
                                        Intent(Intent.ACTION_VIEW, Uri.parse(device!!.verificationUri))
                                            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                                    )
                                }.onFailure { error = "无法打开浏览器,请手动访问 github.com/login/device" }
                            },
                            contentPadding = androidx.compose.foundation.layout.PaddingValues(0.dp),
                        ) { Text("打开 GitHub", fontSize = 11.sp) }
                    }
                    Text(
                        "授权完成后这里会自动继续。手机请求零 scope;如果 GitHub 返回额外权限,不会保存 token。",
                        color = Tx3,
                        fontSize = 10.sp,
                        lineHeight = 15.sp,
                    )
                } else {
                    Text(
                        "填桌面 OAuth App 的公开 Client ID(复用桌面授权),或独立手机 OAuth App 的 Client ID。只保存这个公开值,不保存 Client Secret。",
                        color = Tx2,
                        fontSize = 12.sp,
                        lineHeight = 18.sp,
                    )
                    OutlinedTextField(
                        value = clientId,
                        onValueChange = { clientId = it; error = null },
                        label = { Text("公开 Client ID") },
                        placeholder = { Text("Iv1.… / Ov23li…") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Text(
                        "手机请求零 scope。若实测无法读取 Secret Gist,会停止登录并保留匿名模式,不会申请 gist 写权限。",
                        color = Tx3,
                        fontSize = 10.sp,
                        lineHeight = 15.sp,
                    )
                }
                error?.let {
                    Spacer(Modifier.height(2.dp))
                    Text(it, color = Bad, fontSize = 11.sp, lineHeight = 16.sp)
                }
            }
        },
        confirmButton = {
            when {
                state.loggedIn -> TextButton(onClick = ::dismiss) { Text("关闭") }
                device != null -> TextButton(onClick = ::dismiss) { Text("取消") }
                else -> Button(
                    onClick = ::start,
                    enabled = !busy && clientId.isNotBlank(),
                ) { Text(if (busy) "正在连接…" else "开始授权") }
            }
        },
        dismissButton = when {
            state.loggedIn -> {
                {
                    TextButton(
                        onClick = {
                            viewModel.disconnectGitHub()
                            dismiss()
                        }
                    ) { Text("断开登录", color = Bad) }
                }
            }
            device == null -> {
                { TextButton(onClick = ::dismiss) { Text("取消") } }
            }
            else -> null
        },
    )
}
