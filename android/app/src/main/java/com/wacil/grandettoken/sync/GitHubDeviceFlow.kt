package com.wacil.grandettoken.sync

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.FormBody
import okhttp3.OkHttpClient
import okhttp3.Request
import org.json.JSONObject
import java.util.concurrent.TimeUnit

class GitHubLoginException(
    val code: String,
    message: String,
) : Exception(message)

data class GitHubDeviceCode(
    val deviceCode: String,
    val userCode: String,
    val verificationUri: String,
    val expiresIn: Long,
    val interval: Long,
)

sealed interface GitHubPollResult {
    data object Pending : GitHubPollResult
    data object SlowDown : GitHubPollResult
    data class Success(val token: String, val scope: String?) : GitHubPollResult
    data class Failure(val message: String) : GitHubPollResult
}

/**
 * GitHub Device Flow。设备码只存在于当前内存，不写日志、不入库；
 * 只请求零 scope，调用方必须检查返回 scope 为空后再保存 token。
 */
object GitHubDeviceFlow {
    private const val DEVICE_CODE_URL = "https://github.com/login/device/code"
    private const val TOKEN_URL = "https://github.com/login/oauth/access_token"
    private const val USER_URL = "https://api.github.com/user"
    private const val FALLBACK_VERIFICATION = "https://github.com/login/device"

    private val client = OkHttpClient.Builder()
        .connectTimeout(20, TimeUnit.SECONDS)
        .readTimeout(30, TimeUnit.SECONDS)
        .callTimeout(40, TimeUnit.SECONDS)
        .build()

    suspend fun start(clientId: String): GitHubDeviceCode = withContext(Dispatchers.IO) {
        val clean = clientId.trim()
        if (clean.isEmpty()) throw GitHubLoginException("LOGIN_CLIENT_ID", "请填写公开 Client ID")
        val request = Request.Builder()
            .url(DEVICE_CODE_URL)
            .header("Accept", "application/json")
            .post(FormBody.Builder().add("client_id", clean).build())
            .build()
        client.newCall(request).execute().use { response ->
            if (!response.isSuccessful) {
                throw GitHubLoginException("LOGIN_HTTP_${response.code}", "GitHub 授权服务返回 ${response.code}")
            }
            val body = response.body?.string()
                ?: throw GitHubLoginException("LOGIN_EMPTY", "GitHub 授权服务返回空响应")
            val obj = runCatching { JSONObject(body) }
                .getOrElse { throw GitHubLoginException("LOGIN_INVALID", "GitHub 授权响应无法解析") }
            obj.optString("error").takeIf { it.isNotBlank() }?.let { error ->
                throw GitHubLoginException("LOGIN_$error", errorText(error))
            }
            GitHubDeviceCode(
                deviceCode = obj.getString("device_code"),
                userCode = obj.getString("user_code"),
                verificationUri = obj.optString("verification_uri", FALLBACK_VERIFICATION)
                    .ifBlank { FALLBACK_VERIFICATION },
                expiresIn = obj.optLong("expires_in", 900).coerceIn(60, 3600),
                interval = obj.optLong("interval", 5).coerceIn(5, 30),
            )
        }
    }

    suspend fun pollOnce(clientId: String, deviceCode: String): GitHubPollResult =
        withContext(Dispatchers.IO) {
            val request = Request.Builder()
                .url(TOKEN_URL)
                .header("Accept", "application/json")
                .post(
                    FormBody.Builder()
                        .add("client_id", clientId.trim())
                        .add("device_code", deviceCode)
                        .add("grant_type", "urn:ietf:params:oauth:grant-type:device_code")
                        .build()
                )
                .build()
            client.newCall(request).execute().use { response ->
                if (!response.isSuccessful) {
                    return@use GitHubPollResult.Failure("GitHub 授权服务返回 ${response.code}")
                }
                val body = response.body?.string()
                    ?: return@use GitHubPollResult.Failure("GitHub 授权响应为空")
                val obj = runCatching { JSONObject(body) }
                    .getOrElse { return@use GitHubPollResult.Failure("GitHub 授权响应无法解析") }
                val token = obj.optString("access_token")
                if (token.isNotBlank()) {
                    return@use GitHubPollResult.Success(token, obj.optString("scope").takeIf { it.isNotBlank() })
                }
                when (val error = obj.optString("error")) {
                    "authorization_pending" -> GitHubPollResult.Pending
                    "slow_down" -> GitHubPollResult.SlowDown
                    "access_denied" -> GitHubPollResult.Failure("你已取消 GitHub 授权")
                    "expired_token" -> GitHubPollResult.Failure("授权码已过期,请重新开始")
                    "" -> GitHubPollResult.Failure("GitHub 授权状态未知")
                    else -> GitHubPollResult.Failure(errorText(error))
                }
            }
        }

    suspend fun verifyUser(token: String): String = withContext(Dispatchers.IO) {
        val request = Request.Builder()
            .url(USER_URL)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "Grandettoken-Sync-Android/1")
            .header("Authorization", "Bearer $token")
            .get()
            .build()
        client.newCall(request).execute().use { response ->
            when (response.code) {
                401 -> throw GitHubLoginException("SYNC_AUTH", "GitHub 令牌无效或已过期")
                403, 429 -> throw GitHubLoginException("SYNC_RATE_LIMITED", "GitHub 请求过于频繁,请稍后再试")
            }
            if (!response.isSuccessful) {
                throw GitHubLoginException("LOGIN_USER_${response.code}", "无法读取 GitHub 账号")
            }
            val body = response.body?.string()
                ?: throw GitHubLoginException("LOGIN_USER_EMPTY", "GitHub 账号响应为空")
            val login = runCatching { JSONObject(body).optString("login") }.getOrNull()
            if (login.isNullOrBlank()) {
                throw GitHubLoginException("LOGIN_USER_INVALID", "GitHub 账号响应缺少用户名")
            }
            login
        }
    }

    private fun errorText(error: String): String = when (error) {
        "authorization_pending" -> "等待 GitHub 授权"
        "slow_down" -> "请稍后再试"
        "access_denied" -> "你已取消 GitHub 授权"
        "expired_token" -> "授权码已过期,请重新开始"
        "device_flow_disabled" -> "该 OAuth App 未开启 Device Flow"
        "incorrect_client_credentials" -> "Client ID 不正确"
        "unsupported_grant_type" -> "GitHub 授权类型不受支持"
        else -> "GitHub 授权失败($error)"
    }
}
