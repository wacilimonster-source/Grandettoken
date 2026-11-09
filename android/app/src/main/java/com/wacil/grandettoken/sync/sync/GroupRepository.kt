package com.wacil.grandettoken.sync.sync

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

sealed class JoinResult {
    data class Success(val merged: MergedSnapshot) : JoinResult()
    data class Error(val code: String, val message: String) : JoinResult()
}

sealed class RefreshResult {
    data class Loaded(val merged: MergedSnapshot) : RefreshResult()
    data class NotModified(val merged: MergedSnapshot?) : RefreshResult()
    data object NoGroup : RefreshResult()
    data class RateLimited(val until: Long) : RefreshResult()
    data class Error(
        val code: String,
        val message: String,
        val retryAt: Long? = null,
    ) : RefreshResult()
}

class GroupRepository(context: Context) {
    private val appContext = context.applicationContext
    private val store = SyncStore(appContext)

    fun hasGroup(): Boolean = store.hasGroup()

    fun info(): GroupInfo? = store.groupInfo()

    fun cached(): MergedSnapshot? = store.loadCache()

    fun hasToken(): Boolean = store.hasToken()

    fun loginClientId(): String? = store.loadLoginClientId()

    fun saveLoginClientId(clientId: String) = store.saveLoginClientId(clientId)

    fun githubLogin(): String? = store.loadGithubLogin()

    fun saveGithubLogin(login: String) = store.saveGithubLogin(login)

    /** 用登录 token 实际读取当前 Gist，确认零 scope 授权也能访问同步组。 */
    suspend fun verifyTokenRead(token: String): Unit = withContext(Dispatchers.IO) {
        val group = store.loadGroup()
            ?: throw SyncException(SyncCodes.KEY_INVALID, "本机没有同步组,请先扫码加入")
        val client = GistClient(token)
        try {
            client.get(group.gistIdHex(), null)
        } finally {
            client.close()
        }
    }

    suspend fun join(codeText: String): JoinResult = withContext(Dispatchers.IO) {
        gate.withLock { joinLocked(codeText) }
    }

    private fun joinLocked(codeText: String): JoinResult {
        val code = try {
            SyncCode.decode(codeText)
        } catch (e: SyncException) {
            return JoinResult.Error(e.code, e.message)
        }
        val gistId = code.gistIdHex()
        val fetched = try {
            fetchWithToken(null, gistId, null)
        } catch (e: SyncException) {
            return JoinResult.Error(e.code, e.message)
        }
        val files = when (fetched) {
            is FetchResult.NotModified -> emptyList()
            is FetchResult.Loaded -> fetched.files
        }
        val accepted = mutableListOf<SyncSnapshot>()
        val failed = mutableListOf<String>()
        val watermarks = mutableListOf<Triple<String, Long, String>>()
        for (file in files) {
            val source = SyncCode.sourceFromFileName(file.name) ?: continue
            val hash = SyncCrypto.hexEncode(SyncCrypto.sha256(file.content.toByteArray(Charsets.UTF_8)))
            try {
                val plaintext = SyncCrypto.open(
                    code.key,
                    code.relayId,
                    source,
                    code.keyEpoch,
                    file.content.toByteArray(Charsets.UTF_8),
                )
                val snap = SyncJson.decodeFromString(
                    SyncSnapshot.serializer(),
                    plaintext.toString(Charsets.UTF_8),
                )
                if (snap.source != source || !SyncMerge.validate(snap)) {
                    failed.add("$source:${SyncCodes.PAYLOAD_INVALID}")
                    continue
                }
                watermarks.add(Triple(source, snap.seq, hash))
                accepted.add(snap)
            } catch (e: SyncException) {
                failed.add("$source:${e.code}")
            } catch (_: Exception) {
                failed.add("$source:${SyncCodes.PAYLOAD_INVALID}")
            }
        }
        if (accepted.isEmpty()) {
            return JoinResult.Error(
                SyncCodes.EMPTY_GROUP,
                "同步组还没有可验证快照;请先在电脑端发布一次",
            )
        }
        val merged = SyncMerge.merge(accepted, failed)
        val now = System.currentTimeMillis() / 1000
        store.clearCache()
        store.saveGroup(code)
        store.saveCache(merged)
        store.saveEtag((fetched as? FetchResult.Loaded)?.etag)
        store.saveLastRefresh(now)
        store.saveNextAllowed(0)
        watermarks.forEach { (source, seq, hash) -> store.saveWatermark(source, seq, hash) }
        return JoinResult.Success(merged)
    }

    suspend fun refresh(force: Boolean = false): RefreshResult = withContext(Dispatchers.IO) {
        gate.withLock { refreshLocked(force) }
    }

