package com.wacil.grandettoken.sync

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.wacil.grandettoken.sync.sync.GroupInfo
import com.wacil.grandettoken.sync.sync.GroupRepository
import com.wacil.grandettoken.sync.sync.JoinResult
import com.wacil.grandettoken.sync.sync.MergedSnapshot
import com.wacil.grandettoken.sync.sync.RefreshResult
import com.wacil.grandettoken.sync.sync.SyncScheduler
import com.wacil.grandettoken.sync.widget.WidgetSyncBridge
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class SyncUiState(
    val group: GroupInfo? = null,
    val merged: MergedSnapshot? = null,
    val loading: Boolean = false,
    val error: String? = null,
    val lastRefresh: Long = 0,
    val loggedIn: Boolean = false,
    val backgroundMinutes: Int = 15,
)

class SyncViewModel(app: Application) : AndroidViewModel(app) {
    private val repo = GroupRepository(app)
    private val _state = MutableStateFlow(initialState())
    val state = _state.asStateFlow()
    private var pollJob: Job? = null

    private fun initialState(): SyncUiState {
        val info = repo.info()
        return SyncUiState(
            group = info,
            merged = repo.cached(),
            lastRefresh = info?.lastRefresh ?: 0,
            loggedIn = info?.hasToken == true,
            backgroundMinutes = info?.backgroundMinutes ?: 15,
        )
    }

    fun join(code: String, onSuccess: () -> Unit) {
        if (code.isBlank()) {
            _state.update { it.copy(error = "请填写同步码") }
            return
        }
        viewModelScope.launch {
            _state.update { it.copy(loading = true, error = null) }
            when (val r = repo.join(code)) {
                is JoinResult.Success -> {
                    val info = repo.info()
                    _state.update {
                        it.copy(
                            group = info,
                            merged = r.merged,
                            loading = false,
                            error = null,
                            lastRefresh = System.currentTimeMillis() / 1000,
                            loggedIn = info?.hasToken == true,
                            backgroundMinutes = info?.backgroundMinutes ?: 15,
                        )
                    }
                    SyncScheduler.schedule(getApplication(), info?.backgroundMinutes ?: 15)
                    NotificationHelper.onJoin(getApplication(), r.merged)
                    WidgetSyncBridge.onGroupChanged(getApplication())
                    startPolling()
                    onSuccess()
                }
                is JoinResult.Error -> _state.update {
                    // 错误码对用户隐藏(SYNC_* 前缀),码本身在「设置 → 诊断」里仍有展示位
                    it.copy(loading = false, error = r.message)
                }
            }
        }
    }

    fun refresh(manual: Boolean = false) {
        if (_state.value.group == null) return
        viewModelScope.launch {
            _state.update { it.copy(loading = true) }
            when (val r = repo.refresh(force = manual)) {
                is RefreshResult.Loaded -> {
                    _state.update {
                        it.copy(
                            merged = r.merged,
                            loading = false,
                            error = null,
                            lastRefresh = System.currentTimeMillis() / 1000,
                            loggedIn = repo.hasToken(),
                        )
                    }
                    NotificationHelper.onRefreshResult(getApplication(), r, r.merged)
                    WidgetSyncBridge.onRefreshResult(getApplication(), r)
                }
                is RefreshResult.NotModified -> {
                    _state.update {
                        it.copy(
                            merged = r.merged ?: it.merged,
                            loading = false,
                            error = null,
                            lastRefresh = System.currentTimeMillis() / 1000,
                            loggedIn = repo.hasToken(),
                        )
                    }
                    NotificationHelper.onRefreshResult(
                        getApplication(),
                        r,
                        r.merged ?: _state.value.merged,
                    )
                    WidgetSyncBridge.onRefreshResult(getApplication(), r)
                }
                is RefreshResult.RateLimited -> {
                    _state.update {
                        it.copy(loading = false, error = "请求太频繁,稍后再试")
                    }
                    WidgetSyncBridge.onRefreshResult(getApplication(), r)
                }
                is RefreshResult.NoGroup -> {
                    _state.update {
                        it.copy(group = null, merged = null, loading = false, loggedIn = false)
                    }
                    NotificationHelper.onRefreshResult(getApplication(), r, null)
                    WidgetSyncBridge.onRefreshResult(getApplication(), r)
                }
                is RefreshResult.Error -> {
                    _state.update {
                        it.copy(
                            loading = false,
                            // 错误码对用户隐藏(同上),r.code 仍用于下面 SYNC_AUTH 的判定与诊断卡
                            error = r.message,
                            loggedIn = if (r.code == "SYNC_AUTH") false else repo.hasToken(),
                        )
                    }
                    NotificationHelper.onRefreshResult(getApplication(), r, _state.value.merged)
                    WidgetSyncBridge.onRefreshResult(getApplication(), r)
                }
            }
        }
    }

    fun setBackgroundMinutes(minutes: Int) {
        repo.setBackgroundMinutes(minutes)
        _state.update { it.copy(backgroundMinutes = minutes) }
    }

    fun loginClientId(): String? = repo.loginClientId()

    fun saveLoginClientId(clientId: String) = repo.saveLoginClientId(clientId)

    fun githubLogin(): String? = repo.githubLogin()

    fun completeLogin(token: String, login: String) {
        repo.setToken(token)
        repo.saveGithubLogin(login)
        _state.update { it.copy(loggedIn = true) }
        startPolling()
        refresh(true)
    }

    suspend fun verifyTokenRead(token: String) = repo.verifyTokenRead(token)

    fun logout() {
        stopPolling()
        repo.logout()
        _state.value = SyncUiState()
        WidgetSyncBridge.onGroupChanged(getApplication())
    }

    fun clearCache() {
        repo.clearCache()
        _state.update { it.copy(merged = null) }
        WidgetSyncBridge.onGroupChanged(getApplication())
    }

    fun disconnectGitHub() {
        repo.disconnectGitHub()
        _state.update { it.copy(loggedIn = false) }
    }

    fun startPolling() {
        if (pollJob?.isActive == true) return
        pollJob = viewModelScope.launch {
            while (true) {
                delay(if (_state.value.loggedIn) 60_000L else 300_000L)
                refresh()
            }
        }
    }

    fun stopPolling() {
        pollJob?.cancel()
        pollJob = null
    }
}
