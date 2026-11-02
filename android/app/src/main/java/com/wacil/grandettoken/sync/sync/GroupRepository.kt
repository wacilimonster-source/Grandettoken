package com.wacil.grandettoken.sync.sync

import android.content.Context
import kotlinx.coroutines.Dispatchers
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

    suspend fun join(codeText: String): JoinResult = withContext(Dispatchers.IO) {
        val code = try {
            SyncCode.decode(codeText)
        } catch (e: SyncException) {
            return@withContext JoinResult.Error(e.code, e.message)
        }
        val gistId = code.gistIdHex()
        val fetched = try {
            fetchWithToken(null, gistId, null)
        } catch (e: SyncException) {
            return@withContext JoinResult.Error(e.code, e.message)
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
            return@withContext JoinResult.Error(
                SyncCodes.EMPTY_GROUP,
                "同步组还没有可验证快照；请先在电脑端发布一次",
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
        JoinResult.Success(merged)
    }

    suspend fun refresh(force: Boolean = false): RefreshResult = withContext(Dispatchers.IO) {
        val group = store.loadGroup()
            ?: return@withContext RefreshResult.NoGroup
        val gistId = group.gistIdHex()
        val now = System.currentTimeMillis() / 1000
        if (!force) {
            val next = store.loadNextAllowed()
            if (next > now) return@withContext RefreshResult.RateLimited(next)
        }

        var usedToken = store.loadToken()
        var fetched: FetchResult
        try {
            fetched = fetchWithToken(usedToken, gistId, store.loadEtag())
        } catch (e: SyncException) {
            if (e.code == SyncCodes.AUTH && usedToken != null) {
                // 401：停止使用 token，保留缓存，降为匿名策略再试一次。
                store.clearToken()
                usedToken = null
                try {
                    fetched = fetchWithToken(null, gistId, store.loadEtag())
                } catch (e2: SyncException) {
                    return@withContext handleFailure(e2, now)
                }
            } else if (e.code == SyncCodes.KEY_INVALID) {
                return@withContext RefreshResult.Error(
                    e.code,
                    "本机密钥已失效，请退出同步组后重新扫码",
                )
            } else {
                return@withContext handleFailure(e, now)
            }
        }

        when (fetched) {
            is FetchResult.NotModified -> {
                store.saveLastRefresh(now)
                store.saveNextAllowed(0)
                RefreshResult.NotModified(store.loadCache())
            }
            is FetchResult.Loaded -> {
                val accepted = mutableListOf<SyncSnapshot>()
                val failed = mutableListOf<String>()
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
                        if (wm != null) {
                            if (snap.seq < wm.first) continue
                            if (snap.seq == wm.first) {
                                if (hash == wm.second) continue
                                failed.add("$source:${SyncCodes.CONFLICT}")
                                continue
                            }
                        }
                        store.saveWatermark(source, snap.seq, hash)
                        accepted.add(snap)
                    } catch (e: SyncException) {
                        failed.add("$source:${e.code}")
                    } catch (_: Exception) {
                        failed.add("$source:${SyncCodes.PAYLOAD_INVALID}")
                    }
                }
                val merged = SyncMerge.merge(accepted, failed)
                store.saveCache(merged)
                store.saveEtag(fetched.etag)
                store.saveLastRefresh(now)
                store.saveNextAllowed(0)
                RefreshResult.Loaded(merged)
            }
        }
    }

    fun logout() {
        store.clearGroup()
        SyncScheduler.cancel(appContext)
    }

    fun disconnectGitHub() {
        store.clearToken()
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
}
