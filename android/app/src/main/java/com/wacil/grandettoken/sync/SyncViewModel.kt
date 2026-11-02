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
                    startPolling()
                    onSuccess()
                }
                is JoinResult.Error -> _state.update {
                    it.copy(loading = false, error = "${r.code}: ${r.message}")
                }
            }
        }
    }

    fun refresh(manual: Boolean = false) {
        if (_state.value.group == null) return
        viewModelScope.launch {
            _state.update { it.copy(loading = true) }
            when (val r = repo.refresh(force = manual)) {
                is RefreshResult.Loaded -> _state.update {
                    it.copy(
                        merged = r.merged,
                        loading = false,
                        error = null,
                        lastRefresh = System.currentTimeMillis() / 1000,
                    )
                }
                is RefreshResult.NotModified -> _state.update {
                    it.copy(
                        merged = r.merged ?: it.merged,
                        loading = false,
                        error = null,
                        lastRefresh = System.currentTimeMillis() / 1000,
                    )
                }
                is RefreshResult.RateLimited -> _state.update {
                    it.copy(loading = false, error = "请求太频繁，请稍后再试")
                }
                is RefreshResult.NoGroup -> _state.update {
                    it.copy(group = null, merged = null, loading = false)
                }
                is RefreshResult.Error -> _state.update {
                    it.copy(
                        loading = false,
                        error = "${r.code}: ${r.message}",
                        loggedIn = if (r.code == "SYNC_AUTH") false else it.loggedIn,
                    )
                }
            }
        }
    }

    fun setBackgroundMinutes(minutes: Int) {
        repo.setBackgroundMinutes(minutes)
        _state.update { it.copy(backgroundMinutes = minutes) }
    }

    fun logout() {
        stopPolling()
        repo.logout()
        _state.value = SyncUiState()
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
