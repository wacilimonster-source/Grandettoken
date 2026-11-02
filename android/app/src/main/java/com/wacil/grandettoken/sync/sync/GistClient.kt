package com.wacil.grandettoken.sync.sync

import kotlinx.serialization.Serializable
import okhttp3.OkHttpClient
import okhttp3.Request
import java.io.IOException
import java.util.concurrent.TimeUnit

data class GistFile(val name: String, val content: String)

sealed class FetchResult {
    data class NotModified(val etag: String?) : FetchResult()
    data class Loaded(val files: List<GistFile>, val etag: String?) : FetchResult()
}

/**
 * 手机只读 Gist 客户端。匿名时 token = null；登录后带 Bearer。
 * 所有响应都做截断/大小检查，错误只保留状态码与错误码。
 */
class GistClient(private val token: String? = null) {
    private val http = OkHttpClient.Builder()
        .connectTimeout(20, TimeUnit.SECONDS)
        .readTimeout(20, TimeUnit.SECONDS)
        .writeTimeout(20, TimeUnit.SECONDS)
        .build()

    fun get(gistId: String, etag: String?): FetchResult {
        if (!SyncCode.validGistId(gistId)) {
            throw SyncException(SyncCodes.GIST_ID_INVALID, "Gist ID 格式不正确")
        }
        val builder = Request.Builder()
            .url("https://api.github.com/gists/$gistId")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "Grandettoken-Sync-Android/1")
        if (!token.isNullOrBlank()) {
            builder.header("Authorization", "Bearer $token")
        }
        if (!etag.isNullOrBlank()) {
            builder.header("If-None-Match", etag)
        }

        val response = try {
            http.newCall(builder.build()).execute()
        } catch (e: IOException) {
            throw SyncException(SyncCodes.NETWORK, "请求 GitHub 失败: ${e.message ?: "网络错误"}")
        }

        response.use { resp ->
            val remaining = resp.header("x-ratelimit-remaining")?.toLongOrNull()
            val resetAt = resp.header("x-ratelimit-reset")?.toLongOrNull()
            val retryAfter = resp.header("retry-after")?.toLongOrNull()
            when (resp.code) {
                304 -> return FetchResult.NotModified(
                    resp.header("etag") ?: etag,
                )
                401 -> throw SyncException(SyncCodes.AUTH, "GitHub 令牌无效或已过期")
                403 -> {
                    if (remaining == 0L || retryAfter != null) {
                        throw SyncException(
                            SyncCodes.RATE_LIMITED,
                            "GitHub 限流",
                            retryAfterSec = retryAfter,
                            resetAt = resetAt,
                        )
                    }
                    throw SyncException(SyncCodes.FORBIDDEN, "GitHub 拒绝访问（权限不足）")
                }
                404 -> throw SyncException(SyncCodes.NOT_FOUND, "Gist 不存在或不可访问")
                429 -> throw SyncException(
                    SyncCodes.RATE_LIMITED,
                    "GitHub 限流（429）",
                    retryAfterSec = retryAfter,
                    resetAt = resetAt,
                )
                in 500..599 -> throw SyncException(
                    SyncCodes.UPSTREAM,
                    "GitHub 服务端错误（${resp.code}）",
                )
            }
            if (!resp.isSuccessful) {
                throw SyncException(SyncCodes.UPSTREAM, "GitHub 返回 ${resp.code}")
            }

            val body = resp.body?.string()
                ?: throw SyncException(SyncCodes.UPSTREAM, "GitHub 响应为空")
            if (body.toByteArray(Charsets.UTF_8).size > MAX_RESPONSE_BYTES) {
                throw SyncException(SyncCodes.TOO_LARGE, "Gist 响应超过上限")
            }
            val parsed = try {
                SyncJson.decodeFromString(GistResponse.serializer(), body)
            } catch (_: Exception) {
                throw SyncException(SyncCodes.PAYLOAD_INVALID, "Gist 响应不是预期 JSON")
            }
            if (parsed.truncated) {
                throw SyncException(SyncCodes.TRUNCATED, "Gist 响应被 GitHub 截断")
            }

            val files = ArrayList<GistFile>()
            var total = 0L
            for ((name, file) in parsed.files) {
                if (SyncCode.sourceFromFileName(name) == null) continue
                if (files.size >= MAX_SOURCES) {
                    throw SyncException(SyncCodes.TOO_LARGE, "来源文件超过 $MAX_SOURCES 个")
                }
                if (file.truncated) {
                    throw SyncException(SyncCodes.TRUNCATED, "文件 $name 被截断")
                }
                val content = file.content
                    ?: throw SyncException(SyncCodes.TRUNCATED, "文件 $name 没有内联内容")
                val bytes = content.toByteArray(Charsets.UTF_8).size.toLong()
                if (file.size > MAX_SOURCE_FILE_BYTES || bytes > MAX_SOURCE_FILE_BYTES) {
                    throw SyncException(SyncCodes.TOO_LARGE, "文件 $name 超过单文件上限")
                }
                total += bytes
                if (total > MAX_TOTAL_BYTES) {
                    throw SyncException(SyncCodes.TOO_LARGE, "来源文件总大小超过上限")
                }
                files.add(GistFile(name, content))
            }
            return FetchResult.Loaded(files, resp.header("etag"))
        }
    }

    fun close() {
        http.dispatcher.executorService.shutdown()
        http.connectionPool.evictAll()
    }

    companion object {
        const val MAX_SOURCES = 32
        const val MAX_SOURCE_FILE_BYTES = 256L * 1024L
        const val MAX_TOTAL_BYTES = 4L * 1024L * 1024L
        const val MAX_RESPONSE_BYTES = 8L * 1024L * 1024L
    }
}

@Serializable
private data class GistResponse(
    val id: String = "",
    val truncated: Boolean = false,
    val files: Map<String, GistFileResponse> = emptyMap(),
)

@Serializable
private data class GistFileResponse(
    val content: String? = null,
    val truncated: Boolean = false,
    val size: Long = 0,
)