    /**
     * force = 用户手动刷新：既跳过限流门，也跳过 If-None-Match，真正重新读取。
     * 否则 Gist 未变时 304 会把同一份（可能不完整的）缓存再画一遍。
     */
    private fun refreshLocked(force: Boolean): RefreshResult {
        val group = store.loadGroup()
            ?: return RefreshResult.NoGroup
        val gistId = group.gistIdHex()
        val now = System.currentTimeMillis() / 1000
        if (!force) {
            val next = store.loadNextAllowed()
            if (next > now) return RefreshResult.RateLimited(next)
        }

        val etag = if (force) null else store.loadEtag()
        var usedToken = store.loadToken()
        var fetched: FetchResult
        try {
            fetched = fetchWithToken(usedToken, gistId, etag)
        } catch (e: SyncException) {
            if (e.code == SyncCodes.AUTH && usedToken != null) {
                // 401：停止使用 token，保留缓存，降为匿名策略再试一次。
                store.clearToken()
                store.clearGithubLogin()
                usedToken = null
                try {
                    fetched = fetchWithToken(null, gistId, etag)
                } catch (e2: SyncException) {
                    return handleFailure(e2, now)
                }
            } else if (e.code == SyncCodes.KEY_INVALID) {
                return RefreshResult.Error(
                    e.code,
                    "本机密钥已失效,请退出同步组后重新扫码",
                )
            } else {
                return handleFailure(e, now)
            }
        }

        return when (fetched) {
            is FetchResult.NotModified -> {
                store.saveLastRefresh(now)
                store.saveNextAllowed(0)
                RefreshResult.NotModified(store.loadCache())
            }
            is FetchResult.Loaded -> {
                val accepted = mutableListOf<SyncSnapshot>()
                val failed = mutableListOf<String>()
                var skipped = 0
                for (file in fetched.files) {
                    val source = SyncCode.sourceFromFileName(file.name) ?: continue
                    val hash = SyncCrypto.hexEncode(
                        SyncCrypto.sha256(file.content.toByteArray(Charsets.UTF_8))
                    )
                    try {
                        val plaintext = SyncCrypto.open(
                            group.key,
                            group.relayId,
                            source,
                            group.keyEpoch,
                            file.content.toByteArray(Charsets.UTF_8),
                        )
                        val snap = SyncJson.decodeFromString(
                            SyncSnapshot.serializer(),
                            plaintext.toString(Charsets.UTF_8),
                        )
                        if (snap.source != source || !SyncMerge.validate(snap)) {
                            failed.add("$source:${SyncCodes.PAYLOAD_INVALID}")
                            continue
                        }
                        val wm = store.loadWatermark(source)
                        if (wm != null && snap.seq < wm.first) {
                            // 回放/乱序：不能拿它覆盖已见水位，但也不能让整源从
                            // 本轮合并里消失；记下来，稍后用旧缓存补齐。
                            skipped++
                            continue
                        }
                        if (wm != null && snap.seq == wm.first && hash != wm.second) {
                            failed.add("$source:${SyncCodes.CONFLICT}")
                            continue
                        }
                        // 同 seq 同密文不是新数据，但本来源仍然有效：必须参与合并，
                        // 否则另一个来源一发布，这个来源的渠道就会整块从缓存消失。
                        if (wm == null || snap.seq > wm.first) {
                            store.saveWatermark(source, snap.seq, hash)
                        }
                        accepted.add(snap)
                    } catch (e: SyncException) {
                        failed.add("$source:${e.code}")
                    } catch (_: Exception) {
                        failed.add("$source:${SyncCodes.PAYLOAD_INVALID}")
                    }
                }
                if (accepted.isEmpty()) {
                    if (failed.isEmpty()) {
                        // 没有新内容（水位命中/只有元数据变化）：沿用缓存。
                        store.saveEtag(fetched.etag)
                        store.saveLastRefresh(now)
                        store.saveNextAllowed(0)
                        return RefreshResult.NotModified(store.loadCache())
                    }
                    // 全部来源都不可用：绝不拿空结果覆盖缓存。
                    return RefreshResult.Error(
                        failed.first().substringAfterLast(':'),
                        "本轮没有可用来源,已保留上次数据",
                    )
                }
                val merged = SyncMerge.merge(accepted, failed)
                val kept = if (failed.isNotEmpty() || skipped > 0) {
                    SyncMerge.retainMissing(merged, store.loadCache())
                } else {
                    merged
                }
                store.saveCache(kept)
                store.saveEtag(fetched.etag)
                store.saveLastRefresh(now)
                store.saveNextAllowed(0)
                RefreshResult.Loaded(kept)
            }
        }
    }

    fun logout() {
        store.clearGroup()
        // 退出即彻底清理本机凭据,换同步组后从匿名模式干净起步,
        // 不残留旧 token 让新组静默变成"已登录"。
        store.clearToken()
        store.clearGithubLogin()
        SyncScheduler.cancel(appContext)
    }

    fun clearCache() {
        store.clearCache()
    }

    fun disconnectGitHub() {
        store.clearToken()
        store.clearGithubLogin()
        SyncScheduler.scheduleAnonymous(appContext)
    }

    fun setToken(token: String) {
        store.saveToken(token)
        SyncScheduler.schedule(appContext, store.loadBackgroundMinutes())
    }

    fun setBackgroundMinutes(minutes: Int) {
        store.saveBackgroundMinutes(minutes)
        SyncScheduler.schedule(appContext, store.loadBackgroundMinutes())
    }

    private fun handleFailure(e: SyncException, now: Long): RefreshResult {
        if (e.code == SyncCodes.RATE_LIMITED) {
            val until = e.resetAt ?: (now + (e.retryAfterSec ?: 300L))
            store.saveNextAllowed(until)
            return RefreshResult.RateLimited(until)
        }
        return RefreshResult.Error(e.code, e.message)
    }

    private fun fetchWithToken(token: String?, gistId: String, etag: String?): FetchResult {
        val client = GistClient(token)
        return try {
            client.get(gistId, etag)
        } finally {
            client.close()
        }
    }

    private companion object {
        /**
         * 前台轮询、后台 SyncWorker 和小部件 Worker 各自 new 一个仓库实例，
         * 但共享同一份缓存与来源水位；并发时会互相覆盖，必须串行。
         */
        val gate = Mutex()
    }
}
